//! 底座的命令行：dhampir probe | info | gop | frame | render。
//!
//! # 它为什么属于底座
//!
//! 在此之前，后端的能力只活在 examples/ 里 —— 例子是给人看的，没有任何稳定性承诺，
//! 也不会被任何守卫盯着。而「远端模式」与「本机模式」的差别**只是后端在哪**，
//! 所以后端真正需要的那个东西，就是一个能被任何宿主张起来的**确定性入口**：
//!
//! > 本机后端（scripts/dhampir-local.mjs）就是这么用它的：
//! > HTTP 层在 Node，渲染在 Rust，两边都不重复实现对方的活。
//!
//! # 输出契约
//!
//! * 需要读结构的子命令（probe / info / gop / frame）在 stdout 上打**一个 JSON 文档**；
//! * render 在 stdout 上打 **NDJSON**（一行一个对象）：先 start，再若干 progress，末行 done。
//!   带进度是因为出片是分钟级的活，调用方要能显示进度而不能干等。
//! * 诊断、警告、被忽略的音轨一律走 **stderr** —— stdout 只给机器读。
//!
//! # 退出码（契约的一部分，被 scripts/cli-contract.mjs 钉住）
//!
//! | 码 | 含义 |
//! |---|---|
//! | 0 | 成功 |
//! | 2 | 用法错，或工程校验有 error（**用户能改的**） |
//! | 1 | 运行期失败（GPU、ffmpeg、写盘、渲染报告判失败） |
//!
//! **不认识的参数一律退出 2**，不静默丢掉 —— 静默丢参数会让 --width 打错字变成
//! 「用默认尺寸跑完了」，而人以为已经生效。

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use dhampir_core::compose;
use dhampir_core::effects::REGISTRY;
use dhampir_core::overlay::{OverlaySpan, SubtitleTable, evaluate_overlay, overlay_spans};
use dhampir_core::timeline::edit::{EditOp, TrimEdge, apply as apply_edit};
use dhampir_core::timeline::history::History;
use dhampir_core::timeline::host_api::{AssetInfoView, SampleView, gop_slices};
use dhampir_core::timeline::project::{
    Asset, AssetKind, ProjectDoc, asset_reference_counts, load_doc, validate_project_doc,
};
use dhampir_core::timeline::schema::{Frame, TimebaseDto, TrackKind};
use dhampir_core::timeline::subtitle::{
    AssStyle, Cue, CueStyle, ms_at_frame, parse_ass, parse_srt, to_ass, to_srt,
};
use dhampir_worker::pipeline::{AudioMode, RenderPlan, SourceTable, render_frames_png, render_plan};

const USAGE: &str = "\
用法：dhampir <子命令> [选项]

子命令：
  probe   --project <文件>                        解析并校验工程，打印 DocIssues
  info    --asset <文件>                          打印素材信息（尺寸/帧数/时间基/GOP 长度）
  gop     --asset <文件>                          打印 GOP 切片表
  frame   --project <文件> --frame <N> --out <目录>
                                                  出第 N 帧的 PNG（文件名 frame-<N>.png）
  frame   --project <文件> --from <N> --to <N> --out <目录>
                                                  出一段 PNG（一张一帧，名字同上）。
                                                  **与 --frame 只能给一种**：
                                                  同时给会退 2，而不是让其中一个悄悄赢。
                                                  只给 --from 就是「从这里到结尾」，
                                                  只给 --to 就是「从第 0 帧到这里」；
                                                  一个都不给则退 2（不替你猜要哪几帧）
  render  --project <文件> --from <N> --to <N> --out <文件.mp4>
                                                  出片（stdout 是 NDJSON 进度）
          [--subtitle-out <文件>] [--format srt|ass]
                                                  把这一段里的字幕另存一份**侧挂文件**。
                                                  --format 不给就看扩展名（.ass/.ssa -> ASS，
                                                  其余 -> SRT）；看不出来**不猜**，直接报错
          [--no-audio]                            出片**不要声音**。
                                                  默认是「工程里有音轨就出声音」；
                                                  明确给出来才走无声那条路 ——
                                                  它与引入音频之前**逐字节相同**，
                                                  也是排查声音问题时该拿来对照的那一份

公共选项：
  import  --project <文件> --file <素材> [--id <id>] [--replace] [--write]
                         把一个文件登记成资产（ffprobe 自动填尺寸/帧数/时间基）。
                         不给 --write 就是**干跑**：只打印将要写入的那一条
  library --project <文件>
                         列出素材库：每个资产被引用了多少次
  edit    --project <文件> --op <JSON> [--history <文件>] [--write]
                         执行一次编辑操作（与浏览器走的是同一份实现）。
                         形状是一个带 op 字段的 JSON 对象，六个操作：
                         insert / trim / split / move / remove / set_sequence
                         （split 的形状：op=split, layer=c, at=75）
                         **不给 --write 就只在内存里做一遍并打印结果**
  edit    --project <文件> --undo [--write]   （--redo 同理）
                        按 --history 指定的历史退一步 / 进一步。
                        历史存哪**必须由你说**（不替你往工程旁边写文件）；
                        历史文件不在 = 从空历史开始，不是错误。
                        没有可撤销 / 可重做的步骤时退出码 2 并明说
                        （静默什么都不做更难查）。
                        --history 只在 --write 时才落盘：干跑不碰磁盘

具名子命令（**同一实现的糖**：与 edit 走同一个 apply、同一条落盘路径）：
  undo / redo  --project <文件> --history <文件> [--write]
                        就是 edit --undo / edit --redo。连调用的函数都一样，
                        所以行为不可能有第二份
  clip    <动作> --project <文件> [--history <文件>] [--write]
                        把「动片段」写成具名动作，开关拼出来的操作与 --op **逐字段相同**：
                          insert --track <轨> --asset <id> --at <帧>
                                 --source-in <帧> --length <帧> [--id <id>]
                          trim   --layer <片段> --edge in|out --to <帧>
                          split  --layer <片段> --at <帧>
                          move   --layer <片段> --to <帧> [--track <轨>]
                          remove --layer <片段> [--ripple]
                        每个动作**要哪些开关、认哪些开关**都在参数这一关判：
                        少给会报，**多给也会报**（静静丢掉一个开关比报错难查得多）
  sequence <set> --project <文件> [--timebase <num/den>] [--width <像素>]
                 [--height <像素>] [--write]
                        序列设置：改帧率会按时间重算所有序列帧号。
                        不给 --timebase / --width / --height 的那一项就不动它
  batch   --project <文件> --script <文件> [--history <文件>] [--write]
                        --script 是脚本文件：**一行一个 op 的 JSON**
                        （空行与 # 开头是注释）。一次写、一条历史。
                        中途有一步不成立就**整份不落盘**，并明说卡在第几行 ——
                        否则用户只能靠 diff 猜是哪一步的问题

公共选项：
  --asset-root <目录>   工程文件里 asset.uri 的相对根（默认 target/s3）
  --asset-map <文件>     兜底资产登记表（形状：assets.<id>.file）。
                         **只补工程文件没登记的 id**：工程文件里的位置永远优先
  --width <像素>        输出宽度（默认取工程文件里的 render_hints.width）
  --height <像素>       输出高度（默认取工程文件里的 render_hints.height）
  --font-file <文件>    frame / render 画字幕用的字体文件（ttf/ttc/otf）。
                        工程里有字幕轨时**必须给**：本仓不内嵌字体、也不猜系统字体，
                        画不出来就判失败（问题码 subtitle_font_missing），
                        而不是静默出一份没有字幕的片子
  -h, --help            显示本帮助

  字幕有**两种口径**，可以只要一种，也可以都要 —— 它们是两条独立的路：
    * 烧进画面：--font-file（工程里有字幕轨时少给就判失败，见上）。
      它要 GPU 栅格化，所以只有 frame / render 走得到；
    * 侧挂文件：--subtitle-out（**只有 render 认**）。它不要字体也不要 GPU，
      搬的是「这一段里说过什么」：条目的时间是**相对这一趟的产物**从 0 起算的毫秒，
      内容只有文本与时间（源里的加粗/斜体/颜色不进侧挂）。
      没给 --font-file 时画面上的字一个都不会有，侧挂文件照写。

  subtitle 子命令不需要 GPU，也不需要 ffmpeg —— 它只出结构，不画图。

退出码：0 成功 / 2 用法或校验错 / 1 运行期失败";

/// 解析出来的选项。**用显式字段而不是一张 HashMap** ——
/// 拼错的名字要在解析阶段就变成错误，而不是到用的时候才发现取不到。
#[derive(Debug, Default, Clone, PartialEq)]
struct Args {
    command: String,
    /// 第二个位置参数。**只有 `clip` / `sequence` 用** ——
    /// 它们是「把 op 写成一个具名动作」，动作名跟在子命令后面
    /// （`clip split …`），而不是塞进 `--op` 的 JSON 里。
    op_name: Option<String>,
    project: Option<String>,
    asset: Option<String>,
    out: Option<String>,
    asset_root: Option<String>,
    asset_map: Option<String>,
    file: Option<String>,
    id: Option<String>,
    op: Option<String>,
    history: Option<String>,
    undo: bool,
    redo: bool,
    write: bool,
    replace: bool,
    from: Option<i64>,
    to: Option<i64>,
    frame: Option<i64>,
    width: Option<u32>,
    height: Option<u32>,
    font_file: Option<String>,
    /// 粗体字体文件（可选）。字重 >= 600 时用它 —— ffmpeg 的 drawtext
    /// **没有** bold 开关，粗体就是换一个字体文件。
    font_bold_file: Option<String>,
    /// 字体目录（可选）：按契约里的 `font_family` 名字在里面找。
    font_dir: Option<String>,
    /// 分块并行：`1` = 不分块（默认）、`0` = 自动、`n` = 指定 n。
    chunk_workers: Option<usize>,
    subtitle_out: Option<String>,
    format: Option<SidecarFormat>,
    /// `--track` / `--layer`：`clip insert` 放哪条轨 / 其余动作动哪个片段。
    track: Option<String>,
    layer: Option<String>,
    /// `--at`：插入的落点 / 剃刀的刀口。
    at: Option<i64>,
    /// `--source-in`：插入时**素材自己的**入点（素材帧号，不是序列帧号）。
    source_in: Option<i64>,
    /// `--length`：插入占多长（序列帧数）。
    length: Option<i64>,
    /// `--edge`：修剪哪一边（`in` / `out`）。**解析阶段就定死** ——
    /// 与 `--format` 同理，打错字要在参数这一关退 2。
    edge: Option<TrimEdge>,
    /// `--timebase`：序列帧率，写作 `num/den`（如 `30000/1001`）或单个数（按 den=1）。
    timebase: Option<TimebaseDto>,
    /// `--ripple`：删除时是否把后面的内容往前拉。不给就是留空。
    ripple: bool,
    /// `--script`：批处理脚本文件（一行一个 op 的 JSON）。
    script: Option<String>,
    /// `--no-audio`：明确不要声音。**明确**是要点 —— 它让"这次是故意静音的"
    /// 与"这次本该有声却没出"在日志里能分开。
    no_audio: bool,
    help: bool,
}

/// 侧挂字幕文件的格式。**只有两种，且在解析阶段就定死** ——
/// 非法值（`vtt`、`SRT` 之流）要在参数这一关退 2，而不是等出片跑完才发现写了一份没人认的文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidecarFormat {
    Srt,
    Ass,
}

impl SidecarFormat {
    /// 参数里认的拼法。**小写两个**，与 `--format` 的帮助文案同源。
    fn from_flag(value: &str) -> Option<Self> {
        match value {
            "srt" => Some(Self::Srt),
            "ass" => Some(Self::Ass),
            _ => None,
        }
    }

    /// 扩展名认出来的格式。认不出返回 None —— **认不出就不猜**，
    /// 猜错的表现是"文件名叫 .ass、里面其实是 SRT"，播放器只会说解析失败。
    fn from_extension(path: &Path) -> Option<Self> {
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase());
        match extension.as_deref() {
            Some("srt") => Some(Self::Srt),
            Some("ass") | Some("ssa") => Some(Self::Ass),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Ass => "ass",
        }
    }
}

/// 认得的**带值**选项。不在表里的一律报错。
const KNOWN_VALUE_FLAGS: [&str; 25] = [
    "--project",
    "--asset",
    "--out",
    "--asset-root",
    "--asset-map",
    "--file",
    "--id",
    "--op",
    "--history",
    "--from",
    "--to",
    "--width",
    "--height",
    "--font-file",
    "--font-bold-file",
    "--font-dir",
    "--chunk-workers",
    // clip / sequence / batch 用的（见 EDIT_FAMILY）。
    "--track",
    "--layer",
    "--at",
    "--source-in",
    "--length",
    "--edge",
    "--timebase",
    "--script",
];
/// 认得的**不带值**选项。
const KNOWN_FLAGS: [&str; 9] = [
    "--frame",
    "--write",
    "--replace",
    "--undo",
    "--redo",
    "--no-audio",
    "--ripple",
    "-h",
    "--help",
];
/// 侧挂导出的两个带值选项。
///
/// **它们不跟 `KNOWN_VALUE_FLAGS` 混在一起**，因为它们比别的选项多两条规矩：
/// 只有 render 认（sidecar 是"一段区间"的导出，frame 只有一帧，谈区间没有意义），
/// 而且 `--format` 的值只认 srt / ass。那两条都在解析阶段判 —— 打错字要立刻看到。
const KNOWN_SIDECAR_FLAGS: [&str; 2] = ["--subtitle-out", "--format"];

/// **编辑那一组**子命令。
///
/// 它们共用的不是"差不多的代码"，而是**同一个函数**：`apply_edit` 是唯一的操作入口，
/// 写盘与历史也只有一条路径（`record_edit`）。具名子命令做的事只有一件 ——
/// 把开关拼成同一个 `EditOp`。所以「同一实现的糖」这句话能被测出来：
/// 具名写法与等价的 `edit --op` 摊出来的 `EditOp` **逐字段相同**（见本文件的单测）。
const EDIT_FAMILY: [&str; 6] = ["edit", "undo", "redo", "clip", "sequence", "batch"];

/// 认 `--write` 的子命令。
///
/// **比 `EDIT_FAMILY` 多一个 `import`** —— 它也要落盘（把资产登记进工程）。
/// 这两张表分开写是有意的：把它们合成一张，"import 认不认 --history" 这个问题
/// 就被顺手答成"认"了，而那是错的。
const WRITE_FAMILY: [&str; 7] = [
    "edit", "undo", "redo", "clip", "sequence", "batch", "import",
];

/// `clip` 认的动作名。**与 `EditOp` 里那五个"动片段"的变体一一对应** ——
/// `set_sequence` 不在这里，它是 `sequence set`（它动的是序列，不是片段）。
const CLIP_OPS: [&str; 5] = ["insert", "trim", "split", "move", "remove"];

/// 认得的子命令。
const COMMANDS: [&str; 14] = [
    "probe", "info", "gop", "frame", "render", "import", "library", "edit", "subtitle", "undo",
    "redo", "clip", "sequence", "batch",
];

/// `clip` / `sequence` **专用**的那些开关（别处一个都不认）。
///
/// 这是一张**显式表**而不是"看一眼代码里哪些字段"，因为漏掉一个的表现是
/// 「参数被静静丢掉」—— 正是这份文件开头在防的那件事。
fn clip_only_flags(args: &Args) -> Vec<(&'static str, bool)> {
    vec![
        ("--track", args.track.is_some()),
        ("--layer", args.layer.is_some()),
        ("--at", args.at.is_some()),
        ("--source-in", args.source_in.is_some()),
        ("--length", args.length.is_some()),
        ("--edge", args.edge.is_some()),
        ("--ripple", args.ripple),
        ("--timebase", args.timebase.is_some()),
    ]
}

