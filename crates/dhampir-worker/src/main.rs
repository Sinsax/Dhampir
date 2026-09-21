//! `dhampir-render` —— 服务端探针 / 渲染入口。
//!
//! M0 阶段它只做三件事：
//!
//! 1. `--probe-only`：把跨运行时等价性探针的报告与摘要写出来（**不需要 GPU**，
//!    所以哪怕容器里没有 GPU，这一条验收也跑得了）
//! 2. 默认：离屏渲染探针三角形 → 读回 → 写 PNG 与 `adapter.json`
//! 3. 把"跑了什么、出了什么"写进 `run.json`，让 M0 的证据可以事后复核
//!
//! M1 会把它扩成 `--scene <name> --frames <range>`，加上 corpus 场景集与计时表。
//! 命令行参数刻意手写解析：M0 的依赖越少，"编译不过"的原因就越少。

use std::path::PathBuf;
use std::process::ExitCode;

use dhampir_core::render::SampleExpectation;
use dhampir_core::wgpu;
use dhampir_worker::offscreen::{self, PROBE_TARGET_FORMAT, SampleReading};
use dhampir_worker::{probe_digest, probe_report};

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
    /// 只跑纯逻辑探针，完全不碰 GPU。
    probe_only: bool,
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
  --out <dir>            产物目录（默认 records/m0）
  --backend <all|dx12|vulkan>
                         要跑的后端（默认 all）。all = 每个后端各跑一遍。
                         macOS 上 metal 由 core 的 NATIVE_BACKENDS 提供，此处不选。
  --probe-only           只跑纯逻辑探针，不碰 GPU
  -h, --help             显示本帮助";

    fn parse(mut argv: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut out = PathBuf::from("records/m0");
        let mut backends = BackendSelection::All;
        let mut probe_only = false;

        while let Some(arg) = argv.next() {
            match arg.as_str() {
                "--out" => {
                    out = PathBuf::from(argv.next().ok_or("--out 后面要跟一个目录")?);
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
                "--probe-only" => probe_only = true,
                "-h" | "--help" => {
                    println!("{}", Self::USAGE);
                    std::process::exit(0);
                }
                other => return Err(format!("不认识的参数：{other}")),
            }
        }

        Ok(Self {
            out,
            backends,
            probe_only,
        })
    }
}

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

fn run(args: &Args) -> Result<(), String> {
    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("建目录 {} 失败：{e}", args.out.display()))?;

    let mut summary = RunSummary::default();

    // ---- 1. 纯逻辑探针 ----------------------------------------------------
    // 先做这一步，是因为它不依赖 GPU。如果容器里没有 GPU，"两端一致"这条
    // 验收依然可以被验证——这正是把探针和渲染分开的价值。
    let report = probe_report();
    summary.probe_digest = format!("{:016x}", probe_digest());
    summary.probe_lines = report.lines().count();

    let report_path = args.out.join("selfcheck-native.txt");
    std::fs::write(&report_path, &report)
        .map_err(|e| format!("写 {} 失败：{e}", report_path.display()))?;
    println!(
        "纯逻辑探针：{} 行，摘要 {}",
        summary.probe_lines, summary.probe_digest
    );
    println!("  报告 → {}", report_path.display());

    if args.probe_only {
        write_summary(args, &summary)?;
        return Ok(());
    }

    // ---- 2. 逐后端离屏出图 ------------------------------------------------
    let selected = args.backends.expand(dhampir_core::gpu::NATIVE_BACKENDS);
    if selected.is_empty() {
        return Err(
            "本构建里没有任何可用的后端（dhampir-worker 的 Cargo.toml 应当开 vulkan/dx12/metal）"
                .to_string(),
        );
    }

    for backends in selected {
        let label = format!("{backends:?}");
        println!("\n▶ 后端 {label}");
        match offscreen::run_probe(backends) {
            Ok(run) => {
                let files = offscreen::write_run_artifacts(&args.out, &run)
                    .map_err(|e| format!("写产物失败：{e}"))?;
                let samples = offscreen::sample_points(&run.image);

                let adapter_name = run
                    .adapter_json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<未知>")
                    .to_string();

                println!("  adapter : {adapter_name}");
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
                    adapter_name,
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
