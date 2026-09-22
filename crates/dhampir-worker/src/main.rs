//! `dhampir-render` —— 服务端探针 / corpus 渲染入口。
//!
//! 三个模式，按给不给 `--scene` 分：
//!
//! 1. `--probe-only`：把跨运行时等价性探针的报告与摘要写出来（**不需要 GPU**，
//!    所以哪怕容器里没有 GPU，这一条验收也跑得了）
//! 2. 默认（M0 的路径）：离屏渲染探针三角形 → 读回 → 写 PNG 与 `adapter.json`，
//!    再把"跑了什么、出了什么"写进 `run.json`
//! 3. `--scene all --frames 0..16`（M1 的路径）：跑 corpus 场景集 → 出图 →
//!    逐点判定 → adapter / 1080p 计时 / 逐帧摘要各写各的文件；给了 `--compare-run`
//!    再与另一份记录逐帧比
//!
//! 每个后端写进**自己的目录**（`--backend all` 就是 `dx12/` 与 `vulkan/`）：
//! M0 时两个后端靠文件名区分（`probe-native-dx12.png`）还够用，到 M1 一条腿有
//! 80 张图，文件名不够了。
//!
//! 命令行参数刻意手写解析：依赖越少，"编译不过"的原因就越少。

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dhampir_core::render::SampleExpectation;
use dhampir_core::wgpu;
use dhampir_worker::baseline::{self, AdapterIdentity};
use dhampir_worker::offscreen::{self, PROBE_TARGET_FORMAT, SampleReading};
use dhampir_worker::{probe_digest, probe_report, scenes};

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误：{e}\n\n{}", Args::USAGE);
            return ExitCode::from(2);
        }
    };

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("✗ {e}");
            ExitCode::FAILURE
        }
    }
}

/// 命令行参数。
#[derive(Debug)]
struct Args {
    /// 产物目录。
    out: PathBuf,
    /// 要跑的后端。`All` 会依次为每个后端建一个独立的 `Instance` 各跑一遍。
    backends: BackendSelection,
    /// 这次要做什么。
    mode: RunMode,
}

/// 这次要做什么。
#[derive(Debug)]
enum RunMode {
    /// 只跑纯逻辑探针，完全不碰 GPU（`--probe-only`）。
    LogicProbeOnly,
    /// M0 的路径：探针三角形 + `run.json`。**默认走这条**，所以 M0 记录的复现命令
    /// （`--out records/m0`）在这个提交里仍然原样有效。
    Probe,
    /// M1 的路径：corpus 场景集。
    Scenes {
        selection: scenes::SceneSelection,
        /// 帧区间，半开。
        frames: (u32, u32),
        /// 要与哪一份记录逐帧比（该目录下每个后端一个 `<slug>/run.json`）。
        compare_run: Option<PathBuf>,
        /// 不跑 1080p 计时。复现性检查用它：计时不是那一次的目标，而它是最慢的一段。
        skip_timing: bool,
    },
}

/// 后端选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendSelection {
    /// 依次跑 DX12 与 Vulkan，各出一份产物。
    All,
    Dx12,
    Vulkan,
}

impl BackendSelection {
    /// 展开成实际的 `wgpu::Backends` 位标志列表。
    ///
    /// 每个后端**单独建 Instance**，而不是把所有位置或起来：合起来跑只会得到
    /// "其中某一个能跑"，而 M1 要的是"每个后端分别能不能跑"。
    fn expand(self, available: wgpu::Backends) -> Vec<wgpu::Backends> {
        let wanted = match self {
            Self::All => vec![wgpu::Backends::DX12, wgpu::Backends::VULKAN],
            Self::Dx12 => vec![wgpu::Backends::DX12],
            Self::Vulkan => vec![wgpu::Backends::VULKAN],
        };
        wanted
            .into_iter()
            .filter(|b| available.contains(*b))
            .collect()
    }
}

impl Args {
    const USAGE: &'static str = "\
用法：dhampir-render [选项]

选项：
  --out <dir>            产物目录（M0 探针路径的默认值是 records/m0；
                         corpus 路径**必须**显式给，免得往已归档的记录里写）
  --backend <all|dx12|vulkan>
                         要跑的后端（默认 all）。all = 每个后端各跑一遍，
                         每个后端写进自己的子目录（dx12/、vulkan/）。
                         macOS 上 metal 由 core 的 NATIVE_BACKENDS 提供，此处不选。
  --scene <all|名字>     跑 corpus（M1）。名字是 gradient / checker / srgb_linear /
                         alpha_stack / blur 之一。**给了它就走 corpus 路径。**
  --frames <区间>        帧区间，半开：`0..16`（默认，正好是每个场景的整周期）。
                         也可以只给一帧：`5`。不认 `..=`。
  --compare-run <dir>    与另一份记录逐帧比。<dir> 是**记录目录**（不是某个
                         run.json 的路径）：读该目录下每个后端的 <slug>/run.json，
                         结论写进 <out>/<slug>/compare.json
  --skip-timing          corpus 路径下不跑 1080p 计时（复现性检查用它）
  --probe-only           只跑纯逻辑探针，不碰 GPU
  -h, --help             显示本帮助";