/// 那五个**别处也认**、但对具名动作也有意义的开关
/// （`--asset` 归 info / import，`--to` 归 render，`--width` / `--height` 归 render / frame）。
/// 它们不能进 `clip_only_flags`（那样会把 render 也判红），但**必须**进每个动作的"总共认"清单 ——
/// 否则 `clip split --width 100` 会被静静收下。
fn shared_op_flags(args: &Args) -> Vec<(&'static str, bool)> {
    vec![
        ("--asset", args.asset.is_some()),
        ("--to", args.to.is_some()),
        ("--id", args.id.is_some()),
        ("--width", args.width.is_some()),
        ("--height", args.height.is_some()),
    ]
}

/// 一个具名动作**要**哪些开关、**总共认**哪些开关。
///
/// 返回"总共认"而不是"还可以给哪些"是有意的：多给一个开关等于**那个开关被丢掉**，
/// 而它的值会被读成默认值 —— 那是一种很安静的错。
fn op_shape(op: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match op {
        "insert" => (
            &["--track", "--asset", "--at", "--source-in", "--length"],
            &[
                "--track", "--asset", "--at", "--source-in", "--length", "--id",
            ],
        ),
        "trim" => (
            &["--layer", "--edge", "--to"],
            &["--layer", "--edge", "--to"],
        ),
        "split" => (&["--layer", "--at"], &["--layer", "--at"]),
        "move" => (&["--layer", "--to"], &["--layer", "--to", "--track"]),
        "remove" => (&["--layer"], &["--layer", "--ripple"]),
        "set" => (&["--timebase"], &["--timebase", "--width", "--height"]),
        _ => (&[], &[]),
    }
}

/// 一个**用法错**（退出码 2），与运行期失败（退出码 1）分开。
///
/// # 它为什么不是一句 `format!`
///
/// 「少给了一个开关」与「GPU 拿不到上下文」是两种完全不同的事，而它们从同一条
/// `Result<_, String>` 里出来时长得一模一样 —— 调用方只能去做字符串匹配，
/// 而那种判据会在文案改一个字之后静默失效（表现是"用法错被当成运行期失败"，
/// 于是调用方去查渲染管线，而真正该做的是补上那个开关）。
///
/// 所以这里给用法错一个**类型**：`main` 只看类型，不看文案。
/// 运行期失败仍是裸 `String`，两种不会互相冒充。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Usage(String);

impl Usage {
    /// 退出码。**只此一处**定义"用法错退几" —— 别处再写一个 2 就会漂。
    fn code(self) -> ExitCode {
        eprintln!("{self}");
        ExitCode::from(2)
    }
}

impl std::fmt::Display for Usage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// 让 `?` 能把用法错送进 `Result<_, String>` 的那些函数。
///
/// # 为什么单靠这个 `From` 还不够
///
/// 它只解决**编译**：`cmd_*` 仍声明成 `Result<ExitCode, String>`，用法错在类型上
/// 被抹平成字符串，`main` 就再也分不出它。所以真正的判据在下面这个 [`CommandError`]：
/// 子命令返回的是它，而不是裸 `String` —— 于是"用法错退 2、运行期失败退 1"
/// 这件事在类型上就没法写错。
impl From<Usage> for String {
    fn from(error: Usage) -> Self {
        error.0
    }
}

/// 子命令的报错：**两类分开**。
///
/// `main` 只按这个枚举决定退出码，不去看文案 —— 文案会改，而分类不会。
#[derive(Debug, Clone)]
enum CommandError {
    /// 用法错（退出码 2）：少给了开关、值不对、区间反了。**用户能改**。
    Usage(Usage),
    /// 运行期失败（退出码 1）：GPU、ffmpeg、写盘、渲染报告判失败。
    Runtime(String),
}

impl CommandError {
    fn code(self) -> ExitCode {
        match self {
            Self::Usage(error) => error.code(),
            Self::Runtime(message) => {
                eprintln!("{message}");
                ExitCode::from(1)
            }
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Usage(error) => write!(formatter, "{error}"),
            Self::Runtime(message) => formatter.write_str(message),
        }
    }
}

/// 子命令里那些 `?` 的来源：裸 `String` 与 [`Usage`] 都收，但**落点不同**。
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::Runtime(message)
    }
}

impl From<Usage> for CommandError {
    fn from(error: Usage) -> Self {
        Self::Usage(error)
    }
}

/// 造一个用法错。`format!` 的参数与它同形，所以调用点读起来与从前一样。
fn usage_error(message: String) -> Usage {
    Usage(message)
}

/// 报错文案里的名单。
fn fmt_flags(names: &[&str]) -> String {
    if names.is_empty() {
        "（没有）".to_string()
    } else {
        names.join(" / ")
    }
}

/// 解析。**纯函数**，所以能脱离命令行单测。
fn parse(argv: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut index = 0;
    while index < argv.len() {
        let token = argv[index].as_str();
        if token == "-h" || token == "--help" {
            args.help = true;
            index += 1;
            continue;
        }
        if token == "--write"
            || token == "--replace"
            || token == "--undo"
            || token == "--redo"
            || token == "--no-audio"
            || token == "--ripple"
        {
            match token {
                "--write" => args.write = true,
                "--replace" => args.replace = true,
                "--undo" => args.undo = true,
                "--no-audio" => args.no_audio = true,
                "--ripple" => args.ripple = true,
                _ => args.redo = true,
            }
            index += 1;
            continue;
        }
        if !token.starts_with('-') {
            if args.command.is_empty() {
                if !COMMANDS.contains(&token) {
                    return Err(format!("不认识的子命令：{token}"));
                }
                args.command = token.to_string();
                index += 1;
                continue;
            }
            // 第二个位置参数：只有 clip / sequence 收，而且只收一个。
            // 别的子命令多给一个词一律报错 —— 静默丢掉一个位置参数，
            // 用户会以为它生效了（与"选项被丢掉"同一种坏）。
            if (args.command == "clip" || args.command == "sequence") && args.op_name.is_none() {
                args.op_name = Some(token.to_string());
                index += 1;
                continue;
            }
            return Err(format!("多余的位置参数：{token}"));
        }
        if !KNOWN_VALUE_FLAGS.contains(&token)
            && !KNOWN_FLAGS.contains(&token)
            && !KNOWN_SIDECAR_FLAGS.contains(&token)
        {
            return Err(format!("不认识的选项：{token}"));
        }
        let value = argv
            .get(index + 1)
            .ok_or_else(|| format!("{token} 后面要跟一个值"))?
            .clone();
        match token {
            "--project" => args.project = Some(value),
            "--asset" => args.asset = Some(value),
            "--out" => args.out = Some(value),
            "--asset-root" => args.asset_root = Some(value),
            "--asset-map" => args.asset_map = Some(value),
            "--file" => args.file = Some(value),
            "--id" => args.id = Some(value),
            "--op" => args.op = Some(value),
            "--history" => args.history = Some(value),
            "--from" => args.from = Some(parse_int(&token, &value)?),
            "--to" => args.to = Some(parse_int(&token, &value)?),
            "--frame" => args.frame = Some(parse_int(&token, &value)?),
            "--width" => args.width = Some(parse_uint(&token, &value)?),
            "--height" => args.height = Some(parse_uint(&token, &value)?),
            "--font-file" => args.font_file = Some(value),
            "--font-bold-file" => args.font_bold_file = Some(value),
            "--font-dir" => args.font_dir = Some(value),
            // **这里的 0 是有意义的**（自动），所以不能用 `parse_uint`（它把 0 当错）。
            "--chunk-workers" => {
                args.chunk_workers = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| format!("--chunk-workers 要一个非负整数，得到 {value}"))?,
                )
            }
            "--subtitle-out" => args.subtitle_out = Some(value),
            "--format" => {
                let parsed = SidecarFormat::from_flag(&value).ok_or_else(|| {
                    format!("--format 只认 srt / ass，得到 {value}")
                })?;
                args.format = Some(parsed);
            }
            "--track" => args.track = Some(value),
            "--layer" => args.layer = Some(value),
            "--at" => args.at = Some(parse_int(&token, &value)?),
            "--source-in" => args.source_in = Some(parse_int(&token, &value)?),
            "--length" => args.length = Some(parse_int(&token, &value)?),
            "--edge" => {
                // 与 `--format` 同一条口径：**值只认两种，且在参数这一关判**。
                // 等到动手时才发现边写错，是一条成功的编辑被报成用法错。
                let parsed = match value.as_str() {
                    "in" => TrimEdge::In,
                    "out" => TrimEdge::Out,
                    other => {
                        return Err(format!("--edge 只认 in / out，得到 {other}"));
                    }
                };
                args.edge = Some(parsed);
            }
            "--timebase" => {
                let (num, den) = parse_rate(&value)
                    .ok_or_else(|| format!("--timebase 要 num/den（如 30000/1001）或一个数，得到 {value}"))?;
                if num == 0 {
                    return Err(format!("--timebase 的分子不能是 0（得到 {value}）"));
                }
                args.timebase = Some(TimebaseDto { num, den });
            }
            "--script" => args.script = Some(value),
            other => return Err(format!("不认识的选项：{other}")),
        }
        index += 2;
    }
    // 侧挂导出只有 render 认。**别的子命令静默收下就是"参数被丢掉"** ——
    // 用户以为写了侧挂文件，结果什么都没有，而退出码还是 0。
    let wants_sidecar = args.subtitle_out.is_some() || args.format.is_some();
    if wants_sidecar && !args.command.is_empty() && args.command != "render" {
        return Err(format!(
            "--subtitle-out / --format 只有 render 认（现在给的是 {}）：\
             侧挂文件是「一段区间」的导出，frame 只出一帧，谈区间没有意义",
            args.command
        ));
    }
    // 同理：`--format` 说的是"那份侧挂文件"的格式，没有文件就没有它说明的对象。
    if args.format.is_some() && args.subtitle_out.is_none() {
        return Err("--format 要跟着 --subtitle-out：它说明的是那份侧挂文件的格式".to_string());
    }
    // 撤销/重做那两个**旗标**只归 edit —— 它们的新写法是具名子命令 undo / redo。
    // 口径与上面侧挂那两条**完全一样**：别的子命令静默收下就是「参数被丢掉」——
    // 用户以为退了，结果什么都没发生，而退出码还是 0。
    if (args.undo || args.redo) && !args.command.is_empty() && args.command != "edit" {
        return Err(format!(
            "--undo / --redo 只有 edit 认（现在给的是 {}）；\
             另有两个具名写法：dhampir undo / dhampir redo",
            args.command
        ));
    }
    // `--history` / `--write` 的归属**不是同一张表**：`--history` 只有编辑那一组认，
    // 而 `--write` 连 `import` 也认（它也要落盘）。合成一张就会把
    // 「import 认不认 --history」顺手答成"认"，而那是错的。
    if args.history.is_some()
        && !args.command.is_empty()
        && !EDIT_FAMILY.contains(&args.command.as_str())
    {
        return Err(format!(
            "--history 只有 {} 认（现在给的是 {}）",
            fmt_flags(&EDIT_FAMILY),
            args.command
        ));
    }
    if args.write && !args.command.is_empty() && !WRITE_FAMILY.contains(&args.command.as_str()) {
        return Err(format!(
            "--write 只有 {} 认（现在给的是 {}）",
            fmt_flags(&WRITE_FAMILY),
            args.command
        ));
    }
    // `--op` 只归 edit：具名子命令存在的意义就是**不必**手写那段 JSON。
    if args.op.is_some() && !args.command.is_empty() && args.command != "edit" {
        return Err(format!(
            "--op 只有 edit 认（现在给的是 {}）：clip / sequence 用开关拼同一个操作",
            args.command
        ));
    }
    if args.undo && args.redo {
        return Err("--undo 与 --redo 只能给一个".to_string());
    }
    if (args.undo || args.redo) && args.op.is_some() {
        return Err("--op 与 --undo / --redo 只能给一个：一次只做一件事".to_string());
    }
    // **历史存哪由调用方说**：随手往工程旁边写一个隐藏文件，会在用户没要求的地方留下东西
    // （本仓自己的 fixtures/ 第一个就中），而「看不出来不猜」是本仓一贯口径。
    if (args.undo || args.redo) && args.history.is_none() {
        return Err(
            "--undo / --redo 要跟 --history <文件>：历史存哪得由你说，本工具不替你猜".to_string(),
        );
    }
    // ---- 具名子命令 ----
    // undo / redo：与 `edit --undo` 同一条规矩（历史存哪必须由你说）。
    if (args.command == "undo" || args.command == "redo") && args.history.is_none() {
        return Err(format!(
            "{} 要跟 --history <文件>：历史存哪得由你说，本工具不替你猜",
            args.command
        ));
    }
    // clip / sequence：动作名必给，而且必须在名单里。
    if args.command == "clip" {
        let op = args
            .op_name
            .as_deref()
            .ok_or_else(|| format!("clip 要一个动作名：{}", fmt_flags(&CLIP_OPS)))?;
        if !CLIP_OPS.contains(&op) {
            return Err(format!(
                "clip 不认识的动作：{op}（认 {}）",
                fmt_flags(&CLIP_OPS)
            ));
        }
    }
    if args.command == "sequence" {
        match args.op_name.as_deref() {
            None => return Err("sequence 要一个动作名：set".to_string()),
            Some("set") => {}
            Some(other) => {
                return Err(format!("sequence 不认识的动作：{other}（只认 set）"));
            }
        }
    }
    // batch：脚本文件必给。
    if args.command == "batch" && args.script.is_none() {
        return Err("batch 要 --script <文件>：一行一个 op 的 JSON".to_string());
    }
    // 那八个开关只归 clip / sequence。
    if args.command != "clip" && args.command != "sequence" {
        let stray: Vec<&str> = clip_only_flags(&args)
            .into_iter()
            .filter(|(_, present)| *present)
            .map(|(name, _)| name)
            .collect();
        if !stray.is_empty() {
            return Err(format!(
                "{} 只有 clip / sequence 认（现在给的是 {}）",
                fmt_flags(&stray),
                if args.command.is_empty() {
                    "（没给子命令）"
                } else {
                    args.command.as_str()
                }
            ));
        }
    }
    // `--script` 只归 batch。
    if args.script.is_some() && args.command != "batch" {
        return Err(format!(
            "--script 只有 batch 认（现在给的是 {}）",
            args.command
        ));
    }
    // 每个动作**要**哪些、**总共认**哪些。多给与少给都是错 —— 两种都会让"参数被丢掉"。
    if args.command == "clip" || args.command == "sequence" {
        let op = args.op_name.as_deref().unwrap_or_default();
        let (need, allowed) = op_shape(op);
        let mut given: Vec<(&'static str, bool)> = clip_only_flags(&args);
        given.extend(shared_op_flags(&args));
        for (name, present) in &given {
            if *present && !allowed.contains(name) {
                return Err(format!(
                    "{op} 不吃 {name}（它认的是 {}）",
                    fmt_flags(allowed)
                ));
            }
        }
        for name in need {
            if !given.iter().any(|(n, present)| n == name && *present) {
                return Err(format!("{op} 要 {name}（它认的是 {}）", fmt_flags(allowed)));
            }
        }
    }
    Ok(args)
}

fn parse_int(name: &str, value: &str) -> Result<i64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} 要一个整数，得到 {value}"))
}

fn parse_uint(name: &str, value: &str) -> Result<u32, String> {
    let parsed: u32 = value
        .parse()
        .map_err(|_| format!("{name} 要一个非负整数，得到 {value}"))?;
    if parsed == 0 {
        return Err(format!("{name} 不能是 0"));
    }
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// ffprobe 的两件事：流信息与包列表
// ---------------------------------------------------------------------------

fn ffprobe_json(args: &[&str], file: &Path) -> Result<serde_json::Value, String> {
    let output = Command::new("ffprobe")
        .args(args)
        .arg(file)
        .output()
        .map_err(|error| format!("起不了 ffprobe：{error}（PATH 里有 ffprobe 吗？）"))?;
    if !output.status.success() {
        return Err(format!(
            "ffprobe 读不了 {}：{}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("ffprobe 的输出不是 JSON：{error}"))
}

/// 解析 ffprobe 给的有理数字符串（如 30/1）。
fn parse_rate(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split('/');
    let num: u32 = parts.next()?.trim().parse().ok()?;
    let den: u32 = parts.next().unwrap_or("1").trim().parse().ok()?;
    if den == 0 {
        return None;
    }
    Some((num, den))
}

/// 从 ffprobe 的一个字段里取文本。
///
/// **它给的 JSON 里类型是混的**：pos/size/flags 是字符串，dts/duration 是数字。
/// 只认字符串的话，duration 会被静默当成 0 —— 那正是"错得很安静"的那一类。
fn field_text(item: &serde_json::Value, name: &str) -> Option<String> {
    match item.get(name)? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 包列表 -> SampleView 列表。返回的第二个值是**被减掉的时间戳原点**。
///
/// # 为什么要归零
///
/// 契约里的 SampleView.dts 是 **u64**，而真实 MP4 的 dts **可以是负的** ——
/// 带 B 帧或 edit list 的片子第一个包的 dts 就是负的（实测
/// target/s3/proxy1080p.mp4 是 -512）。
///
/// 三条路里只有一条是对的：
///
/// * 报错 —— 于是 info/gop 在**真实素材上从来跑不通**（第一版就是这么写的，实测踩到）；
/// * 夹成 0 —— 多个包会撞到同一个时间戳，而且"时间戳从 0 开始"变成一句没说的谎；
/// * **减去首包 dts 并把这个偏移报出去** —— 时间戳变成相对量，且偏移是可见的。
///
/// 选第三条。它不影响 GOP 切分（那只看 is_sync 与字节范围），
/// 而调用方若需要绝对时间戳，拿 origin 就能还原。
///
/// 键名是**单字母**（o/s/d/u/k），那是契约层定下的，别改 ——
/// 这里只是把 ffprobe 的字段搬进去，不重新发明一套。
pub fn samples_from_packets(packets: &serde_json::Value) -> Result<(Vec<SampleView>, i64), String> {
    let list = packets
        .get("packets")
        .and_then(serde_json::Value::as_array)
        .ok_or("ffprobe 的输出里没有 packets 数组")?;
    let mut raw = Vec::with_capacity(list.len());
    for (index, packet) in list.iter().enumerate() {
        let offset: usize = field_text(packet, "pos")
            .ok_or_else(|| format!("第 {index} 个包没有 pos"))?
            .parse()
            .map_err(|_| format!("第 {index} 个包的 pos 不是数"))?;
        let size: usize = field_text(packet, "size")
            .ok_or_else(|| format!("第 {index} 个包没有 size"))?
            .parse()
            .map_err(|_| format!("第 {index} 个包的 size 不是数"))?;
        let dts_text =
            field_text(packet, "dts").ok_or_else(|| format!("第 {index} 个包没有 dts"))?;
        let dts: i64 = dts_text
            .parse()
            .map_err(|_| format!("第 {index} 个包的 dts 不是数：{dts_text}"))?;
        let duration: u32 = field_text(packet, "duration")
            .unwrap_or_else(|| "0".to_string())
            .parse()
            .map_err(|_| format!("第 {index} 个包的 duration 不是数"))?;
        let flags = field_text(packet, "flags").unwrap_or_default();
        raw.push((offset, size, dts, duration, flags.contains('K')));
    }
    // 原点取所有包 dts 的最小值（不只是首包：edit list 下首包未必最小）。
    let origin = raw.iter().map(|row| row.2).min().unwrap_or(0);
    let samples = raw
        .into_iter()
        .map(|(offset, size, dts, duration, is_sync)| SampleView {
            offset,
            size,
            // origin <= 每个 dts，所以这个减法不会出负数。
            dts: dts.saturating_sub(origin) as u64,
            duration,
            is_sync,
        })
        .collect();
    Ok((samples, origin))
}

const PACKET_ARGS: [&str; 9] = [
    "-v",
    "error",
    "-select_streams",
    "v:0",
    "-show_packets",
    "-show_entries",
    "packet=pos,size,dts,duration,flags",
    "-of",
    "json",
];

/// 素材信息。**形状就是契约里的 AssetInfoView**，不另造一套。
fn asset_info(file: &Path) -> Result<AssetInfoView, String> {
    let stream = ffprobe_json(
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames,width,height,avg_frame_rate",
            "-of",
            "json",
        ],
        file,
    )?;
    let stream = stream
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .and_then(|list| list.first())
        .ok_or_else(|| format!("{} 里没有视频流", file.display()))?;
    // **同样要按混合类型读**：实测 ffprobe 的 width/height/nb_read_frames 是字符串，
    // 但不同版本/不同格式下可能是数字。只认一种就会静默给出 0 —— 上面那个
    // "width: 0" 就是这么来的，它是**错的**而不是"未知"。
    let text_field = |name: &str| field_text(stream, name);
    let width: u32 = text_field("width")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出宽度".to_string())?;
    let height: u32 = text_field("height")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出高度".to_string())?;
    let frame_count: i64 = text_field("nb_read_frames")
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| "ffprobe 没给出帧数（-count_frames 没生效？）".to_string())?;
    let (num, den) = text_field("avg_frame_rate")
        .and_then(|text| parse_rate(&text))
        .ok_or_else(|| "ffprobe 没给出可解析的帧率".to_string())?;

    let packets = ffprobe_json(&PACKET_ARGS, file)?;
    let (samples, dts_origin) = samples_from_packets(&packets)?;
    if dts_origin != 0 {
        // 归零是**可见的**：不写出来就成了"时间戳从 0 开始"这句没说的谎。
        eprintln!("时间戳已按首包归零（原 dts 最小值 {dts_origin}，流时间基内）。");
    }
    let slices = gop_slices(&samples);
    // GOP 长度**由切片实测**，不写死也不猜：只有至少两段时才谈得上"间隔"。
    // 0 表示未知 —— 契约里明说此时不能按 GOP 切。
    let gop_length = if slices.len() >= 2 {
        u32::try_from(slices[0].sample_count).unwrap_or(0)
    } else {
        0
    };

    Ok(AssetInfoView {
        id: file
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default(),
        kind: "video".to_string(),
        frame_count,
        timebase: TimebaseDto { num, den },
        width,
        height,
        gop_length,
        // 本 CLI 不做代理生成 —— 如实说没有，而不是给一个好看的 true。
        proxy_available: false,
    })
}

// ---------------------------------------------------------------------------
// 工程
// ---------------------------------------------------------------------------

fn read_doc(path: &str) -> Result<ProjectDoc, String> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("读不了工程 {path}：{error}"))?;
    load_doc(&text).map_err(|error| format!("工程 {path} 载入失败：{error}"))
}

/// 载入工程；失败就打一行人话并**退 2**。
///
/// 文件不在、JSON 不合法、字段不符，都是**用户能改的**错 ——
/// 报成运行期失败（1）会让人以为"程序坏了"，而去查不该查的地方。
fn load_project_or_usage(path: &str) -> Result<ProjectDoc, ExitCode> {
    match read_doc(path) {
        Ok(doc) => Ok(doc),
        Err(error) => {
            eprintln!("{error}");
            Err(ExitCode::from(2))
        }
    }
}

/// 读兜底资产登记表。形状与 fixtures/local-assets.json 一致：
/// {"assets":{"<id>":{"file":"...","kind":"..."}}}，file 相对 asset_root 解析。
pub fn load_asset_map(path: &str, asset_root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("读不了兜底登记表 {path}：{error}"))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|error| format!("兜底登记表不是 JSON：{error}"))?;
    let table = value
        .get("assets")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("兜底登记表 {path} 里没有 assets 对象"))?;
    let mut rows = Vec::new();
    for (id, entry) in table {
        let Some(file) = entry.get("file").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if file.is_empty() {
            continue;
        }
        let raw = Path::new(file);
        rows.push((
            id.clone(),
            if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                asset_root.join(raw)
            },
        ));
    }
    Ok(rows)
}

/// 素材表：id -> 文件。**位置由宿主解释**，所以相对 uri 要挂到 --asset-root 上。
///
/// 优先级：**工程文件的 assets 先来，兜底表只补缺**。
/// 反过来的话，兜底表会悄悄盖掉工程文件里写的真实位置，而用户看不到。
fn build_sources(
    doc: &ProjectDoc,
    asset_root: &Path,
    fallback: &[(String, PathBuf)],
) -> SourceTable {
    let mut table = SourceTable::new();
    for asset in &doc.assets {
        if asset.uri.is_empty() {
            continue;
        }
        let raw = Path::new(&asset.uri);
        let file = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            asset_root.join(raw)
        };
        table.insert(asset.id.clone(), file);
    }
    for (id, file) in fallback {
        if table.file_for(id).is_none() {
            table.insert(id.clone(), file.clone());
        }
    }
    table
}

/// 解析兜底表；给了路径但读不了就**报错而不是忽略** ——
/// 静默忽略会让"兜底没生效"和"兜底不需要"看起来一样。
fn resolve_fallback(args: &Args, asset_root: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    match args.asset_map.as_ref() {
        None => Ok(Vec::new()),
        Some(path) => load_asset_map(path, asset_root),
    }
}

/// 输出尺寸：命令行 > 工程文件里的 render_hints。
fn resolve_size(args: &Args, doc: &ProjectDoc) -> (u32, u32) {
    (
        args.width.unwrap_or(doc.render_hints.width),
        args.height.unwrap_or(doc.render_hints.height),
    )
}

/// 帧区间：没给就取整个工程（左闭右开换算成闭区间）。
fn resolve_range(args: &Args, doc: &ProjectDoc) -> Result<(i64, i64), String> {
    let first = compose::first_frame_v2(&doc.timeline).unwrap_or(0);
    let end = compose::end_frame_v2(&doc.timeline).unwrap_or(0);
    let from = args.from.unwrap_or(first);
    let to = args.to.unwrap_or(end.saturating_sub(1));
    if to < from {
        return Err(format!("帧区间是空的：from={from} to={to}"));
    }
    Ok((from, to))
}

/// 校验不通过就打印清单并返回退出码 2。**校验只有一份实现**，在契约层。
fn gate(doc: &ProjectDoc) -> Option<ExitCode> {
    let issues = validate_project_doc(doc, REGISTRY);
    if issues.is_ok() {
        return None;
    }
    eprintln!("工程没通过校验，先修它：");
    eprintln!(
        "{}",
        serde_json::to_string_pretty(&issues).unwrap_or_default()
    );
    Some(ExitCode::from(2))
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<ExitCode, CommandError> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// 子命令
// ---------------------------------------------------------------------------

fn cmd_probe(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("probe 要 --project <文件>".to_string()))?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let issues = validate_project_doc(&doc, REGISTRY);
    // 校验有 error 就让退出码说话 —— 调用方不该去解析 JSON 才知道失败了。
    // 但 stdout 上仍然给完整清单：**退出码与内容是两件事**。
    let code = if issues.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    };
    print_json(&issues)?;
    Ok(code)
}

fn cmd_info(args: &Args) -> Result<ExitCode, CommandError> {
    let asset = args
        .asset
        .as_ref()
        .ok_or_else(|| usage_error("info 要 --asset <文件>".to_string()))?;
    let info = asset_info(Path::new(asset))?;
    print_json(&info)
}

fn cmd_gop(args: &Args) -> Result<ExitCode, CommandError> {
    let asset = args
        .asset
        .as_ref()
        .ok_or_else(|| usage_error("gop 要 --asset <文件>".to_string()))?;
    let packets = ffprobe_json(&PACKET_ARGS, Path::new(asset))?;
    let (samples, dts_origin) = samples_from_packets(&packets)?;
    let slices = gop_slices(&samples);
    print_json(&serde_json::json!({
        "asset": asset,
        "samples": samples.len(),
        "sync_samples": samples.iter().filter(|sample| sample.is_sync).count(),
        // 样本表里的 dts 是**相对量**：这个偏移已经在每个 dts 上减掉了。
        // 写出来，需要绝对时间戳的调用方才能还原。
        "dts_origin": dts_origin,
        "slices": slices,
    }))
}

/// 这份工程里**会被出片忽略**的音轨 id。
///
/// 抽成纯函数有两个理由：能脱离 GPU 单测；以及"哪些东西没被渲染"这件事
/// 应该是**列出来的**，而不是一句笼统的"不支持音频"。
fn audio_track_ids(timeline: &dhampir_core::timeline::layer::TimelineV2) -> Vec<String> {
    timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| track.id.clone())
        .collect()
}

/// 帧区间取到的**工程量**，给调用方在真去解码之前知道要处理多少帧。
fn project_frames(doc: &ProjectDoc) -> usize {
    let end = compose::end_frame_v2(&doc.timeline).unwrap_or(0);
    let first = compose::first_frame_v2(&doc.timeline).unwrap_or(0);
    usize::try_from(end.saturating_sub(first).max(0)).unwrap_or(0)
}

/// `frame` 要出哪几帧。
///
/// # 为什么是两种写法而不是一种
///
/// 「出一帧」与「出一段」在调用方那里是两件事：前者是看一眼这一帧长什么样，
/// 后者是拿一串 PNG 去拼预览或做像素比对。
///
/// 从前只有 `--frame`，而**多给的 `--to` 会被静默收下**（解析阶段认它，
/// 这里却不读）—— 用户以为出了一段，实际只出了一帧，退出码还是 0。
/// 这正是本文件开头在防的那件事，所以现在把它变成一条真的路。
///
/// 两种写法**互斥**：同时给 `--frame` 与 `--from` / `--to` 是用法错。
/// 让后者悄悄赢（或让前者悄悄赢）都会产出与用户预期不同的那一份。
fn frame_range(args: &Args) -> Result<Vec<Frame>, Usage> {
    let explicit = args.frame.is_some();
    let ranged = args.from.is_some() || args.to.is_some();
    if explicit && ranged {
        return Err(usage_error(
            "--frame 与 --from / --to 只能给一种：前者出一帧，后者出一段".to_string(),
        ));
    }
    if explicit {
        return Ok(vec![args.frame.expect("刚刚判过它存在")]);
    }
    if !ranged {
        return Err(usage_error(
            "frame 要 --frame <N>，或者 --from <N> / --to <N> 出一段".to_string(),
        ));
    }
    // 只给一头是**有意的**，但两头的含义**不对称** —— 这是这里唯一容易写错的地方：
    //
    //   * 只给 `--from 10`：“从第 10 帧起，到工程结尾”；
    //   * 只给 `--to 5` ：“从第 0 帧到第 5 帧”。
    //
    // 两头都缺省成"另一头"的话，`--to 5` 会变成 `from=5, to=5`（只出第 5 帧）——
    // 一个看着成功、实际少了 5 帧的结果。缺的那一头固定取 0，不做对称处理。
    let from = args.from.unwrap_or(0);
    let to = args.to.unwrap_or(from);
    if to < from {
        return Err(usage_error(format!(
            "帧区间是空的：from={from} to={to}（--to 要比 --from 大）"
        )));
    }
    Ok((from..=to).collect())
}