    fn parse(mut argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut out = PathBuf::from("records/m0");
        let mut out_given = false;
        let mut backends = BackendSelection::All;
        let mut probe_only = false;
        let mut scene: Option<scenes::SceneSelection> = None;
        let mut frames: Option<(u32, u32)> = None;
        let mut compare_run: Option<PathBuf> = None;
        let mut skip_timing = false;

        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "--out" => {
                    out = PathBuf::from(argv.next().ok_or("--out 后面要跟一个目录")?);
                    out_given = true;
                }
                "--backend" => {
                    let value = argv.next().ok_or("--backend 后面要跟 all/dx12/vulkan")?;
                    backends = match value.as_str() {
                        "all" => BackendSelection::All,
                        "dx12" => BackendSelection::Dx12,
                        "vulkan" => BackendSelection::Vulkan,
                        other => return Err(format!("不认识的 --backend 取值：{other}")),
                    };
                }
                "--scene" => {
                    let value = argv.next().ok_or("--scene 后面要跟 all 或场景名")?;
                    scene = Some(scenes::parse_selection(&value)?);
                }
                "--frames" => {
                    let value = argv.next().ok_or("--frames 后面要跟区间（例如 0..16）")?;
                    frames = Some(scenes::parse_frames(&value)?);
                }
                "--compare-run" => {
                    compare_run =
                        Some(PathBuf::from(argv.next().ok_or("--compare-run 后面要跟一个目录")?));
                }
                "--skip-timing" => skip_timing = true,
                "--probe-only" => probe_only = true,
                "-h" | "--help" => {
                    println!("{}", Self::USAGE);
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数：{other}")),
            }
        }

        // `--frames` / `--compare-run` / `--skip-timing` 只在 corpus 路径下有意义。
        // 不报错的话，`--frames 0..16` 会被**静默丢掉**，然后人看着一份"跑到哪儿去了"
        // 的记录发呆——参数被忽略比参数被拒绝难查得多。
        if scene.is_none() {
            for (given, name) in [
                (frames.is_some(), "--frames"),
                (compare_run.is_some(), "--compare-run"),
                (skip_timing, "--skip-timing"),
            ] {
                if given {
                    return Err(format!("{name} 只在 corpus 路径下有意义，请同时给 --scene"));
                }
            }
        }

        let mode = match (scene, probe_only) {
            (Some(_), true) => {
                return Err("--scene 与 --probe-only 是两条不同的路径，不能一起给".to_string())
            }
            (Some(selection), false) => RunMode::Scenes {
                selection,
                frames: frames.unwrap_or((0, scenes::FULL_PERIOD_FRAMES)),
                compare_run,
                skip_timing,
            },
            (None, true) => RunMode::LogicProbeOnly,
            (None, false) => RunMode::Probe,
        };

        // corpus 路径必须显式给 `--out`：默认值 `records/m0` 是 M0 的**已归档**记录，
        // 往那里写 corpus 的产物会把"那份字节可以被重跑复现"这条性质毁掉。
        //
        // 这一条**只在这里查**。放在 `run_corpus` 里也行，但那已经是 `create_dir_all`
        // 之后了——"拒绝"会发生在"已经建了目录"之后，而且真正的原因（命令行没给全）
        // 与它出现的位置（渲染函数）隔了半屏。同一个问题只在同一个地方回答。
        if matches!(mode, RunMode::Scenes { .. }) && !out_given {
            return Err(
                "corpus 路径必须显式给 --out：默认值 records/m0 是 M0 的已归档记录".to_string(),
            );
        }

        Ok(Self { out, backends, mode })
    }
}

fn run(args: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("建目录 {} 失败：{e}", args.out.display()))?;

    match &args.mode {
        RunMode::LogicProbeOnly => {
            let (digest, lines) = write_logic_probe_report(&args.out)?;
            println!("纯逻辑探针：{lines} 行，摘要 {digest}");
            // 只跑探针时不碰 GPU，`run.json` 里那一栏的 `sample_check_passed`
            // 因此是 `null`（**没验**），不是"通过"。
            let summary = RunSummary {
                probe_digest: digest,
                probe_lines: lines,
                backends: Vec::new(),
            };
            write_summary(args, &summary)
        }
        RunMode::Probe => run_probe_mode(args),
        RunMode::Scenes {
            selection,
            frames,
            compare_run,
            skip_timing,
        } => run_corpus(args, *selection, *frames, compare_run.as_deref(), *skip_timing),
    }
}