fn cmd_frame(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("frame 要 --project <文件>".to_string()))?;
    let out = args
        .out
        .as_ref()
        .ok_or_else(|| usage_error("frame 要 --out <目录>".to_string()))?;
    let frames = frame_range(args)?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    if let Some(code) = gate(&doc) {
        return Ok(code);
    }
    let (width, height) = resolve_size(args, &doc);
    let root = PathBuf::from(
        args.asset_root
            .clone()
            .unwrap_or_else(|| "target/s3".to_string()),
    );
    let sources = build_sources(&doc, &root, &resolve_fallback(args, &root)?);
    // 粗体与字体目录是可选的：没给就是 None（见 RenderPlan 那两个字段的说明）。
    let font_bold_file = args.font_bold_file.as_deref().map(Path::new);
    let font_dir = args.font_dir.as_deref().map(Path::new);
    // 默认 `1`：不分块。要速度就显式给 `--chunk-workers 0`（自动）或一个具体数。
    let chunk_workers = args.chunk_workers.unwrap_or(1);
    let font_file = match resolve_font(args) {
        Ok(font) => font,
        Err(code) => return Ok(code),
    };
    // 字幕：与 subtitle 子命令**同一份装载**。工程里没有字幕素材时它是空表，
    // 于是 evaluate_overlay 直接给 None —— 无字幕工程的输出逐字节不变。
    let subtitles = load_subtitles(&doc, &sources)?;

    // 文件名固定按帧号，调用方给的是**目录** —— 这样同一帧重跑一定落在同一个路径上。
    // 区间也走这一条：文件名是 `frame-<帧号四位>.png`，一帧一条，不会互相覆盖。
    let output = PathBuf::from(out).join("frame.png");
    // **把资产时间基带上。** 少了它就会退回恒等换算（素材帧率按时间线算），
    // 而 60fps 素材放进 30fps 工程的表现是**半速播放**。
    let asset_timebases = doc.asset_timebases();
    let (first_frame, last_frame) = (
        frames.first().copied().expect("frame_range 一定给至少一帧"),
        frames.last().copied().expect("frame_range 一定给至少一帧"),
    );
    let plan = RenderPlan {
        timeline: &doc.timeline,
        sources: &sources,
        asset_timebases: &asset_timebases,
        from: first_frame,
        to: last_frame,
        width,
        height,
        sequence: doc.sequence_size(),
        subtitles: &subtitles,
        font_file,
        font_bold_file,
        font_dir,
        chunk_workers,
        // frame 出的是 PNG：没有容器可放音轨。
        audio: AudioMode::Silent,
        output: &output,
    };
    let written = render_frames_png(&plan, &frames)?;
    // 字幕画不出来 / 被切 -> 这几张 PNG 里的字幕不对，**不能报成功**。
    // 与 render 的判据同源：都读问题清单，不另设一套。
    //
    // 区间是**整体**判：一段里坏了一帧就退出 1，但那些好帧的账照打 ——
    // 只报第一帧的问题会让"第 47 帧的字幕被切了"看起来像是第 1 帧的事。
    let failed = written.iter().any(|frame| !frame.issues.is_empty());
    let failures: Vec<serde_json::Value> = written
        .iter()
        .filter(|frame| !frame.issues.is_empty())
        .map(|frame| {
            serde_json::json!({
                "frame": frame.frame,
                "path": frame.path.display().to_string(),
                "issues": frame.issues,
            })
        })
        .collect();
    // 单帧那一路的字段**逐字保留**（frame / path / digest / overlay）：
    // 老的调用方（check-cli、任何按一份 JSON 读的人）不该因为这里支持了区间就变。
    // 区间那一路多出来的两个键只在真出多帧时出现 —— 一帧时它们与单帧同义，
    // 写出来只会让"这一趟到底出了几张"这件事有两种读法。
    let single = written.len() == 1;
    let mut body = serde_json::json!({
        "frame": first_frame,
        "path": written.first().map(|f| f.path.display().to_string()),
        "digest": written.first().map(|f| f.digest.clone()),
        "width": width,
        "height": height,
        "project_frames": project_frames(&doc),
        "overlay": written.first().map(|f| f.overlay.clone()),
        "issues": written.first().map(|f| f.issues.clone()).unwrap_or_default(),
        "failed": failed,
    });
    if !single {
        // 摘要：一帧一条，**顺序与请求一致** —— 调用方要拿它去比对
        // "第 N 张是不是我要的那一帧"，乱序会让比对静默错位。
        body["frames"] = serde_json::json!(written
            .iter()
            .map(|frame| serde_json::json!({
                "frame": frame.frame,
                "path": frame.path.display().to_string(),
                "digest": frame.digest,
            }))
            .collect::<Vec<_>>());
        body["count"] = serde_json::json!(written.len());
        body["failures"] = serde_json::json!(failures);
    }
    print_json(&body)?;
    if failed {
        if single {
            eprintln!("这一帧的字幕有问题（见 issues）—— 出图了，但图里的字幕不对。");
        } else {
            eprintln!(
                "这一段里有 {} 帧的字幕有问题（见 failures）—— 图出了，但那些帧的字幕不对。",
                failures.len()
            );
        }
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_render(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("render 要 --project <文件>".to_string()))?;
    let out = args
        .out
        .as_ref()
        .ok_or_else(|| usage_error("render 要 --out <文件.mp4>".to_string()))?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    if let Some(code) = gate(&doc) {
        return Ok(code);
    }
    // 音轨：默认出声音（T6 起），`--no-audio` 可以明确不要。
    // 两种情况下**都要在 stderr 上说一句** —— 无声的片子看起来和有声的一样"成功"，
    // 所以"这一次有没有声音、为什么"必须是打出来的，不能靠人猜。
    let audio_mode = if args.no_audio {
        AudioMode::Silent
    } else {
        AudioMode::Auto
    };
    let audio_tracks = audio_track_ids(&doc.timeline);
    if args.no_audio && !audio_tracks.is_empty() {
        eprintln!(
            "注意：你给了 --no-audio，本次出片**是无声的**（工程里有音轨：{}）。",
            audio_tracks.join(", ")
        );
    }

    let (from, to) = resolve_range(args, &doc)?;
    let (width, height) = resolve_size(args, &doc);
    let root = PathBuf::from(
        args.asset_root
            .clone()
            .unwrap_or_else(|| "target/s3".to_string()),
    );
    let sources = build_sources(&doc, &root, &resolve_fallback(args, &root)?);
    // 粗体与字体目录是可选的：没给就是 None（见 RenderPlan 那两个字段的说明）。
    let font_bold_file = args.font_bold_file.as_deref().map(Path::new);
    let font_dir = args.font_dir.as_deref().map(Path::new);
    // 默认 `1`：不分块。要速度就显式给 `--chunk-workers 0`（自动）或一个具体数。
    let chunk_workers = args.chunk_workers.unwrap_or(1);
    let font_file = match resolve_font(args) {
        Ok(font) => font,
        Err(code) => return Ok(code),
    };
    let subtitles = load_subtitles(&doc, &sources)?;
    if !subtitles.is_empty() && font_file.is_none() {
        // 提前出声：这个组合的结果是**整趟出片判失败**（每一帧都记 subtitle_font_missing），
        // 而等待一趟分钟级的出片之后再看到失败，是最没有用的失败方式。
        eprintln!(
            "工程里有 {} 份字幕素材，却没给 --font-file —— 这一趟会判失败。\
             本仓不内嵌字体、也不猜系统字体。",
            subtitles.len()
        );
    }
    let output = PathBuf::from(out);

    // ---- 侧挂字幕（`--subtitle-out`）----
    //
    // 它**先于出片**落地，理由是它跟出片没有依赖关系：算它要的是时间线、素材表和字幕解析，
    // 既不要 GPU 也不要 ffmpeg。写失败就在开始出片之前退出去，不用等一趟分钟级的活白跑。
    //
    // 它与**烧进画面**是两条独立的路：没给 `--font-file` 时画面上一个字都不会有
    // （那一趟判失败），而侧挂文件照写 —— 侧挂是"这段里说过什么"，不是"像素里有几个字"。
    let sidecar = match sidecar_target(args) {
        Ok(target) => target,
        Err(message) => {
            eprintln!("{message}");
            return Ok(ExitCode::from(2));
        }
    };
    let mut sidecar_written: Option<(PathBuf, usize)> = None;
    if let Some((path, format)) = sidecar {
        let spans = overlay_spans(&doc.timeline, from, to, &subtitles);
        let text = sidecar_text(&spans, from, &doc.timeline.timebase, format)?;
        std::fs::write(&path, &text)
            .map_err(|error| format!("写不了侧挂字幕 {}：{error}", path.display()))?;
        // 事实走 stderr（stdout 只给机器读）：写了几条、来自哪几份素材。
        // 一份侧挂文件只装得下一份素材，所以**用到了几份要明说** ——
        // 两份素材混在一个文件里这件事，只能在写出去的时候讲。
        let mut per_asset: Vec<(String, usize)> = Vec::new();
        for span in &spans {
            match per_asset.iter_mut().find(|(id, _)| *id == span.asset_id) {
                Some((_, count)) => *count += 1,
                None => per_asset.push((span.asset_id.clone(), 1)),
            }
        }
        let summary = per_asset
            .iter()
            .map(|(id, count)| format!("{id} {count} 条"))
            .collect::<Vec<_>>()
            .join("、");
        if spans.is_empty() {
            eprintln!("侧挂字幕：这一段里没有字幕，{} 是空的", path.display());
        } else {
            eprintln!(
                "侧挂字幕：{} 条 -> {}（{}）",
                spans.len(),
                path.display(),
                summary
            );
        }
        sidecar_written = Some((path, spans.len()));
    }

    let total = (to - from + 1) as usize;
    println!(
        "{}",
        serde_json::json!({
            "event": "start",
            "from": from,
            "to": to,
            "total": total,
            "width": width,
            "height": height,
            "assets": sources.len(),
        })
    );

    let asset_timebases = doc.asset_timebases();
    let plan = RenderPlan {
        timeline: &doc.timeline,
        sources: &sources,
        asset_timebases: &asset_timebases,
        from,
        to,
        width,
        height,
        sequence: doc.sequence_size(),
        subtitles: &subtitles,
        font_file,
        font_bold_file,
        font_dir,
        chunk_workers,
        audio: audio_mode,
        output: &output,
    };
    let report = render_plan(&plan, |done, total| {
        // 每帧一行，调用方自己决定要不要节流 —— 这里不替它做决定。
        println!(
            "{}",
            serde_json::json!({"event": "progress", "done": done, "total": total})
        );
    })?;

    let failed = report.failed();
    println!(
        "{}",
        serde_json::json!({
            "event": "done",
            "output": report.output.display().to_string(),
            "frames": report.frames,
            "encoded_frames": report.encoded_frames,
            "width": report.width,
            "height": report.height,
            "fps": report.encoder_fps,
            "seconds": report.seconds,
            "elapsed_ms": report.elapsed_ms,
            "opened_streams": report.opened_streams,
            "empty_frames": report.empty_frames,
            "overlay": report.overlay,
            "decode": report.decode,
            "audio": report.audio,
            "subtitle_out": sidecar_written.as_ref().map(|(path, _)| path.display().to_string()),
            "subtitle_entries": sidecar_written.as_ref().map(|(_, count)| *count),
            "issues": report.issues,
            "failed": failed,
        })
    );
    // 事实与判据分开说：字画了几行/几条、丢了几行/几条是**事实**（走 stderr，stdout 只给机器读），
    // 而"画不出来"与"被切"进的是问题清单 —— 判失败的是后者。
    //
    // 字幕与弹幕**分开报**：两边都会丢东西，但丢的原因与要改的地方不同（改 max_lines 还是加泳道）。
    // 弹幕没有"被切"这一项 —— 滚动中越界是常态，不进判据（见 text_overlay 的模块文档）。
    if !report.overlay.is_silent() {
        eprintln!(
            "文字覆盖层：字幕画了 {} 行、被切 {} 行、丢弃 {} 行（超过 max_lines）、画不出 {} 行；\
             弹幕画了 {} 条、丢弃 {} 条（泳道排不下）、画不出 {} 条；\
             栅格化缓存命中 {} / 未命中 {}（字幕与弹幕共用）",
            report.overlay.lines_drawn,
            report.overlay.lines_clipped,
            report.overlay.lines_dropped,
            report.overlay.lines_failed,
            report.overlay.danmaku_drawn,
            report.overlay.danmaku_dropped,
            report.overlay.danmaku_failed,
            report.overlay.cache_hits,
            report.overlay.cache_misses,
        );
    }
    // 音轨那一路同样：**事实报出来，判据仍是问题清单**。
    // `padded_samples` 单独报 —— 它不为零就说明有素材比它那段短，有内容被截断了。
    if report.audio.segments > 0 {
        eprintln!(
            "音轨：{} 段、{} Hz / {} 声道、共 {} 个采样点（空档补静音 {}、\
             素材不够长补静音 {}）；从素材读了 {} 个采样点",
            report.audio.segments,
            report.audio.sample_rate,
            report.audio.channels,
            report.audio.expected_samples,
            report.audio.gap_samples,
            report.audio.padded_samples,
            report.audio.source_samples_read,
        );
    }
    if report.decode.replays > 0 {
        // **回退是事实，不是错**：工程要求过已经读过去的源内帧，代价是那一路从头再读一遍。
        // 说出来是因为它是"这一趟为什么比那趟慢"的第一个可查的数。
        eprintln!(
            "解码：命中池子 {} 次、向前读 {} 次、**回退重启 {} 次**（共读 {} 帧）。\
             回退多说明片子里有倒着引用同一素材的地方 —— 它不是错，代价是重读。",
            report.decode.hits,
            report.decode.forward,
            report.decode.replays,
            report.decode.frames_read,
        );
    }
    if failed {
        eprintln!("出片报告判定为失败（问题清单或空帧不为空，或帧数对不上）—— 见 done 那一行。");
        return Ok(ExitCode::from(1));
    }
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// 素材库：导入与清点
// ---------------------------------------------------------------------------

/// 按扩展名判素材种类。
///
/// **判不出来就按 video 处理**：ffprobe 会立刻给出答案（拿不到视频流就报错），
/// 比在这里堆一张越来越长的扩展名表可靠。
fn infer_kind(path: &Path) -> AssetKind {
    let extension = path
        .extension()
        .and_then(|text| text.to_str())
        .map(|text| text.to_ascii_lowercase());
    match extension.as_deref() {
        Some("srt") | Some("ass") | Some("ssa") | Some("vtt") => AssetKind::Subtitle,
        Some("wav") | Some("mp3") | Some("aac") | Some("m4a") | Some("flac") => AssetKind::Audio,
        // 动图按扩展名认：这几种**按规范就是多帧容器**。
        Some("gif") | Some("apng") => AssetKind::ImageSequence,
        // **webp 两可**：它既可能是单帧图，也可能是动画 WebP。
        // 扩展名看不出来，所以这里不猜 —— 猜错的代价单向：
        // 把静态图当动图会要求它给 frame_count（那张图本来就没有），
        // 把动图当静态图的表现是"它停在第一帧"，而那是**静默**的。
        //
        // 归静态图，需要动图时由调用方显式改登记表里的 kind（见 README 的登记流程）。
        Some("png") | Some("jpg") | Some("jpeg") | Some("webp") | Some("bmp") => AssetKind::Image,
        _ => AssetKind::Video,
    }
}

/// 把文件路径写成 uri：**在资产根下面就用相对路径，否则原样写**。
///
/// 不硬塞一个假的相对路径：那是把"这个素材不归我管"这件事藏起来，
/// 而藏起来的结果是换台机器就找不到素材、且没人知道为什么。
fn relativize_uri(path: &Path, asset_root: &Path) -> String {
    let normalized = |value: &Path| value.to_string_lossy().replace('\\', "/");
    let file = normalized(path);
    let root = normalized(asset_root);
    let root = root.trim_end_matches('/');
    let prefix = format!("{root}/");
    match file.strip_prefix(&prefix) {
        Some(rest) => rest.to_string(),
        None => file,
    }
}

fn cmd_import(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("import 要 --project <文件>".to_string()))?;
    let file = args
        .file
        .as_ref()
        .ok_or_else(|| usage_error("import 要 --file <素材>".to_string()))?;
    let path = Path::new(file);
    if !path.exists() {
        eprintln!("文件不在：{file}");
        return Ok(ExitCode::from(2));
    }
    let asset_root = PathBuf::from(
        args.asset_root
            .clone()
            .unwrap_or_else(|| "target/s3".to_string()),
    );
    let id = match args.id.clone() {
        Some(explicit) => explicit,
        None => path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .ok_or_else(|| {
                usage_error("这个路径没有文件名，请用 --id 指定".to_string())
            })?,
    };
    let kind = infer_kind(path);
    let mut built = Asset {
        id: id.clone(),
        kind,
        name: id.clone(),
        uri: relativize_uri(path, &asset_root),
        frame_count: None,
        timebase: None,
        width: None,
        height: None,
        content_hash: None,
        tags: std::collections::BTreeMap::new(),
        note: String::new(),
    };
    // 只有视频流才有尺寸与帧数 —— 字幕/音频/图片问了也是白问。
    if kind == AssetKind::Video {
        let info = asset_info(path)?;
        built.frame_count = Some(info.frame_count);
        built.timebase = Some(info.timebase.clone());
        built.width = Some(info.width);
        built.height = Some(info.height);
    }

    let mut doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    match doc.assets.iter().position(|asset| asset.id == id) {
        Some(index) => {
            if !args.replace {
                eprintln!("资产 id 已存在：{id}（要覆盖就加 --replace）");
                return Ok(ExitCode::from(2));
            }
            doc.assets[index] = built.clone();
        }
        None => doc.assets.push(built.clone()),
    }

    let issues = validate_project_doc(&doc, REGISTRY);
    if args.write {
        // 写回：pretty + 结尾换行。**migrated_from 是 serde(skip) 的，不会进文件。**
        let text = serde_json::to_string_pretty(&doc).map_err(|error| error.to_string())?;
        std::fs::write(project, format!("{text}\n"))
            .map_err(|error| format!("写不回工程 {project}：{error}"))?;
    }
    print_json(&serde_json::json!({
        "asset": built,
        "written": args.write,
        "asset_count": doc.assets.len(),
        "issues": issues,
    }))?;
    Ok(if issues.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

fn cmd_library(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("library 要 --project <文件>".to_string()))?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let counts = asset_reference_counts(&doc);
    let mut rows = Vec::with_capacity(doc.assets.len());
    let mut unused = Vec::new();
    for (index, asset) in doc.assets.iter().enumerate() {
        let references = counts.get(&asset.id).copied().unwrap_or(0);
        if references == 0 && !asset.id.is_empty() {
            unused.push(asset.id.clone());
        }
        rows.push(serde_json::json!({
            "index": index,
            "id": asset.id,
            "kind": asset.kind,
            "uri": asset.uri,
            "frame_count": asset.frame_count,
            "timebase": asset.timebase,
            "width": asset.width,
            "height": asset.height,
            "references": references,
        }));
    }
    print_json(&serde_json::json!({
        "total": doc.assets.len(),
        "used": doc.assets.len() - unused.len(),
        // **未被引用的列出来**：它们是 unused_asset 警告的来源，
        // 而"库里有 12 条"和"库里 12 条里有 3 条没人用"是两件事。
        "unused": unused,
        "assets": rows,
    }))
}

/// 字幕素材 id -> 已解析的字幕条：**读文件 + 解析**。
///
/// 三条命令（subtitle / frame / render）共用它。各写一遍的话，就会出现
/// 「subtitle 说这一帧有字、render 画不出来」这种最难查的分叉 ——
/// 而两端结构一致正是 T2 全部工作的目的。
///
/// 读不了、解析不了都**报错**，不静默当成「这部片子没有字幕」：
/// 那两种情况的输出一模一样，而后者是「看起来成功、其实不对」的典型。
///
/// 引用不存在的素材、或者引到非字幕素材，都不在这里管 ——
/// 那些是 `validate_project_doc` 的 error（unknown_asset / subtitle_asset_kind），
/// 在命令开头就被 gate 拦掉了。这里再查一遍就是第二份实现。
fn load_subtitles(doc: &ProjectDoc, sources: &SourceTable) -> Result<SubtitleTable, String> {
    let mut table = SubtitleTable::new();
    let mut unreadable: Vec<String> = Vec::new();
    for asset in &doc.assets {
        if asset.kind != AssetKind::Subtitle {
            continue;
        }
        let Some(file) = sources.file_for(&asset.id) else {
            unreadable.push(format!("{}：没有登记文件位置", asset.id));
            continue;
        };
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(error) => {
                unreadable.push(format!(
                    "{}：读不了 {}（{error}）",
                    asset.id,
                    file.display()
                ));
                continue;
            }
        };
        let extension = file
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let parsed = match extension.as_str() {
            "srt" => parse_srt(&text),
            "ass" | "ssa" => parse_ass(&text),
            other => Err(format!(
                "不认得这个字幕格式：{other}（现在只认 srt/ass/ssa）"
            )),
        };
        match parsed {
            Ok(report) => {
                if report.skipped > 0 {
                    eprintln!("{}：跳过了 {} 个解析不了的块", asset.id, report.skipped);
                }
                table.insert(asset.id.clone(), report.cues);
            }
            Err(error) => return Err(format!("{} 解析失败：{error}", asset.id)),
        }
    }
    if !unreadable.is_empty() {
        return Err(format!("字幕素材读不了：{}", unreadable.join("；")));
    }
    Ok(table)
}

/// 字体文件：没给就是**没有字体**（见 `RenderPlan::font_file` 的说明）。
///
/// 给了但指不到文件就报**用法错**（退出码 2）：把路径打错字的人应该马上看到这句话，
/// 而不是等着看「每一行字幕都画不出来」的清单 —— 那是同一个错，但难查得多。
fn resolve_font(args: &Args) -> Result<Option<&Path>, ExitCode> {
    let Some(text) = args.font_file.as_deref() else {
        return Ok(None);
    };
    let file = Path::new(text);
    if !file.is_file() {
        eprintln!("--font-file 指不到一个文件：{text}");
        return Err(ExitCode::from(2));
    }
    Ok(Some(file))
}

/// 侧挂字幕文件的**落点与格式**：`--subtitle-out` 与 `--format` 一起决定。
///
/// **纯函数**（只读 args，不碰盘），所以能脱离命令行单测。
///
/// 格式的判定顺序是「明说的优先，没明说就看扩展名」：
///   * 两边都给、而且**打架**（`--format srt` 却写 `x.ass`）-> 报用法错。
///     那是"文件名叫 ASS、里面是 SRT"的典型，而它在播放器里看起来像"ASS 解析失败"；
///   * 扩展名认不出来又没给 `--format` -> 报用法错。**不猜**：猜错的方向正好是上面那一种。
fn sidecar_target(args: &Args) -> Result<Option<(PathBuf, SidecarFormat)>, String> {
    let Some(text) = args.subtitle_out.as_deref() else {
        // 没要侧挂文件。`--format` 单独出现已经在解析阶段被拦下了。
        return Ok(None);
    };
    let path = PathBuf::from(text);
    match (SidecarFormat::from_extension(&path), args.format) {
        (Some(from_name), None) => Ok(Some((path, from_name))),
        (None, Some(asked)) => Ok(Some((path, asked))),
        (Some(from_name), Some(asked)) if from_name == asked => Ok(Some((path, asked))),
        (Some(from_name), Some(asked)) => Err(format!(
            "{text} 的扩展名说这是 {}，--format 却说要写 {} —— 两个里得改一个：\
             扩展名与内容对不上，播放器打开时只会说解析失败",
            from_name.name(),
            asked.name()
        )),
        (None, None) => Err(format!(
            "从 {text} 看不出要写哪种字幕：--subtitle-out 的扩展名不是 .srt / .ass / .ssa，\
             请用 --format 明说"
        )),
    }
}

/// 把侧挂条目排成一份文件内容。**纯函数**：不碰盘、不读时钟，所以能单测。
///
/// 时间**换算回毫秒并重定基到这一趟出的片子**（`base` 是产物的第 0 帧）：
/// 侧挂文件是给这段产物用的，从 0 起算才是播放器会看到的东西。
///
/// 条目按起点排序：顺序乱的 SRT 有的播放器直接当坏文件。
fn sidecar_text(
    spans: &[OverlaySpan],
    base: Frame,
    timebase: &TimebaseDto,
    format: SidecarFormat,
) -> Result<String, String> {
    let broken = || format!("时间基坏掉（{}/{}），算不出侧挂字幕的时间", timebase.num, timebase.den);
    let base_ms = ms_at_frame(base, timebase).ok_or_else(broken)?;
    let mut cues: Vec<Cue> = Vec::with_capacity(spans.len());
    for span in spans {
        let start = ms_at_frame(span.first, timebase).ok_or_else(broken)?;
        // 终点是**最后一帧的下一个起点**：SRT 的结束时间是"什么时候消失"。
        let end = ms_at_frame(span.last.saturating_add(1), timebase).ok_or_else(broken)?;
        let start = u64::try_from(start - base_ms)
            .map_err(|_| format!("侧挂字幕的时间算出来是负的（第 {} 帧）", span.first))?;
        let mut end = u64::try_from(end - base_ms)
            .map_err(|_| format!("侧挂字幕的时间算出来是负的（第 {} 帧）", span.last))?;
        // SRT / ASS 的时间精度是 1ms。比 1ms 还短的一帧在这里没法精确表示 ——
        // 至少不能让终点落在起点之前：那样的条目有的播放器直接丢掉。
        if end <= start {
            end = start + 1;
        }
        cues.push(Cue {
            start_ms: start,
            end_ms: end,
            text: span.text.clone(),
            // 侧挂 SRT 没有 travel（那是 ASS 的 `\move` 专有）。
            travel_ms: None,
            // 侧挂文件只带文本与时间：源里的加粗/斜体/颜色不进这里（`to_srt` 也没有地方放）。
            style: CueStyle::default(),
        });
    }
    cues.sort_by_key(|cue| (cue.start_ms, cue.end_ms));
    Ok(match format {
        SidecarFormat::Srt => to_srt(&cues),
        SidecarFormat::Ass => to_ass(&cues, &AssStyle::default()),
    })
}

/// 打印某一帧的**文字覆盖层**：要画哪几行字幕、几条弹幕，各占哪个归一化矩形。
///
/// 两个作用：
///   * 给「两端要画的那份结构」一个可以逐字段核对的参照 ——
///     宿主的输出与它不一致，就是宿主错了；
///   * 让 core 的文字评估**一出生就有调用方**。只写不用的公共 API 比没有更容易误导。
///
/// 它**不画图**：栅格化是宿主的事，所以这里不需要 GPU，也不需要 ffmpeg。
///
/// # 弹幕与字幕分两个键，为什么
///
/// 两者的**落点规则不同**：字幕的每一行居中于整条目标宽，弹幕按自己的宽度
/// 左对齐、位置是帧的函数（见 `dhampir-timeline::danmaku::rect_at`）。混进一个数组
/// 就得在每个元素上带一个种类标签，而那与"这里是纯结构"的定位冲突。
///
/// 键名与预览宿主的 `dhampir_project_text_frame`、
/// `dhampir-timeline::host_api::OverlayView` **逐字段同名**：
/// `items[{text,rect}]`、`danmaku[{text,rect,lane,enter,exit}]`、`color`、`outline`、
/// `dropped_lines`、`dropped_danmaku`、`subtitle_assets` —— 三处同名，
/// 比对时不需要一张映射表（映射表自己会漂）。
///
/// # 弹幕的 ASS 导出不在这里（明写的边界）
///
/// `danmaku::to_ass_danmaku`（带 `\move`）已经备好并有单测，但**没有接进
/// `--subtitle-out`**：一份 ASS 只有一个 `Style`，字幕字号来自 `AssStyle::font_size`、
/// 弹幕字号来自 `font_ratio × 序列高`，合进同一个文件要么改共享的 `ass_header`
/// （牵动 `to_ass` 与两端），要么再添一个旗标。而 T3 的验收只要求**结构一致**
/// （同一输入两端给出相同的 text / 泳道 / 进出帧、丢弃数一致），
/// 所以这里先记为边界，不顺手扩契约。
fn cmd_subtitle(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("subtitle 要 --project <文件>".to_string()))?;
    let frame = args
        .frame
        .ok_or_else(|| usage_error("subtitle 要 --frame <帧号>".to_string()))?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let asset_root = PathBuf::from(
        args.asset_root
            .clone()
            .unwrap_or_else(|| "target/s3".to_string()),
    );
    let fallback = resolve_fallback(args, &asset_root)?;
    let sources = build_sources(&doc, &asset_root, &fallback);
    let table = load_subtitles(&doc, &sources)?;

    let sequence = doc.sequence_size();
    let overlay = evaluate_overlay(&doc.timeline, frame, sequence, Some(&table));
    let (items, danmaku, subtitle_style, danmaku_style, dropped_lines, dropped_danmaku) = match overlay {
        Some(overlay) => (
            overlay.items.iter().map(text_item_json).collect::<Vec<_>>(),
            overlay.danmaku.iter().map(danmaku_item_json).collect::<Vec<_>>(),
            text_style_json(&overlay.subtitle_style),
            text_style_json(&overlay.danmaku_style),
            overlay.dropped_lines,
            overlay.dropped_danmaku,
        ),
        None => (
            Vec::new(),
            Vec::new(),
            serde_json::Value::Null,
            serde_json::Value::Null,
            0,
            0,
        ),
    };

    print_json(&serde_json::json!({
        "frame": frame,
        "sequence": [sequence.0, sequence.1],
        "subtitle_assets": table.len(),
        "items": items,
        "danmaku": danmaku,
        // **两类各一套画法**（以前是一份共用的，见 `TextStyle` 的说明）。
        // 逐字段同名于 `host_api::TextStyleView` 与 wasm 的 `dhampir_project_text_frame`。
        "subtitle_style": subtitle_style,
        "danmaku_style": danmaku_style,
        "dropped_lines": dropped_lines,
        "dropped_danmaku": dropped_danmaku,
    }))?;
    Ok(ExitCode::SUCCESS)
}

/// `{color, outline, stroke_px, stroke_color}` —— 三处同名。
fn text_style_json(style: &dhampir_core::overlay::TextStyle) -> serde_json::Value {
    serde_json::json!({
        "color": style.color,
        "outline": style.outline,
        "stroke_px": style.stroke_px,
        "stroke_color": style.stroke_color,
    })
}

/// `{text, rect}` —— 与预览宿主的 `text_item_json`、`host_api::TextItemView` 同一形状。
///
/// **三处逐字段同名**（见 `dhampir-timeline::host_api::OverlayView` 的说明），
/// 所以这里不写"另一个字段名"：比对两端靠的就是这一份同名，映射表自己会漂。
fn text_item_json(item: &dhampir_core::overlay::TextItem) -> serde_json::Value {
    serde_json::json!({
        "text": item.text,
        "rect": {
            "x": item.rect.x,
            "y": item.rect.y,
            "width": item.rect.width,
            "height": item.rect.height,
        },
        // 淡入淡出：两端都要能对账"这一帧多透明、偏了多少"。
        "opacity": item.opacity,
        "dy_px": item.dy_px,
        // **逐条颜色**：同一轨里不同的条可以不一样（ASS 的 `\c`）。
        "color": item.color,
        // 字号与**这一条被缩了多少**。JS 侧栅格化要用它们：
        // 字号决定 `ctx.font`，缩放决定**描边宽度**（参照 `swEff = sw * scale`）。
        "font_ratio": item.font_ratio,
        "scale": item.scale,
        // **高亮分段**（`.hl`），颜色已由求值层解析好。
        // 空数组 = 没有标记，宿主走"一次画完"的老路（既有工程逐字节不变）。
        "parts": item
            .parts
            .iter()
            .map(|r| serde_json::json!({ "text": r.text, "color": r.color }))
            .collect::<Vec<_>>(),
    })
}