/// 纯逻辑探针：报告 + 摘要落盘。
///
/// 两条路径都要它，因为它是**"这份记录是哪个二进制写的"的指纹**：摘要与
/// `adapter.json` 里那一栏对上，才说明记录出自同一份代码。
fn write_logic_probe_report(out: &Path) -> Result<(String, usize), String> {
    let report = probe_report();
    let digest = format!("{:016x}", probe_digest());
    let lines = report.lines().count();

    let path = out.join("selfcheck-native.txt");
    std::fs::write(&path, &report)
        .map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
    println!("  报告 → {}", path.display());
    Ok((digest, lines))
}

// ---------------------------------------------------------------------------
// M0 路径：探针三角形
// ---------------------------------------------------------------------------

/// 一次运行的结果汇总。写进 `run.json`，是 M0 里程碑记录的主证据。
#[derive(Debug, Default)]
struct RunSummary {
    probe_digest: String,
    probe_lines: usize,
    backends: Vec<BackendOutcome>,
}

/// 单个后端的运行结果。
#[derive(Debug)]
struct BackendOutcome {
    backend: String,
    adapter_name: String,
    deterministic_in_process: bool,
    files: Vec<String>,
    samples: Vec<SampleReading>,
    /// 采样判定的结论。`None` = 通过（或这一轮没判——比如进程内就不一致，
    /// 那时已经是一个**发现**，不该再用颜色断言去盖住它）。
    verdict: Option<String>,
    error: Option<String>,
}