/// `{text, rect, lane, enter, exit}` —— 与预览宿主、`host_api::DanmakuItemView` 同一形状。
///
/// `lane` / `enter` / `exit` **必须给**：只比矩形的话，「泳道被分配错了」（两条换了位置）
/// 在单帧里可能完全看不出来 —— 而那正是两端最容易漂的地方。
///
/// 这里的 `rect` 是**这一帧**的滚动位置（`danmaku::rect_at` 是时间的函数），
/// 不是像字幕那样的固定居中矩形。要"这条弹幕最终停在哪个泳道、活在哪几帧"，
/// 看 `lane` / `enter` / `exit` 三个字段。
fn danmaku_item_json(item: &dhampir_core::overlay::DanmakuTextItem) -> serde_json::Value {
    let mut value = text_item_json(&dhampir_core::overlay::TextItem {
        text: item.text.clone(),
        rect: item.rect,
        opacity: item.opacity,
        dy_px: item.dy_px,
        color: item.color,
        font_ratio: item.font_ratio,
        // 弹幕不缩字（参照的弹幕没有缩字逻辑）；这里是从弹幕条目转出来的。
        scale: 1.0,
        // 弹幕没有 `.hl` 标记（参照的弹幕路径也不解析它）。
        parts: Vec::new(),
    });
    value["lane"] = serde_json::json!(item.lane);
    value["enter"] = serde_json::json!(item.enter);
    value["exit"] = serde_json::json!(item.exit);
    // **逐条的滚动时长**：`rect` 是它的函数，两端要对账就得看得见它。
    value["travel_frames"] = serde_json::json!(item.travel_frames);
    value
}

/// 历史层的默认上限（**条**）。一条是一份整份快照，所以这个数别往上开。
const HISTORY_CAP: usize = 64;

/// 历史文件不在 = **空历史**，不是错误：第一次编辑时它本来就不存在。
fn load_history(path: &str) -> Result<History, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("历史文件 {path} 不是合法的历史：{error}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(History::new(HISTORY_CAP))
        }
        Err(error) => Err(format!("读不了历史文件 {path}：{error}")),
    }
}

fn save_history(path: &str, history: &History) -> Result<(), String> {
    let text = serde_json::to_string_pretty(history).map_err(|error| error.to_string())?;
    std::fs::write(path, format!("{text}\n"))
        .map_err(|error| format!("写不回历史文件 {path}：{error}"))
}

fn cmd_edit(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| {
            usage_error(format!("{} 要 --project <文件>", command_name(args)))
        })?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    if args.undo || args.redo {
        return cmd_edit_history(args, project, doc);
    }
    let Some(op_text) = args.op.as_ref() else {
        eprintln!("edit 要 --op <JSON>，或者 --undo / --redo（配 --history <文件>）");
        return Ok(ExitCode::from(2));
    };
    let op: EditOp = match serde_json::from_str(op_text) {
        Ok(op) => op,
        Err(error) => {
            eprintln!("--op 不是合法的编辑操作：{error}");
            return Ok(ExitCode::from(2));
        }
    };
    record_edit(args, project, doc, &op)
}

/// 日志与报错里该把这个调用叫成什么。**`undo` 报错时不该说"edit 要…"** ——
/// 用户敲的是哪个词，就回哪个词。
fn command_name(args: &Args) -> &str {
    if args.command.is_empty() {
        "edit"
    } else {
        args.command.as_str()
    }
}

/// 落盘这一步：**先历史、后工程**。
///
/// 顺序反过来的话，历史写失败会留下「编辑已经落盘、却退不回去」——
/// 而用户看到的是一条错误，会以为没改。`record_edit` 与 `cmd_batch` 共用这一处，
/// 所以这条顺序只有一份实现。
fn commit_edit(
    args: &Args,
    project: &str,
    before: &ProjectDoc,
    after: &ProjectDoc,
    label: &str,
) -> Result<(), String> {
    if let Some(history_path) = args.history.as_ref() {
        let mut history = load_history(history_path)?;
        history.push(label.to_string(), before.clone());
        save_history(history_path, &history)?;
    }
    let text = serde_json::to_string_pretty(after).map_err(|error| error.to_string())?;
    std::fs::write(project, format!("{text}\n"))
        .map_err(|error| format!("写不回工程 {project}：{error}"))?;
    Ok(())
}

/// **一次操作的唯一落点**：算、落盘、打印结果。
///
/// `edit --op` 与具名子命令 `clip` / `sequence` 都走这里 ——
/// 「同一实现的糖」这句验收要求说的就是这件事，而且它是**可测**的：
/// 具名写法与等价的 `edit --op` 给出的**工程字节**与 **stdout JSON** 逐字节相同。
fn record_edit(
    args: &Args,
    project: &str,
    doc: ProjectDoc,
    op: &EditOp,
) -> Result<ExitCode, CommandError> {
    let outcome = apply_edit(&doc, REGISTRY, op);
    if args.write && outcome.is_ok() {
        let label = if outcome.summary.is_empty() {
            "编辑".to_string()
        } else {
            outcome.summary.clone()
        };
        commit_edit(args, project, &doc, &outcome.doc, &label)?;
    }
    print_json(&serde_json::json!({
        "ok": outcome.is_ok(),
        "summary": outcome.summary,
        "written": args.write && outcome.is_ok(),
        "issues": outcome.issues,
    }))?;
    Ok(if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}

/// 把具名动作的开关拼成**同一个** `EditOp`。
///
/// 全部字段在解析阶段已经过一遍（要哪些、认哪些），所以这里不再报用法错 ——
/// 从这里报错会变成退出码 1，而"开关给错了"是退出码 2 的事。
/// 两边万一对不上，`expect` 会当场炸掉，**不会静静用默认值跑完**
/// （那正是最坏的结果：一次看着成功的编辑，内容却不是用户要的）。
fn build_named_op(args: &Args) -> EditOp {
    let op = args.op_name.as_deref().unwrap_or_default();
    match op {
        "insert" => EditOp::Insert {
            track: args.track.clone().expect("--track 解析阶段就要求了"),
            asset: args.asset.clone().expect("--asset 解析阶段就要求了"),
            at: args.at.expect("--at 解析阶段就要求了"),
            source_in: args.source_in.expect("--source-in 解析阶段就要求了"),
            length: args.length.expect("--length 解析阶段就要求了"),
            id: args.id.clone(),
        },
        "trim" => EditOp::Trim {
            layer: args.layer.clone().expect("--layer 解析阶段就要求了"),
            edge: args.edge.expect("--edge 解析阶段就要求了"),
            to: args.to.expect("--to 解析阶段就要求了"),
        },
        "split" => EditOp::Split {
            layer: args.layer.clone().expect("--layer 解析阶段就要求了"),
            at: args.at.expect("--at 解析阶段就要求了"),
        },
        "move" => EditOp::Move {
            layer: args.layer.clone().expect("--layer 解析阶段就要求了"),
            to: args.to.expect("--to 解析阶段就要求了"),
            track: args.track.clone(),
        },
        "remove" => EditOp::Remove {
            layer: args.layer.clone().expect("--layer 解析阶段就要求了"),
            ripple: args.ripple,
        },
        "set" => EditOp::SetSequence {
            timebase: args.timebase.clone().expect("--timebase 解析阶段就要求了"),
            width: args.width.unwrap_or(0),
            height: args.height.unwrap_or(0),
        },
        other => unreachable!("{other} 在解析阶段就被挡掉了"),
    }
}

/// `clip` / `sequence`：把开关拼成 op，然后走**同一条** `record_edit`。
fn cmd_named_op(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| {
            usage_error(format!("{} 要 --project <文件>", command_name(args)))
        })?;
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let op = build_named_op(args);
    record_edit(args, project, doc, &op)
}

/// `undo` / `redo`：**连函数都不换** —— 把旗标设上再调 `cmd_edit`。
///
/// 这是"糖"最诚实的形态：想有一处行为不同都不可能，因为根本没有第二份实现。
fn cmd_named_history(args: &Args) -> Result<ExitCode, CommandError> {
    cmd_edit(&history_alias(args))
}

/// 把具名 `undo` / `redo` 折成 `edit` 那一组旗标。
///
/// **抽成纯函数是为了能测**：具名写法折出来的 `Args` 必须与
/// `edit --undo` / `edit --redo` 解析出来的 `Args` **逐字段相同**。
/// 留在这里当私有实现的话，"同一实现的糖"就只是一句注释。
fn history_alias(args: &Args) -> Args {
    let mut local = args.clone();
    local.undo = args.command == "undo";
    local.redo = args.command == "redo";
    local.command = "edit".to_string();
    local
}

/// `batch`：把脚本里的 op **一个接一个**走同一条 `apply`。
///
/// 语义是**一次写、一条历史**，不是一个"循环调 N 次 edit"的宏：
///
/// * 中途有一步不成立就**整份不落盘**，并明说**卡在第几行** ——
///   否则用户只能靠 diff 猜是哪一步的问题；
/// * 空脚本**不算成功**：它会打印 ok 却什么都没做，与"脚本路径写错了"分不开。
fn cmd_batch(args: &Args) -> Result<ExitCode, CommandError> {
    let project = args
        .project
        .as_ref()
        .ok_or_else(|| usage_error("batch 要 --project <文件>".to_string()))?;
    let script = args.script.as_ref().expect("--script 解析阶段就要求了");
    let doc = match load_project_or_usage(project) {
        Ok(doc) => doc,
        Err(code) => return Ok(code),
    };
    let text =
        std::fs::read_to_string(script).map_err(|error| format!("读不了脚本 {script}：{error}"))?;
    let mut ops: Vec<(usize, EditOp)> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        // 空行与 `#` 开头是注释：脚本是给人写的，得能写注释。
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let op: EditOp = serde_json::from_str(trimmed)
            .map_err(|error| format!("脚本第 {} 行不是合法的编辑操作：{error}", index + 1))?;
        ops.push((index + 1, op));
    }
    if ops.is_empty() {
        eprintln!("脚本 {script} 里一个操作都没有（空行与 # 注释不算）");
        return Ok(ExitCode::from(2));
    }
    // 留一份**批处理之前**的原样：它是历史里那一步的 before。
    let mut current = doc.clone();
    let mut summaries: Vec<String> = Vec::new();
    for (line, op) in &ops {
        let outcome = apply_edit(&current, REGISTRY, op);
        if !outcome.is_ok() {
            let issues: Vec<serde_json::Value> = outcome
                .issues
                .iter()
                .map(|issue| {
                    serde_json::json!({
                        "code": issue.code,
                        "path": issue.path,
                        "message": format!("第 {line} 行：{}", issue.message),
                    })
                })
                .collect();
            print_json(&serde_json::json!({
                "ok": false,
                "summary": format!("批处理在第 {line} 行停下"),
                "written": false,
                "steps": ops.len(),
                "issues": issues,
            }))?;
            return Ok(ExitCode::from(2));
        }
        if !outcome.summary.is_empty() {
            summaries.push(outcome.summary.clone());
        }
        current = outcome.doc;
    }
    let label = format!("批处理：{} 步", ops.len());
    if args.write {
        commit_edit(args, project, &doc, &current, &label)?;
    }
    print_json(&serde_json::json!({
        "ok": true,
        "summary": format!("{label}（{}）", summaries.join("；")),
        "written": args.write,
        "steps": ops.len(),
        "issues": [],
    }))?;
    Ok(ExitCode::SUCCESS)
}

/// 撤销 / 重做：从历史文件里退一步或进一步。
///
/// **不给 `--write` 就是干跑** —— 连历史文件都不碰（干跑不许在磁盘上留下任何痕迹）。
fn cmd_edit_history(args: &Args, project: &str, doc: ProjectDoc) -> Result<ExitCode, CommandError> {
    let history_path = args
        .history
        .as_ref()
        .expect("--undo / --redo 一定带 --history（解析阶段就拦了）");
    let mut history = load_history(history_path)?;
    let restored = if args.undo {
        history.undo(doc)
    } else {
        history.redo(doc)
    };
    let Some(snapshot) = restored else {
        let (code, message) = if args.undo {
            ("nothing_to_undo", "没有可撤销的步骤")
        } else {
            ("nothing_to_redo", "没有可重做的步骤")
        };
        // **静默什么都不做比报错难查得多**：退 2 并说清是哪一边空了。
        print_json(&serde_json::json!({
            "ok": false,
            "summary": "",
            "written": false,
            "issues": [{ "code": code, "path": "history", "message": message }],
        }))?;
        return Ok(ExitCode::from(2));
    };
    if args.write {
        save_history(history_path, &history)?;
        let text = serde_json::to_string_pretty(&snapshot.doc).map_err(|error| error.to_string())?;
        std::fs::write(project, format!("{text}\n"))
            .map_err(|error| format!("写不回工程 {project}：{error}"))?;
    }
    print_json(&serde_json::json!({
        "ok": true,
        "summary": format!("{}：{}", if args.undo { "撤销" } else { "重做" }, snapshot.label),
        "written": args.write,
        "issues": [],
    }))?;
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse(&argv) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            eprintln!();
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if args.help || args.command.is_empty() {
        println!("{USAGE}");
        // 没给子命令**不是**错误：dhampir 与 dhampir --help 一样是"问怎么用"。
        return ExitCode::SUCCESS;
    }
    let result = match args.command.as_str() {
        "probe" => cmd_probe(&args),
        "info" => cmd_info(&args),
        "gop" => cmd_gop(&args),
        "frame" => cmd_frame(&args),
        "render" => cmd_render(&args),
        "import" => cmd_import(&args),
        "library" => cmd_library(&args),
        "edit" => cmd_edit(&args),
        "subtitle" => cmd_subtitle(&args),
        "undo" | "redo" => cmd_named_history(&args),
        "clip" | "sequence" => cmd_named_op(&args),
        "batch" => cmd_batch(&args),
        other => Err(CommandError::Usage(usage_error(format!(
            "不认识的子命令：{other}"
        )))),
    };
    // **退出码由类型决定，不由文案决定。**
    //
    // 这一段从前无条件退 1，于是「忘给 --project」与「GPU 拿不到上下文」
    // 在调用方眼里长得一模一样 —— 而本机后端正是靠 0 / 2 区分
    // 「成功」与「用户能改的错」。混淆的方向是坏的：调用方会去查渲染管线，
    // 而真正该做的是补上那个开关。
    //
    // 所以 `Err` 两侧是不同的类型（见 [`CommandError`]）：用法错带的是
    // [`Usage`]，运行期失败带的是裸 `String`。想混都混不了。
    match result {
        Ok(code) => code,
        Err(error) => error.code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn 子命令与选项能被解析出来() {
        let args = parse(&argv(&[
            "render",
            "--project",
            "p.json",
            "--from",
            "0",
            "--to",
            "9",
            "--out",
            "o.mp4",
        ]))
        .expect("合法");
        assert_eq!(args.command, "render");
        assert_eq!(args.project.as_deref(), Some("p.json"));
        assert_eq!(args.from, Some(0));
        assert_eq!(args.to, Some(9));
        assert_eq!(args.out.as_deref(), Some("o.mp4"));
    }

    #[test]
    fn 不认识的选项要报错而不是静默丢掉() {
        // 这条正是「参数打错字被静默忽略」的防线。
        let error = parse(&argv(&["render", "--wdith", "640"])).expect_err("应当报错");
        assert!(error.contains("--wdith"), "{error}");
    }

    #[test]
    fn 音频默认出声音_除非明确说不要() {
        // 默认（不给标志）= Auto：**不能**默认成静音 ——
        // 默认静音会让"工程里有音轨"这件事静默地不起作用。
        let args = parse(&argv(&[
            "render", "--project", "p.json", "--to", "9", "--out", "o.mp4",
        ]))
        .expect("合法");
        assert!(!args.no_audio);
        // `--no-audio` 要认，而且要认成"不要声音"，不是"没给"。
        let muted = parse(&argv(&[
            "render", "--project", "p.json", "--to", "9", "--out", "o.mp4", "--no-audio",
        ]))
        .expect("合法");
        assert!(muted.no_audio);
        // 顺序无关。
        let front = parse(&argv(&[
            "--no-audio", "render", "--project", "p.json", "--to", "9", "--out", "o.mp4",
        ]))
        .expect("合法");
        assert!(front.no_audio);
        assert_eq!(front.command, "render");
    }

    #[test]
    fn 音频标志的近亲拼法不许被当成同一个() {
        // 拼错一个字母必须报错：静默接受会让人以为"已经静音了"，而片子照旧有声。
        for wrong in ["--noaudio", "--no_audio", "--noaudio-mode"] {
            let error = parse(&argv(&["render", wrong])).expect_err("应当报错");
            assert!(error.contains(wrong), "{error}");
        }
    }

    #[test]
    fn 不认识的子命令要报错() {
        let error = parse(&argv(&["transcode"])).expect_err("应当报错");
        assert!(error.contains("transcode"), "{error}");
    }

    #[test]
    fn 选项少给值要报错() {
        let error = parse(&argv(&["probe", "--project"])).expect_err("应当报错");
        assert!(error.contains("要跟一个值"), "{error}");
    }

    #[test]
    fn 多给一个位置参数要报错() {
        let error = parse(&argv(&["probe", "extra.json"])).expect_err("应当报错");
        assert!(error.contains("多余的位置参数"), "{error}");
    }

    #[test]
    fn 带值选项表里每一条都真的被处理() {
        // 表里列了却不处理的选项会落到 other 分支报错 —— 而它已经在表里，
        // 说明有人加了选项却没写分支。这条把它变成红。
        //
        // 探针必须**挑对子命令**：有些选项只有某一组认
        // （--history 归编辑那一组、--subtitle-out 归 render、--track 归 clip）。
        // 一律拿 `edit` 探会把「限制」误报成「没处理」——那段注释当年就是这么写的，
        // 直到 T7 把 `--layer` 这类选项加进来，它才真的不够用了。
        //
        // 所以这里是一张**显式**的"谁认它 + 一条完整的合法命令行"的表。
        // 它必须盖住整张 KNOWN_VALUE_FLAGS：新加一个选项却不在这儿声明谁认它、
        // 怎么用，这条就红。**这比原来那版强**——原来那版只需要选项能被 `edit` 收下。
        let probe: [(&str, &[&str]); 25] = [
            ("--project", &["probe", "--project", "1"]),
            ("--asset", &["info", "--asset", "1"]),
            ("--out", &["render", "--out", "1"]),
            ("--asset-root", &["probe", "--asset-root", "1"]),
            ("--asset-map", &["probe", "--asset-map", "1"]),
            ("--file", &["import", "--file", "1"]),
            ("--id", &["import", "--id", "1"]),
            ("--op", &["edit", "--op", "1"]),
            ("--history", &["edit", "--history", "1"]),
            ("--from", &["render", "--from", "1"]),
            ("--to", &["render", "--to", "1"]),
            ("--width", &["render", "--width", "1"]),
            ("--height", &["render", "--height", "1"]),
            ("--font-file", &["frame", "--font-file", "1"]),
            ("--font-bold-file", &["frame", "--font-bold-file", "1"]),
            ("--font-dir", &["frame", "--font-dir", "1"]),
            ("--chunk-workers", &["render", "--chunk-workers", "1"]),
            (
                "--track",
                &[
                    "clip", "insert", "--track", "1", "--asset", "a", "--at", "1",
                    "--source-in", "0", "--length", "1",
                ],
            ),
            ("--layer", &["clip", "split", "--layer", "1", "--at", "1"]),
            ("--at", &["clip", "split", "--layer", "1", "--at", "1"]),
            (
                "--source-in",
                &[
                    "clip", "insert", "--track", "1", "--asset", "a", "--at", "1",
                    "--source-in", "0", "--length", "1",
                ],
            ),
            (
                "--length",
                &[
                    "clip", "insert", "--track", "1", "--asset", "a", "--at", "1",
                    "--source-in", "0", "--length", "1",
                ],
            ),
            ("--edge", &["clip", "trim", "--layer", "1", "--edge", "in", "--to", "1"]),
            ("--timebase", &["sequence", "set", "--timebase", "30"]),
            ("--script", &["batch", "--script", "1"]),
        ];
        assert_eq!(
            probe.len(),
            KNOWN_VALUE_FLAGS.len(),
            "探针表要与 KNOWN_VALUE_FLAGS 一一对应"
        );
        for flag in KNOWN_VALUE_FLAGS {
            let (_, line) = probe
                .iter()
                .find(|(name, _)| *name == flag)
                .unwrap_or_else(|| panic!("表里的选项 {flag} 没说谁认它"));
            let parsed = parse(&argv(line))
                .unwrap_or_else(|error| panic!("表里的选项 {flag} 解析不过：{error}"));
            assert!(!parsed.command.is_empty());
        }
    }

    #[test]
    fn 撤销重做那一组只有_edit_认() {
        // 口径与侧挂那两条一样：别的子命令**静默收下就是参数被丢掉**。
        for argv_line in [
            vec!["render", "--undo", "--history", "h.json"],
            vec!["frame", "--redo", "--history", "h.json"],
            vec!["edit", "--undo", "--history", "h.json"],
        ] {
            let result = parse(&argv(&argv_line));
            if argv_line[0] == "edit" {
                assert!(result.is_ok(), "{argv_line:?} 应当合法");
            } else {
                let error = result.expect_err("非 edit 子命令要给错");
                assert!(error.contains("只有 edit 认"), "{error}");
            }
        }
    }

    #[test]
    fn 撤销重做要带历史_且不许和_op_一起给() {
        let error = parse(&argv(&["edit", "--undo"])).expect_err("没给 --history 要给错");
        assert!(error.contains("--history"), "{error}");

        let error = parse(&argv(&["edit", "--undo", "--history", "h.json", "--op", "{}"]))
            .expect_err("--op 与 --undo 互斥");
        assert!(error.contains("一次只做一件事"), "{error}");

        let error = parse(&argv(&[
            "edit",
            "--undo",
            "--redo",
            "--history",
            "h.json",
        ]))
        .expect_err("--undo 与 --redo 互斥");
        assert!(error.contains("只能给一个"), "{error}");
    }

    #[test]
    fn 宽度为零要被拒() {
        let error = parse(&argv(&["render", "--width", "0"])).expect_err("应当报错");
        assert!(error.contains("不能是 0"), "{error}");
    }

    #[test]
    fn 侧挂标志表里每一条都真的被处理() {
        // 与上面那张表同一条规矩：表里列了却没写分支的选项会落到 other 报错。
        // 这一张的值受约束，所以占位值**各自给合法的**：`--format 1` 本来就该被拒。
        for flag in KNOWN_SIDECAR_FLAGS {
            let value = if flag == "--format" { "ass" } else { "side.srt" };
            let mut line = vec!["render", flag, value];
            if flag != "--subtitle-out" {
                // `--format` 要跟着 `--subtitle-out`（单独出现是用法错，见下一条）。
                line.extend(["--subtitle-out", "side.srt"]);
            }
            let parsed = parse(&argv(&line))
                .unwrap_or_else(|error| panic!("表里的选项 {flag} 解析不过：{error}"));
            assert_eq!(parsed.command, "render");
            assert_eq!(parsed.subtitle_out.as_deref(), Some("side.srt"));
        }
        // 顺序反过来也要认：`--format` 先出现不该改变结论。
        let swapped = parse(&argv(&["render", "--format", "ass", "--subtitle-out", "side.ass"]))
            .expect("合法");
        assert_eq!(swapped.format, Some(SidecarFormat::Ass));
    }

    #[test]
    fn 侧挂格式只认两种拼法() {
        assert_eq!(SidecarFormat::from_flag("srt"), Some(SidecarFormat::Srt));
        assert_eq!(SidecarFormat::from_flag("ass"), Some(SidecarFormat::Ass));
        // 大小写与别的格式都不认：认了就等于"悄悄换一种文件格式"。
        assert_eq!(SidecarFormat::from_flag("SRT"), None);
        assert_eq!(SidecarFormat::from_flag("ssa"), None);
        assert_eq!(SidecarFormat::from_flag("vtt"), None);
        let error = parse(&argv(&["render", "--subtitle-out", "s.srt", "--format", "vtt"]))
            .expect_err("应当报错");
        assert!(error.contains("srt / ass"), "{error}");
    }

    #[test]
    fn 侧挂标志只在_render_上认() {
        // **静默收下就是"参数被丢掉"**：用户以为写了侧挂文件，而退出码还是 0。
        for line in [
            vec!["frame", "--subtitle-out", "s.srt"],
            vec!["frame", "--format", "srt"],
            vec!["probe", "--subtitle-out", "s.srt"],
            vec!["subtitle", "--subtitle-out", "s.srt"],
        ] {
            let error = parse(&argv(&line)).expect_err("应当报错");
            assert!(error.contains("render"), "{line:?} -> {error}");
        }
        // `--format` 说的是"那份侧挂文件"，没有文件就没有它说明的对象。
        let error = parse(&argv(&["render", "--format", "srt"])).expect_err("应当报错");
        assert!(error.contains("--subtitle-out"), "{error}");
        // 没给子命令时照旧是"问怎么用"，不该因为这两个选项变成错误。
        assert!(parse(&argv(&["--subtitle-out", "s.srt"])).is_ok());
    }

    fn sidecar_args(path: &str, format: Option<SidecarFormat>) -> Args {
        Args {
            command: "render".to_string(),
            subtitle_out: Some(path.to_string()),
            format,
            ..Args::default()
        }
    }

    #[test]
    fn 侧挂格式明说的优先没明说看扩展名() {
        // 扩展名认得出：明说一致就照明说的，不一致**报错**（文件名叫 ASS 里面是 SRT，
        // 播放器只会说"ASS 解析失败"）。
        assert_eq!(
            sidecar_target(&sidecar_args("s.srt", None)).expect("合法"),
            Some((PathBuf::from("s.srt"), SidecarFormat::Srt))
        );
        assert_eq!(
            sidecar_target(&sidecar_args("s.ASS", None)).expect("合法"),
            Some((PathBuf::from("s.ASS"), SidecarFormat::Ass))
        );
        assert_eq!(
            sidecar_target(&sidecar_args("s.ssa", None)).expect("合法"),
            Some((PathBuf::from("s.ssa"), SidecarFormat::Ass))
        );
        assert_eq!(
            sidecar_target(&sidecar_args("s.ass", Some(SidecarFormat::Ass))).expect("合法"),
            Some((PathBuf::from("s.ass"), SidecarFormat::Ass))
        );
        let clash = sidecar_target(&sidecar_args("s.ass", Some(SidecarFormat::Srt)))
            .expect_err("应当报错");
        assert!(clash.contains("--format"), "{clash}");
        // 扩展名认不出来：明说了就照明说的，没明说**不猜**。
        assert_eq!(
            sidecar_target(&sidecar_args("subs.txt", Some(SidecarFormat::Srt))).expect("合法"),
            Some((PathBuf::from("subs.txt"), SidecarFormat::Srt))
        );
        let guess = sidecar_target(&sidecar_args("subs.txt", None)).expect_err("应当报错");
        assert!(guess.contains("--format"), "{guess}");
        // 没要侧挂文件时这一步什么都不做。
        assert_eq!(
            sidecar_target(&Args { command: "render".to_string(), ..Args::default() })
                .expect("合法"),
            None
        );
    }

    #[test]
    fn 侧挂文本的时间重定基到产物并按时排序() {
        let timebase = TimebaseDto { num: 60, den: 1 };
        // 第 60 帧是产物的第 0 帧：写出去的时间要从 0 起算，而不是从 1000ms。
        let spans = vec![
            OverlaySpan {
                asset_id: "b.srt".to_string(),
                text: "后说的".to_string(),
                first: 120,
                last: 179,
            },
            OverlaySpan {
                asset_id: "a.srt".to_string(),
                text: "先说的\n第二行".to_string(),
                first: 60,
                last: 119,
            },
        ];
        let srt = sidecar_text(&spans, 60, &timebase, SidecarFormat::Srt).expect("算得出");
        assert_eq!(
            srt,
            "1\n00:00:00,000 --> 00:00:01,000\n先说的\n第二行\n\n\
             2\n00:00:01,000 --> 00:00:02,000\n后说的\n\n",
            "起点要重定基、条目要按起点排序"
        );
        // ASS 是同一条内容的另一种装法：头部是 [Script Info]，条目在 Dialogue 行上。
        let ass = sidecar_text(&spans, 60, &timebase, SidecarFormat::Ass).expect("算得出");
        assert!(ass.starts_with("[Script Info]\n"), "{ass}");
        assert!(ass.contains("Dialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,先说的\\N第二行"), "{ass}");
    }

    #[test]
    fn 侧挂文本不留零长度的条目() {
        // 比 1ms 还短的一帧在 SRT 里没法精确表示（这里时间基 1000fps：一帧不到 1ms）。
        // 能表示的最低限度是"至少 1ms"：终点落在起点之前或等于起点的条目会被播放器丢掉。
        let timebase = TimebaseDto { num: 4000, den: 1 };
        let spans = vec![OverlaySpan {
            asset_id: "s.srt".to_string(),
            text: "一闪而过".to_string(),
            first: 8000,
            last: 8000,
        }];
        let srt = sidecar_text(&spans, 8000, &timebase, SidecarFormat::Srt).expect("算得出");
        assert!(srt.contains("00:00:00,000 --> 00:00:00,001"), "{srt}");
        // 时间基坏掉时**不猜**：这是工程文件坏了，报出去让人查。
        let broken = sidecar_text(&spans, 0, &TimebaseDto { num: 0, den: 1 }, SidecarFormat::Srt)
            .expect_err("应当报错");
        assert!(broken.contains("时间基"), "{broken}");
    }

    #[test]
    fn 帮助文案提到侧挂的两个选项() {
        // 帮助是契约的一部分：能用的选项必须在里面，不然"能不能用"只能靠猜。
        for name in KNOWN_SIDECAR_FLAGS {
            assert!(USAGE.contains(name), "帮助里没写 {name}");
        }
        assert!(USAGE.contains("只有 render 认"), "帮助里没说清谁能用");
    }

    #[test]
    fn 帮助里列了每一个认得的子命令() {
        // 写了一个子命令却没在帮助里说，等于它不存在。
        // check-cli 也在真跑 `--help` 对账，这一条是最便宜的那道。
        for name in COMMANDS {
            assert!(USAGE.contains(name), "帮助里没写 {name}");
        }
    }

    #[test]
    fn 具名动作拼出来的操作与_op_那份逐字段相同() {
        // 「同一实现的糖」这句验收要求，在这里变成一条可执行的判据：
        // 开关拼出来的 EditOp 必须与手写 --op 的那份**完全相等**。
        // 是相等而不是"差不多" —— 差一个字段就是两套实现。
        let cases: [(&[&str], &str); 5] = [
            (
                &[
                    "clip", "insert", "--project", "p.json", "--track", "c", "--asset", "a.mp4",
                    "--at", "10", "--source-in", "3", "--length", "20", "--id", "x",
                ],
                r#"{"op":"insert","track":"c","asset":"a.mp4","at":10,"source_in":3,"length":20,"id":"x"}"#,
            ),
            (
                &[
                    "clip", "trim", "--project", "p.json", "--layer", "c", "--edge", "out", "--to",
                    "40",
                ],
                r#"{"op":"trim","layer":"c","edge":"out","to":40}"#,
            ),
            (
                &["clip", "split", "--project", "p.json", "--layer", "c", "--at", "75"],
                r#"{"op":"split","layer":"c","at":75}"#,
            ),
            (
                &[
                    "clip", "move", "--project", "p.json", "--layer", "c", "--to", "30", "--track",
                    "d",
                ],
                r#"{"op":"move","layer":"c","to":30,"track":"d"}"#,
            ),
            (
                &["clip", "remove", "--project", "p.json", "--layer", "c", "--ripple"],
                r#"{"op":"remove","layer":"c","ripple":true}"#,
            ),
        ];
        for (cli, json) in cases {
            let args = parse(&argv(cli)).expect("合法");
            let manual: EditOp = serde_json::from_str(json).expect("参照 JSON 合法");
            assert_eq!(
                build_named_op(&args),
                manual,
                "具名写法与 --op 对不上：{}",
                cli.join(" ")
            );
        }
    }

    #[test]
    fn sequence_set_也拼成同一份操作() {
        let args = parse(&argv(&[
            "sequence", "set", "--project", "p.json", "--timebase", "30000/1001", "--width",
            "1920", "--height", "1080",
        ]))
        .expect("合法");
        let manual: EditOp = serde_json::from_str(
            r#"{"op":"set_sequence","timebase":{"num":30000,"den":1001},"width":1920,"height":1080}"#,
        )
        .expect("参照 JSON 合法");
        assert_eq!(build_named_op(&args), manual);
        // 不给 --width / --height 时折成 0，而 0 在 set_sequence 里的意思是「不动它」。
        let bare = parse(&argv(&["sequence", "set", "--project", "p.json", "--timebase", "30"]))
            .expect("合法");
        let manual: EditOp = serde_json::from_str(
            r#"{"op":"set_sequence","timebase":{"num":30,"den":1},"width":0,"height":0}"#,
        )
        .expect("参照 JSON 合法");
        assert_eq!(build_named_op(&bare), manual);
    }

    #[test]
    fn undo_redo_折出来就是_edit_的那两个旗标() {
        // 这一条比"看起来一样"强：它比的是**解析结果逐字段相同**。
        let named =
            parse(&argv(&["undo", "--project", "p.json", "--history", "h.json", "--write"]))
                .expect("合法");
        let flag = parse(&argv(&[
            "edit", "--project", "p.json", "--history", "h.json", "--undo", "--write",
        ]))
        .expect("合法");
        assert_eq!(
            history_alias(&named),
            flag,
            "undo 与 edit --undo 必须折成同一件事"
        );

        let named = parse(&argv(&["redo", "--project", "p.json", "--history", "h.json"]))
            .expect("合法");
        let flag = parse(&argv(&[
            "edit", "--project", "p.json", "--history", "h.json", "--redo",
        ]))
        .expect("合法");
        assert_eq!(history_alias(&named), flag);
    }

    #[test]
    fn 具名动作少给开关会报错() {
        // 少给一个开关**不能**拿默认值顶上：`--source-in` 默认 0 的意思是"从素材头开始"，
        // 而省掉它的那个人可能只是漏了。两种意图分不开，所以判错。
        let err = parse(&argv(&[
            "clip", "insert", "--project", "p.json", "--track", "c", "--asset", "a.mp4", "--at",
            "10", "--length", "20",
        ]))
        .expect_err("少 --source-in 应当报错");
        assert!(err.contains("--source-in"), "{err}");
    }

    #[test]
    fn 具名动作多给开关也会报错() {
        // **多给**与少给一样坏：那个开关会被丢掉，而用户以为它生效了。
        let err = parse(&argv(&[
            "clip", "split", "--project", "p.json", "--layer", "c", "--at", "75", "--length",
            "20",
        ]))
        .expect_err("split 不吃 --length");
        assert!(err.contains("--length"), "{err}");
        // 连"别的子命令本来就认的"也不许混进来：--width 是 render / frame 的。
        let err = parse(&argv(&[
            "clip", "split", "--project", "p.json", "--layer", "c", "--at", "75", "--width", "640",
        ]))
        .expect_err("split 不吃 --width");
        assert!(err.contains("--width"), "{err}");
    }

    #[test]
    fn 不认识的具名动作与缺动作都被挡住() {
        let err = parse(&argv(&["clip", "rotate", "--project", "p.json"])).expect_err("不认识");
        assert!(err.contains("rotate"), "{err}");
        let err = parse(&argv(&["clip", "--project", "p.json"])).expect_err("缺动作名");
        assert!(err.contains("动作名"), "{err}");
        let err = parse(&argv(&["sequence", "scale", "--project", "p.json"])).expect_err("不认识");
        assert!(err.contains("scale"), "{err}");
    }

    #[test]
    fn 那一组开关只归_clip_与_sequence() {
        // 与 --subtitle-out 那两条同一个口径：别的子命令静默收下 = 参数被丢掉。
        let err = parse(&argv(&["probe", "--project", "p.json", "--layer", "c"]))
            .expect_err("probe 不认 --layer");
        assert!(err.contains("--layer"), "{err}");
        let err = parse(&argv(&["edit", "--project", "p.json", "--op", "{}", "--at", "3"]))
            .expect_err("edit 不认 --at（它走 --op）");
        assert!(err.contains("--at"), "{err}");
    }

    #[test]
    fn 编辑那一组的选项不外流() {
        // --op 只归 edit：具名子命令存在的意义就是不必手写那段 JSON。
        let err = parse(&argv(&[
            "clip", "split", "--project", "p.json", "--layer", "c", "--at", "1", "--op", "{}",
        ]))
        .expect_err("--op 只归 edit");
        assert!(err.contains("--op"), "{err}");
        // --write / --history **不是同一张表**：`import` 也落盘，所以它认 --write；
        // 但它不认 --history。这两条一起测，是为了不让下一个人把它们合成一张表 ——
        // 合成之后「import 认不认 --history」会被顺手答成"认"，而那是错的。
        assert!(
            parse(&argv(&["import", "--project", "p.json", "--file", "a.mp4", "--write"])).is_ok(),
            "import 也要落盘，它必须认 --write"
        );
        let err = parse(&argv(&[
            "import", "--project", "p.json", "--file", "a.mp4", "--history", "h.json",
        ]))
        .expect_err("import 不认 --history");
        assert!(err.contains("--history"), "{err}");
        let err = parse(&argv(&["probe", "--project", "p.json", "--write"]))
            .expect_err("probe 不认 --write");
        assert!(err.contains("--write"), "{err}");
        // 具名 undo / redo 与旗标同一条规矩：历史存哪必须由你说。
        let err = parse(&argv(&["undo", "--project", "p.json"])).expect_err("undo 缺 --history");
        assert!(err.contains("--history"), "{err}");
    }

    #[test]
    fn batch_要脚本文件_而脚本只归它() {
        let err = parse(&argv(&["batch", "--project", "p.json"])).expect_err("batch 缺 --script");
        assert!(err.contains("--script"), "{err}");
        let err = parse(&argv(&[
            "edit", "--project", "p.json", "--op", "{}", "--script", "s.ndjson",
        ]))
        .expect_err("--script 只归 batch");
        assert!(err.contains("--script"), "{err}");
    }

    #[test]
    fn edge_与_timebase_在参数这一关就定死() {
        let err = parse(&argv(&[
            "clip", "trim", "--project", "p.json", "--layer", "c", "--edge", "left", "--to", "1",
        ]))
        .expect_err("--edge 只认 in / out");
        assert!(err.contains("--edge"), "{err}");
        let err = parse(&argv(&[
            "sequence", "set", "--project", "p.json", "--timebase", "0/1",
        ]))
        .expect_err("分子 0 不是帧率");
        assert!(err.contains("--timebase"), "{err}");
        let err = parse(&argv(&[
            "sequence", "set", "--project", "p.json", "--timebase", "abc",
        ]))
        .expect_err("认不出的帧率");
        assert!(err.contains("--timebase"), "{err}");
    }

    #[test]
    fn 帮助与空参数都算成功路径() {
        let args = parse(&argv(&["--help"])).expect("合法");
        assert!(args.help);
        let args = parse(&[]).expect("合法");
        assert!(args.command.is_empty());
    }

    #[test]
    fn frame_的两种写法互斥且各出各的() {
        // 「出一帧」与「出一段」是两件事，同时给必须报错 ——
        // 让其中一个悄悄赢，产出的就不是用户要的那一份。
        let both = parse(&argv(&["frame", "--project", "p.json", "--frame", "1", "--from", "0"]))
            .expect("解析这一关不该拦（--from 不是 clip 专用开关）");
        assert!(matches!(frame_range(&both), Err(_)), "同时给应当是用法的错");

        // 单帧：就是那一个帧号，原样。
        let one = parse(&argv(&["frame", "--project", "p.json", "--frame", "30"]))
            .expect("合法");
        assert_eq!(frame_range(&one).expect("合法"), vec![30]);

        // 区间：闭区间，两头都算上。
        let span = parse(&argv(&["frame", "--project", "p.json", "--from", "0", "--to", "2"]))
            .expect("合法");
        assert_eq!(frame_range(&span).expect("合法"), vec![0, 1, 2]);

        // **两头的缺省不对称**：只给 --from 是「从这里到它自己」，
        // 只给 --to 是「从第 0 帧到这里」。写成 `from.unwrap_or(to)` 的话
        // `--to 5` 会退化成只出第 5 帧 —— 一个看着成功、实际少了 5 帧的结果。
        let tail = parse(&argv(&["frame", "--project", "p.json", "--from", "3"]))
            .expect("合法");
        assert_eq!(frame_range(&tail).expect("合法"), vec![3]);
        let head = parse(&argv(&["frame", "--project", "p.json", "--to", "2"]))
            .expect("合法");
        assert_eq!(frame_range(&head).expect("合法"), vec![0, 1, 2]);

        // 一头都不给：**不猜**要哪几帧，报用法错。
        let none = parse(&argv(&["frame", "--project", "p.json"])).expect("合法");
        assert!(matches!(frame_range(&none), Err(_)), "都没给应当是用法的错");

        // 区间反了：在这里判掉，而不是等渲染时发现一帧都没出。
        let reversed = parse(&argv(&["frame", "--project", "p.json", "--from", "9", "--to", "2"]))
            .expect("合法");
        assert!(matches!(frame_range(&reversed), Err(_)), "反区间应当是用法的错");
    }

    #[test]
    fn 用法错与运行期失败是不同的类型() {
        // 退出码契约：0 成功 / 2 用法或校验错 / 1 运行期失败。
        // 这两类从前都走 `Err(String)`，于是「忘给 --project」与「GPU 起不来」
        // 在调用方眼里一模一样。这条把分类钉在类型上。
        let usage: CommandError = usage_error("缺一个开关".to_string()).into();
        assert!(matches!(usage, CommandError::Usage(_)));
        let runtime: CommandError = "GPU 起不来".to_string().into();
        assert!(matches!(runtime, CommandError::Runtime(_)));
        // 文案要能原样透出去：分类变了，说的话不能变。
        assert_eq!(usage.to_string(), "缺一个开关");
        assert_eq!(runtime.to_string(), "GPU 起不来");
    }

    #[test]
    fn 有理数帧率解析() {
        assert_eq!(parse_rate("30/1"), Some((30, 1)));
        assert_eq!(parse_rate("30000/1001"), Some((30000, 1001)));
        // 单独一个数按 den=1 处理（ffprobe 偶尔这么给）。
        assert_eq!(parse_rate("25"), Some((25, 1)));
        // 分母为 0 不是「无穷帧率」，是坏数据。
        assert_eq!(parse_rate("30/0"), None);
        assert_eq!(parse_rate(""), None);
    }

    #[test]
    fn 包列表解析出样本表并认出关键帧() {
        let packets = serde_json::json!({
            "packets": [
                {"pos": "48", "size": "100", "dts": "0", "duration": "512", "flags": "K__"},
                {"pos": "148", "size": "90", "dts": "512", "duration": "512", "flags": "___"},
            ]
        });
        let (samples, origin) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(samples.len(), 2);
        assert_eq!(origin, 0);
        assert!(samples[0].is_sync);
        assert!(!samples[1].is_sync);
        assert_eq!(samples[1].offset, 148);
        assert_eq!(samples[1].dts, 512);
    }

    #[test]
    fn 字段类型混着来也要读对() {
        // ffprobe 实测：pos/size/flags 是字符串，dts/duration 是数字。
        // 只认字符串的话 duration 会被静默当成 0 —— 那正是"错得很安静"的那一类。
        let packets = serde_json::json!({
            "packets": [{"pos": "48", "size": 33751, "dts": 0, "duration": 256, "flags": "K__"}]
        });
        let (samples, _) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(samples[0].size, 33751);
        assert_eq!(samples[0].duration, 256);
        assert_eq!(samples[0].offset, 48);
    }

    #[test]
    fn 负的_dts_按最小值归零并把偏移报出来() {
        // 真实素材（target/s3/proxy1080p.mp4）第一个包的 dts 就是 -512。
        // 报错 -> info/gop 在真实素材上从来跑不通；夹成 0 -> 多个包撞同一个时间戳。
        let packets = serde_json::json!({
            "packets": [
                {"pos": "48", "size": "33751", "dts": -512, "duration": 256, "flags": "K__"},
                {"pos": "33799", "size": "21119", "dts": -256, "duration": 256, "flags": "___"},
                {"pos": "54918", "size": "12530", "dts": 0, "duration": 256, "flags": "___"},
            ]
        });
        let (samples, origin) = samples_from_packets(&packets).expect("能解析");
        assert_eq!(origin, -512);
        assert_eq!(
            samples.iter().map(|s| s.dts).collect::<Vec<_>>(),
            vec![0, 256, 512]
        );
    }

    #[test]
    fn 素材表把相对_uri_挂到_asset_root_上() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "assets":[{"id":"a.mp4","kind":"video","uri":"proxy.mp4"},
                          {"id":"b.mp4","kind":"video","uri":"C:/abs/b.mp4"},
                          {"id":"c.mp4","kind":"video","uri":""}]}"#,
        )
        .expect("能载入");
        let table = build_sources(&doc, Path::new("target/s3"), &[]);
        // 相对 uri 挂到根上。
        assert_eq!(
            table
                .file_for("a.mp4")
                .map(|path| path.to_string_lossy().replace('\\', "/")),
            Some("target/s3/proxy.mp4".to_string())
        );
        // 绝对 uri 原样。
        assert_eq!(
            table
                .file_for("b.mp4")
                .map(|path| path.to_string_lossy().replace('\\', "/")),
            Some("C:/abs/b.mp4".to_string())
        );
        // uri 为空的不进表 —— 位置未知就不假装知道。
        assert!(table.file_for("c.mp4").is_none());
    }

    #[test]
    fn 兜底登记表只补工程文件没登记的_id() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "assets":[{"id":"a.mp4","kind":"video","uri":"real.mp4"}]}"#,
        )
        .expect("能载入");
        let fallback = vec![
            // 同一个 id：工程文件里的位置必须赢。
            ("a.mp4".to_string(), PathBuf::from("target/s3/wrong.mp4")),
            ("b.mp4".to_string(), PathBuf::from("target/s3/fallback.mp4")),
        ];
        let table = build_sources(&doc, Path::new("target/s3"), &fallback);
        assert_eq!(
            table
                .file_for("a.mp4")
                .map(|p| p.to_string_lossy().replace('\\', "/")),
            Some("target/s3/real.mp4".to_string())
        );
        assert_eq!(
            table
                .file_for("b.mp4")
                .map(|p| p.to_string_lossy().replace('\\', "/")),
            Some("target/s3/fallback.mp4".to_string())
        );
    }

    #[test]
    fn 输出尺寸缺省取工程文件的渲染提示() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]},
                "render_hints":{"width":1280,"height":720,"format":"mp4"}}"#,
        )
        .expect("能载入");
        let empty = Args::default();
        assert_eq!(resolve_size(&empty, &doc), (1280, 720));
        let explicit = Args {
            width: Some(640),
            ..Args::default()
        };
        // 命令行只给了一个：另一个仍取提示值，不强行配对。
        assert_eq!(resolve_size(&explicit, &doc), (640, 720));
    }

    #[test]
    fn 音轨要被列出来而不是被笼统地说成不支持() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},
                "tracks":[{"id":"v1","kind":"video","layers":[]},
                          {"id":"a1","kind":"audio","layers":[]},
                          {"id":"a2","kind":"audio","layers":[]}]}}"#,
        )
        .expect("能载入");
        assert_eq!(
            audio_track_ids(&doc.timeline),
            vec!["a1".to_string(), "a2".to_string()]
        );
    }

    #[test]
    fn 空时间线的帧区间是空的() {
        let doc = load_doc(
            r#"{"project_schema":1,"timeline":{"schema":2,"timebase":{"num":30,"den":1},"tracks":[]}}"#,
        )
        .expect("能载入");
        // 空工程 -> end=0 -> to = -1 < from = 0，要报错而不是"出 0 帧"。
        assert!(resolve_range(&Args::default(), &doc).is_err());
        // 显式给区间就照给。
        let args = Args {
            from: Some(0),
            to: Some(89),
            ..Args::default()
        };
        assert_eq!(resolve_range(&args, &doc).expect("合法"), (0, 89));
    }
}