fn run_probe_mode(args: &Args) -> Result<(), String> {
    let mut summary = RunSummary::default();

    // ---- 1. 纯逻辑探针 ----------------------------------------------------
    // 先做这一步，是因为它不依赖 GPU。如果容器里没有 GPU，"两端一致"这条
    // 验收依然可以被验证——这正是把探针和渲染分开的价值。
    let (digest, lines) = write_logic_probe_report(&args.out)?;
    println!("纯逻辑探针：{lines} 行，摘要 {digest}");
    summary.probe_digest = digest;
    summary.probe_lines = lines;

    // ---- 2. 逐后端离屏出图 ------------------------------------------------
    let selected = args.backends.expand(dhampir_core::gpu::NATIVE_BACKENDS);
    if selected.is_empty() {
        return Err(
            "本构建里没有任何可用的后端（dhampir-worker 的 Cargo.toml 应当开 vulkan/dx12/metal）"
                .to_string(),
        );
    }

    for backends in selected {
        let label = baseline::backend_label(backends);
        println!("\n▶ 后端 {label}");
        match offscreen::run_probe(backends) {
            Ok(run) => {
                let files = offscreen::write_run_artifacts(&args.out, &run)
                    .map_err(|e| format!("写产物失败：{e}"))?;
                let samples = offscreen::sample_points(&run.image);

                // adapter 名从 `AdapterIdentity` 取（M0 那版从 `run.adapter_json` 里
                // 掏字符串——同一件事两个来源，那一份 JSON 的形状变了这里就会静默地
                // 变成 `<未知>`）。取不到时**不编占位符**：`None` 就记 `null`。
                let adapter_name = run.adapter.name().map(str::to_string);

                println!(
                    "  adapter : {}",
                    adapter_name.as_deref().unwrap_or("（adapter 没报名字）")
                );
                println!(
                    "  进程内重复渲染逐字节一致：{}",
                    run.deterministic_in_process
                );
                for f in &files {
                    println!("  → {}", f.display());
                }
                for reading in &samples {
                    println!(
                        "  采样 {:<20} ({:>3},{:>3}) = {:<18} 期望：{}",
                        reading.sample.name,
                        reading.sample.x,
                        reading.sample.y,
                        reading
                            .rgba
                            .map_or_else(|| "越界".to_string(), |p| format!("{p:?}")),
                        describe_expect(reading.sample.expect),
                    );
                }

                // 判定失败**不提前返回**。M0 第一版就是在这里 `?` 出去的，后果是：
                // run.json 没被写、后面的后端一个都没跑、这一轮的采样值只剩终端回滚。
                // 一次失败要么留下证据，要么就是白跑一遍。
                let verdict = if run.deterministic_in_process {
                    offscreen::check_samples(&samples, (run.image.width, run.image.height)).err()
                } else {
                    println!("  ⚠ 进程内重复渲染不一致——这是一个**发现**，已如实记入 run.json");
                    None
                };
                if let Some(reason) = &verdict {
                    println!("  ✗ 采样判定未通过：{reason}");
                }

                summary.backends.push(BackendOutcome {
                    backend: label,
                    adapter_name: adapter_name.unwrap_or_default(),
                    deterministic_in_process: run.deterministic_in_process,
                    files: files.iter().map(|p| record_path(p)).collect(),
                    samples,
                    verdict,
                    error: None,
                });
            }
            Err(e) => {
                println!("  ✗ 失败：{e}");
                summary.backends.push(BackendOutcome {
                    backend: label,
                    adapter_name: String::new(),
                    deterministic_in_process: false,
                    files: Vec::new(),
                    samples: Vec::new(),
                    verdict: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    write_summary(args, &summary)?;

    if summary.backends.iter().all(|b| b.error.is_some()) {
        return Err("所有后端都失败了——见上面的错误与 run.json".into());
    }

    // 判定失败要落到**退出码**上，而且要在记录写完之后才返回：
    // 一个"只写记录、不改退出码"的守卫，在 CI 里等于没有；反过来，
    // 一个"直接退出、不写记录"的守卫，在事后复核时等于没有。两者都要。
    let failed: Vec<&str> = summary
        .backends
        .iter()
        .filter(|b| b.verdict.is_some())
        .map(|b| b.backend.as_str())
        .collect();
    if !failed.is_empty() {
        return Err(format!(
            "采样判定未通过的后端：{}——细节在 run.json",
            failed.join("、")
        ));
    }
    Ok(())
}

/// 写进记录的路径统一用 `/` 分隔。
///
/// `records/` 里的文件会被跨平台比较（CI 上 Windows 与 Linux 都会跑），
/// 而 `Path::display()` 在 Windows 上给的是 `records/m0\probe.png`——同一份产物
/// 在两个平台上产生不同的字节，diff 里就全是噪音。这里统一，比"到时候再说"便宜。
fn record_path(path: &std::path::Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// 期望的简短说明，给终端看。
///
/// 采样点**声明什么、就断言什么**：判定在
/// [`offscreen::judge_sample`]／[`offscreen::check_samples`]，本函数只负责把它说成人话。
/// 这里不再有一份自己的判断口径——两份口径正是"M0 第一轮误报"的根源：
/// 记录写一套坐标、断言查另一套，两边各错各的，互相掩盖。
fn describe_expect(expect: SampleExpectation) -> &'static str {
    match expect {
        SampleExpectation::Background => "清屏色（中性灰）",
        SampleExpectation::Dominant(0) => "红通道占优",
        SampleExpectation::Dominant(1) => "绿通道占优",
        SampleExpectation::Dominant(2) => "蓝通道占优",
        SampleExpectation::Dominant(_) => "未知通道占优",
    }
}

/// 同一个期望的结构化写法，给 `run.json` 看。
///
/// 记录里带上期望，复核的人就不必回头读源码：**每个采样点当时在验什么**是
/// 记录的一部分。只有坐标和颜色值的话，"这一点该是红的"这条信息只存在于
/// 那一刻的二进制里，事后无法复核。
fn expect_json(expect: SampleExpectation) -> serde_json::Value {
    match expect {
        SampleExpectation::Background => serde_json::json!({ "kind": "background" }),
        SampleExpectation::Dominant(channel) => serde_json::json!({
            "kind": "dominant",
            "channel": channel,
            "channel_name": match channel {
                0 => "r",
                1 => "g",
                2 => "b",
                _ => "?",
            },
        }),
    }
}

fn write_summary(args: &Args, summary: &RunSummary) -> Result<(), String> {
    let mut backends = Vec::new();
    for b in &summary.backends {
        let samples: Vec<serde_json::Value> = b
            .samples
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.sample.name,
                    "x": r.sample.x,
                    "y": r.sample.y,
                    "expect": expect_json(r.sample.expect),
                    "rgba": r.rgba.map(|p| p.to_vec()),
                })
            })
            .collect();
        backends.push(serde_json::json!({
            "requested": b.backend,
            "adapter_name": b.adapter_name,
            "deterministic_in_process": b.deterministic_in_process,
            "files": b.files,
            "samples": samples,
            "sample_verdict": b.verdict,
            "error": b.error,
        }));
    }

    // 三个条件一起看：至少跑了一个后端、每轮都跑成功、每轮采样判定都过。
    // `null` = 这次没碰 GPU（`--probe-only`），**不是"通过"**——把"没验"
    // 记成"验过了"是最难发现的一种谎。
    let sample_check_passed: Option<bool> = if summary.backends.is_empty() {
        None
    } else {
        Some(
            summary
                .backends
                .iter()
                .all(|b| b.error.is_none() && b.verdict.is_none()),
        )
    };

    let json = serde_json::json!({
        "schema": 1,
        "milestone": "M0",
        "probe": {
            "digest": summary.probe_digest,
            "lines": summary.probe_lines,
            "format_version": dhampir_core::timeline::PROBE_FORMAT_VERSION,
        },
        "sample_check_passed": sample_check_passed,
        "backends": backends,
        "pinned_expectations": {
            "probe_target_size": format!("{}x{}", offscreen::PROBE_TARGET_SIZE.0, offscreen::PROBE_TARGET_SIZE.1),
            "probe_target_format": format!("{PROBE_TARGET_FORMAT:?}"),
        },
    });

    let path = args.out.join("run.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("写 {} 失败：{e}", path.display()))?;
    println!("\n运行汇总 → {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// M1 路径：corpus 场景集
// ---------------------------------------------------------------------------

/// 一条腿跑完之后**给退出码用的**结论。
///
/// 为什么两个字段分开：`corpus 不干净`（画出来的东西不对/判不了）与
/// `跨进程不一致`（两次运行画出来的不一样）是两个归因方向完全不同的发现，
/// 合成一个"失败"就没人知道该去看哪一边。
#[derive(Debug)]
struct LegOutcome {
    label: String,
    slug: String,
    /// corpus 这一半的问题：起不来 / 判不干净 / 行对齐探针不通过 / 写文件失败。
    problem: Option<String>,
    /// 跨进程比对那一半：**不一致**时才有内容。
    comparison: Option<String>,
}

fn run_corpus(
    args: &Args,
    selection: scenes::SceneSelection,
    frames: (u32, u32),
    compare_run: Option<&Path>,
    skip_timing: bool,
) -> Result<(), String> {
    // `--out` 是不是显式给的，在 `Args::parse` 里已经查过（它是**命令行**的性质，
    // 不是渲染的性质）。这里不再查第二遍。
    let specs = selection.specs();
    let (digest, lines) = write_logic_probe_report(&args.out)?;
    println!("纯逻辑探针：{lines} 行，摘要 {digest}");
    println!(
        "corpus：{} 个场景，帧区间 {}..{}（每场景 {} 帧，共 {} 帧）",
        specs.len(),
        frames.0,
        frames.1,
        frames.1 - frames.0,
        specs.len() as u32 * (frames.1 - frames.0)
    );

    let selected = args.backends.expand(dhampir_core::gpu::NATIVE_BACKENDS);
    if selected.is_empty() {
        return Err(
            "本构建里没有任何可用的后端（dhampir-worker 的 Cargo.toml 应当开 vulkan/dx12/metal）"
                .to_string(),
        );
    }

    let mut legs: Vec<LegOutcome> = Vec::new();

    for backends in selected {
        let label = baseline::backend_label(backends);
        // 目录名从后端名派生（`DX12` → `dx12`）：两个后端各自一份产物，
        // 否则 `--backend all` 会让它们写出同一批 `frames/*.png`。
        let slug = baseline::backend_slug(backends);
        let dir = args.out.join(&slug);
        println!("\n▶ 后端 {label}（{slug}/）");

        let mut outcome = LegOutcome {
            label: label.clone(),
            slug: slug.clone(),
            problem: None,
            comparison: None,
        };

        if let Err(e) = std::fs::create_dir_all(&dir) {
            outcome.problem = Some(format!("建目录 {} 失败：{e}", dir.display()));
            legs.push(outcome);
            continue;
        }

        // 时间戳在这一步取：`adapter.json` 与 `timing.json` 必须带**同一个**，
        // 否则没人能证明它们是同一次运行的两份记录。
        let epoch_millis = baseline::unix_epoch_millis();

        let (ctx, init) = match baseline::open_leg(backends) {
            Ok(opened) => opened,
            Err(e) => {
                println!("  ✗ 起不来：{e}");
                outcome.problem = Some(format!("起不来：{e}"));
                legs.push(outcome);
                continue;
            }
        };
        let adapter = AdapterIdentity::from_context(&ctx, backends);
        let adapter_name = adapter.name().map(str::to_string);
        println!(
            "  adapter : {}",
            adapter_name.as_deref().unwrap_or("（adapter 没报名字）")
        );
        println!("  init    : {:.3} ms", baseline::millis(init));

        // ---- 1. adapter.json：**先写**。------------------------------------
        // 计时是整条腿里最长的一段，它中途挂掉时，"在什么环境里挂的"必须还查得到。
        if let Err(e) = write_json(
            &dir.join("adapter.json"),
            &baseline::adapter_json(&adapter, epoch_millis),
        ) {
            outcome.problem = Some(e);
            legs.push(outcome);
            continue;
        }
        println!("  → {}/{slug}/adapter.json", record_path(&args.out));

        // ---- 2. corpus：出图、读回、判定 -----------------------------------
        let run = match scenes::run_scenes(&ctx, &specs, frames) {
            Ok(run) => run,
            Err(e) => {
                println!("  ✗ corpus 渲染失败：{e}");
                outcome.problem = Some(format!("corpus 渲染失败：{e}"));
                legs.push(outcome);
                continue;
            }
        };

        // 先落 PNG（主证据），再落清单：进程中途被杀时，至少图还在。
        if let Err(e) = scenes::write_frames(&dir, &run) {
            outcome.problem = Some(format!("写 PNG 失败：{e}"));
        }

        let counts = run.counts();
        println!(
            "  corpus  : {} 帧，采样点 {}，失败 {}，越界 {}，未判定 {}",
            counts.frames, counts.points, counts.failed, counts.out_of_range, counts.unjudged
        );
        for spec in &specs {
            let n = run.frames.iter().filter(|f| f.spec.name == spec.name).count();
            println!("            {:<12} {n} 帧", spec.name);
        }
        let mismatches = run.in_process_mismatches();
        if !mismatches.is_empty() {
            // 不一致是**发现**，不是"这次不算"：照记，并让退出码红。
            println!(
                "  ⚠ 同帧两次渲染不一致：{}（逐帧摘要仍在 run.json 里）",
                mismatches.join("、")
            );
        }
        if !counts.clean() && outcome.problem.is_none() {
            outcome.problem = Some(format!(
                "判定不干净：失败 {}、越界 {}、未判定 {}",
                counts.failed, counts.out_of_range, counts.unjudged
            ));
        }

        let readings = scenes::report_text(&run);
        if let Err(e) = write_text(&dir.join("readings.txt"), &readings) {
            outcome.problem.get_or_insert(e);
        }

        let run_json = scenes::leg_json(&run, &specs, &label, adapter_name.as_deref(), frames);
        if let Err(e) = write_json(&dir.join("run.json"), &run_json) {
            outcome.problem.get_or_insert(e);
        }
        println!(
            "  → {}/{slug}/frames/*.png（{} 个）、readings.txt、run.json",
            record_path(&args.out),
            run.frames.len()
        );

        // ---- 3. timing.json：1080p 计时 + 行对齐探针 ------------------------
        if skip_timing {
            println!("  计时    ：--skip-timing，跳过（这份产物里就没有 timing.json）");
        } else {
            match baseline::measure(&ctx, backends, &specs, init, epoch_millis) {
                Ok(baseline) => {
                    match baseline.budget_verdict() {
                        Some(true) => println!(
                            "  计时    ：最慢一趟往返 {:.3} ms ≤ 预算 {} ms → 通过（含读回；纯 CPU 提交最慢 {:.3} ms）",
                            baseline.worst_readback_ms().unwrap_or_default(),
                            baseline::FRAME_BUDGET_MS,
                            baseline.worst_frame_ms().unwrap_or_default()
                        ),
                        Some(false) => println!(
                            "  计时    ：最慢一趟往返 {:.3} ms > 预算 {} ms → **超预算**（记录里有数；先看机器在干什么，别改口径）",
                            baseline.worst_readback_ms().unwrap_or_default(),
                            baseline::FRAME_BUDGET_MS
                        ),
                        None => println!(
                            "  计时    ：{} 构建，不对预算下结论（数记在 timing.json 里）",
                            baseline::build_profile()
                        ),
                    }
                    println!(
                        "  行对齐  ：{}×{} 填到 {} 字节一行，逐像素比 {} 个，最差距离 {} → {}",
                        baseline.alignment.size.0,
                        baseline.alignment.size.1,
                        baseline.alignment.padded_bytes_per_row,
                        baseline.alignment.pixels_compared,
                        baseline.alignment.worst_distance,
                        if baseline.alignment.ok { "通过" } else { "**未通过**" }
                    );
                    if !baseline.alignment.ok && outcome.problem.is_none() {
                        outcome.problem = Some(format!(
                            "行对齐探针未通过：{}",
                            baseline
                                .alignment
                                .detail
                                .clone()
                                .unwrap_or_else(|| "（detail 没写原因）".to_string())
                        ));
                    }
                    if let Err(e) = write_json(
                        &dir.join("timing.json"),
                        &baseline::timing_json(&baseline),
                    ) {
                        outcome.problem.get_or_insert(e);
                    } else {
                        println!("  → {}/{slug}/timing.json", record_path(&args.out));
                    }
                }
                Err(e) => {
                    println!("  ✗ 计时失败：{e}");
                    outcome.problem.get_or_insert(format!("计时失败：{e}"));
                }
            }
        }

        // ---- 4. 跨进程比对 -------------------------------------------------
        if let Some(other_root) = compare_run {
            let other_path = other_root.join(&slug).join("run.json");
            let (other, other_note) = read_json(&other_path);
            let mut comparison = scenes::compare_runs(&other, &run_json);
            if let Some(note) = other_note {
                // "没读到对方那份记录"与"对方没跑这个后端"是两件事：前者是路径写错了，
                // 后者是一条真发现。插一个字段进来，免得后者把前者盖住。
                if let Some(map) = comparison.as_object_mut() {
                    map.insert(
                        "other_run_error".into(),
                        serde_json::Value::String(note.clone()),
                    );
                }
                println!("  ⚠ {note}");
            }

            let identical = comparison["identical"].as_bool().unwrap_or(false);
            println!(
                "  比对    ：比过 {} 个后端、没比 {} 个、identical={}",
                comparison["backends_compared"],
                comparison["backends_not_compared"],
                identical
            );
            for backend in comparison["backends"].as_array().into_iter().flatten() {
                for (key, label) in [
                    ("pixel_mismatches", "像素不同"),
                    ("png_mismatches", "PNG 不同"),
                    ("missing_in_other", "对方缺帧"),
                    ("missing_in_current", "本次缺帧"),
                ] {
                    let list = backend[key].as_array().cloned().unwrap_or_default();
                    if !list.is_empty() {
                        println!("            {}：{}（{key}）", label, serde_json::Value::Array(list));
                    }
                }
            }

            if let Err(e) = write_json(&dir.join("compare.json"), &comparison) {
                outcome.problem.get_or_insert(e);
            } else {
                println!("  → {}/{slug}/compare.json", record_path(&args.out));
            }

            if !identical {
                outcome.comparison = Some(format!(
                    "与被比的那份记录**不一致**（比过 {} 个后端、没比 {} 个）",
                    comparison["backends_compared"], comparison["backends_not_compared"]
                ));
            }
        }

        legs.push(outcome);
    }

    // ---- 汇总 -------------------------------------------------------------
    println!("\n—— 汇总 ——");
    for leg in &legs {
        println!(
            "  {:<8} {}/　{}",
            leg.label,
            leg.slug,
            match (&leg.problem, &leg.comparison) {
                (None, None) => "干净".to_string(),
                (problem, comparison) => [problem.clone(), comparison.clone()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("；"),
            }
        );
    }

    // 记录都写完了才动退出码：一个"直接退出、不写记录"的守卫在事后复核时等于没有，
    // 一个"只写记录、不改退出码"的守卫在 CI 里等于没有。两者都要。
    let problems: Vec<String> = legs
        .iter()
        .filter_map(|l| l.problem.as_ref().map(|p| format!("{}（{}）", l.label, p)))
        .collect();
    if !problems.is_empty() {
        return Err(format!("corpus 没跑干净：{}", problems.join("；")));
    }

    let mismatches: Vec<String> = legs
        .iter()
        .filter_map(|l| l.comparison.as_ref().map(|c| format!("{}：{c}", l.label)))
        .collect();
    if !mismatches.is_empty() {
        return Err(format!(
            "跨进程比对：{}——这是**发现**：先看是像素不同还是 PNG 不同，两者归因方向不一样",
            mismatches.join("；")
        ));
    }

    Ok(())
}

/// 写一份给人读的 JSON：两空格缩进 + 结尾换行。
///
/// 统一走这一个函数，是因为 `json!` 出来的东西默认不换行——一行几千字节的记录，
/// `git diff` 只会说"这一整行变了"，等于没有 diff。
fn write_json(path: &Path, json: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(json).map_err(|e| e.to_string())?;
    text.push('\n');
    std::fs::write(path, text).map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

/// 写一份文本产物（`readings.txt`）。
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("写 {} 失败：{e}", path.display()))
}

/// 读一份 JSON。读不到时返回**空对象**与一句说明，而不是直接失败：
/// 调用方（比对）拿空对象当"对方什么都没跑"，于是"没读到"会如实变成
/// `identical: false`，而不是让整条腿白跑一遍。
fn read_json(path: &Path) -> (serde_json::Value, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => (value, None),
            Err(e) => (
                serde_json::Value::Object(serde_json::Map::new()),
                Some(format!("{} 读不成 JSON：{e}", record_path(path))),
            ),
        },
        Err(e) => (
            serde_json::Value::Object(serde_json::Map::new()),
            Some(format!("{} 读不到：{e}", record_path(path))),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用一组参数调一次解析。测试里不碰 `std::env`——那东西是全局的，
    /// 两个测试并行改它，失败会指向错误的地方。
    fn parse(argv: &[&str]) -> Result<Args, String> {
        Args::parse(argv.iter().map(|s| s.to_string()))
    }

    /// 不认识的参数不能被当成"没给"。
    ///
    /// 这条是这一整块测试的理由：手写解析最经典的坏法就是把拼错的选项静默丢掉，
    /// 然后人对着一条"按默认参数跑出来的"记录研究它为什么不符合预期。
    #[test]
    fn an_unknown_argument_is_rejected_instead_of_ignored() {
        let cases: [&[&str]; 3] = [
            &["--nope"],
            &["--out", "target/x", "--frmaes", "0..4"],
            &["--probe-only", "extra"],
        ];
        for argv in cases {
            let err = parse(argv).expect_err(argv[0]);
            assert!(err.contains("不认识的参数"), "{argv:?} → {err}");
        }
    }

    #[test]
    fn an_unknown_backend_is_rejected() {
        let err = parse(&["--backend", "metal"]).expect_err("metal 不在这个枚举里");
        assert!(err.contains("不认识的 --backend 取值：metal"), "{err}");
    }

    /// 一个必须跟在值后面的选项，缺了值要报错，不能拿下一个选项当值。
    #[test]
    fn an_option_without_its_value_is_rejected() {
        for (argv, name) in [
            (["--out"], "--out"),
            (["--backend"], "--backend"),
            (["--scene"], "--scene"),
            (["--frames"], "--frames"),
            (["--compare-run"], "--compare-run"),
        ] {
            let err = parse(&argv).expect_err(name);
            assert!(err.contains(name) && err.contains("要跟"), "{argv:?} → {err}");
        }
    }

    /// 不给参数 = M0 那条路径。M0 的复现命令（`--out records/m0`）依赖这一点。
    #[test]
    fn no_arguments_still_means_the_m0_probe_path() {
        let args = parse(&[]).expect("空参数是合法的");
        assert!(matches!(args.mode, RunMode::Probe));
        assert_eq!(args.backends, BackendSelection::All);
        assert_eq!(args.out, PathBuf::from("records/m0"));
    }

    /// `--probe-only` 不碰 GPU，所以它连 `--out` 都不要求给——能解析出来就是这条断言
    /// 的全部内容。（corpus 路径上同样的输入会**被拒绝**，见下面那条。）
    #[test]
    fn probe_only_needs_no_gpu_and_no_explicit_out() {
        let args = parse(&["--probe-only"]).expect("只跑探针是合法的");
        assert!(matches!(args.mode, RunMode::LogicProbeOnly));
    }

    /// `--scene` 是唯一的路径开关：给了它才走 corpus。
    #[test]
    fn a_scene_selects_the_corpus_path() {
        let args = parse(&["--scene", "all", "--out", "target/x"]).expect("合法");
        let RunMode::Scenes {
            selection,
            frames,
            compare_run,
            skip_timing,
        } = args.mode
        else {
            panic!("给了 --scene 却不在 corpus 路径上");
        };
        assert_eq!(selection, scenes::SceneSelection::All);
        assert_eq!(
            frames,
            (0, scenes::FULL_PERIOD_FRAMES),
            "不给 --frames 时默认走满一整个周期"
        );
        assert!(compare_run.is_none());
        assert!(!skip_timing);
        assert_eq!(args.out, PathBuf::from("target/x"));
    }

    #[test]
    fn a_single_frame_is_a_one_frame_range() {
        let args = parse(&["--scene", "checker", "--frames", "5", "--out", "target/x"]).expect("合法");
        let RunMode::Scenes { frames, .. } = args.mode else {
            panic!("不在 corpus 路径上");
        };
        assert_eq!(frames, (5, 6));
    }

    /// corpus 的 `--out` **必须显式给**：默认值 `records/m0` 是已归档的记录，
    /// 往里写就等于把"M0 那份字节可以被重跑复现"这条性质毁掉。
    #[test]
    fn corpus_refuses_the_archived_m0_default_out() {
        let err = parse(&["--scene", "all"]).expect_err("没给 --out");
        assert!(err.contains("必须显式给 --out"), "{err}");
    }

    /// 只在 corpus 下有意义的那三个选项：没有 `--scene` 时**报错**，不是静默丢掉。
    #[test]
    fn corpus_only_flags_without_a_scene_are_rejected() {
        let cases: [(&[&str], &str); 3] = [
            (&["--frames", "0..4"], "--frames"),
            (&["--compare-run", "records/m1"], "--compare-run"),
            (&["--skip-timing"], "--skip-timing"),
        ];
        for (argv, name) in cases {
            let err = parse(argv).expect_err(name);
            assert!(
                err.contains(name) && err.contains("--scene"),
                "{argv:?} → {err}"
            );
        }
    }

    /// 两条路径的开关一起给是**用错**，不是"取交集"。
    #[test]
    fn scene_and_probe_only_are_two_different_paths() {
        let err = parse(&["--scene", "blur", "--probe-only"]).expect_err("互斥");
        assert!(err.contains("不能一起给"), "{err}");
    }

    /// 后端选择按可用位过滤。**不做替换**：要 DX12 而这份构建只有 Vulkan 时，
    /// 结果必须是"什么都没跑"，绝不是"那就跑 Vulkan 吧"——记录里会出现一个
    /// 谁都没请求过的后端。
    #[test]
    fn expand_filters_by_what_this_build_has() {
        assert_eq!(
            BackendSelection::All.expand(wgpu::Backends::DX12 | wgpu::Backends::VULKAN),
            vec![wgpu::Backends::DX12, wgpu::Backends::VULKAN],
            "All 的展开顺序固定：先 DX12 后 Vulkan"
        );
        assert_eq!(
            BackendSelection::All.expand(wgpu::Backends::VULKAN),
            vec![wgpu::Backends::VULKAN]
        );
        assert!(BackendSelection::Dx12.expand(wgpu::Backends::VULKAN).is_empty());
        assert!(BackendSelection::All.expand(wgpu::Backends::empty()).is_empty());
    }
}

