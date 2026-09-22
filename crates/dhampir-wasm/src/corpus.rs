//! 浏览器侧的 corpus 取证：把 M1 那张表在 WebGPU 上重画一遍，并把**可落盘的字节**交出去。
//!
//! # 这一模块里没有渲染逻辑
//!
//! 渲染、判定、记录的形状全在 [`dhampir_core::render::corpus`]。这里只做三件浏览器专属的事：
//!
//! 1. 建 [`wgpu::Instance`]（唯一的分叉，见 [`crate::web::new_instance`]）并缓存 device；
//! 2. 把 **native 那份记录里不成立的项如实标注**——浏览器上 WGSL 由浏览器自己的实现编译
//!    （Chrome 是 Dawn/Tint），naga 不在那条路径上，把 native 的版本号抄过来就是一句假话；
//! 3. 把整轮的产出留在缓存里，让页面**逐帧**取走 PNG 字节。一轮 80 帧，一次全交出去
//!    会白白在 JS 与 wasm 之间多抄一遍，而页面本来就是逐帧 POST 的。
//!
//! # 页面为什么拿不到文件
//!
//! 浏览器不能写磁盘，所以落盘由 `scripts/serve-corpus-harness.mjs` 做：页面把
//! adapter.json、每帧 PNG、readings.txt、run.json 依次 POST 过去，服务端**校验**之后才写。
//! 写下去的顺序与 native 一致（adapter 先、frames 次、readings 再、run.json 最后，
//! 见 `crates/dhampir-worker/src/main.rs` 的 `run_corpus`）——顺序不是装饰：中途失败时
//! "在什么环境里失败的"必须已经在盘上。
//!
//! # 为什么网页腿要自己起个名字
//!
//! native 腿的目录名由后端位标志派生（`DX12` → `dx12`，见
//! [`dhampir_core::gpu::backend_slug`]）。浏览器腿"用了哪块卡"**不是**后端位标志能表达的
//! （同一个 `BROWSER_WEBGPU` 底下可以是 NVIDIA、AMD，也可以是软件适配器），所以腿名由
//! 调用方给，值域钉死在 `[a-z0-9-]`（[`check_leg_slug`]）——它会被当成**目录名**用，
//! 一个能拼出 `..` 的字符串在这里就该被拒绝。
//!
//! 记录里的 `backend_slug` 写的就是这个名字，于是"记录里的腿名 == 产物目录名"这条
//! native 侧本来成立的不变量，在浏览器腿上仍然**可以被守卫逐条检查**，而不是靠约定。

use std::cell::RefCell;

use dhampir_core::gpu::{self, GpuContext};
use dhampir_core::render::corpus;
use dhampir_core::render::{
    SELECTABLE_SCENES, SceneRun, SceneSpec, scene_by_name, scene_names,
};
use dhampir_core::wgpu;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

use crate::web;

// ---------------------------------------------------------------------------
// 常量与纯判定
// ---------------------------------------------------------------------------

/// 本腿在记录里的身份。
///
/// native 那条腿没有这个键——它的身份由 `requested_backends` 就说清了。浏览器腿需要它，
/// 因为"哪条腿"在这里还包含**哪个浏览器实现**：同样的 `BROWSER_WEBGPU`，Chrome 与 Safari
/// 编译 WGSL 的是两套实现，将来排查"一边过一边不过"时，第一个要问的正是这一项。
pub const PRODUCER: &str = "browser-webgpu/wasm32";

/// 一轮能跑的帧数上限。
///
/// 与 `dhampir-worker` 的 `MAX_FRAMES` 同值：这是**输入边界**，两侧各自在边界上拦一次，
/// 而不是渲染逻辑的一部分（渲染本身在 core，那一份是共用的）。
pub const MAX_FRAMES: u32 = 1024;

/// 腿名（= 产物目录名）的字母表：`[a-z0-9-]`，首尾不能是 `-`，最长 64。
///
/// 与 [`dhampir_core::gpu::backend_slug`] 用的是同一个字母表——不是巧合：两条腿的腿名
/// 会出现在同一层的目录名里，值域不一致的话，"哪些字符是安全的"就有了两个答案。
///
/// **拒绝而不是清洗**：`browser/../..` 这种输入被静默改成 `browser` 之后，
/// 记录里写下的名字与调用方以为的名字就不是一个东西了。边界上宁可红。
pub fn check_leg_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() || slug.len() > 64 {
        return Err(format!("腿名长度要在 1..=64，收到 {}", slug.len()));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(format!(
            "腿名只允许 [a-z0-9-]，收到 {slug:?}——它会被当成目录名用，所以不做静默清洗"
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(format!("腿名首尾不能是 `-`，收到 {slug:?}"));
    }
    Ok(())
}

/// 页面上那个下拉框的取值 → 要渲染的场景列表。
///
/// `all` 展开成 [`SELECTABLE_SCENES`] 的**原顺序**（记录里的帧序因此是稳定的）。
/// 名字不认识时报错并列出可用值，而不是悄悄跑 `all`：一个拼错的场景名会变成
/// "两个小时后发现比对少了一个场景"。
pub fn select_specs(scene: &str) -> Result<Vec<&'static SceneSpec>, String> {
    if scene == "all" {
        return Ok(SELECTABLE_SCENES.iter().collect());
    }
    match scene_by_name(scene) {
        Some(spec) => Ok(vec![spec]),
        None => Err(format!(
            "不认识的场景：{scene}。可用：all、{}",
            scene_names().join("、")
        )),
    }
}

/// 帧区间必须是**半开且非空**的（`0..16` 表示 16 帧，与 Rust 的区间写法一致）。
///
/// 空区间被拒绝，而不是"跑 0 帧然后报全绿"：一次什么都没跑的运行不能算通过。
pub fn check_frames(frames: (u32, u32)) -> Result<(), String> {
    if frames.1 <= frames.0 {
        return Err(format!(
            "帧区间必须是半开的非空区间（如 0..16），收到 {}..{}",
            frames.0, frames.1
        ));
    }
    if frames.1 - frames.0 > MAX_FRAMES {
        return Err(format!(
            "一次最多 {MAX_FRAMES} 帧，收到 {} 帧",
            frames.1 - frames.0
        ));
    }
    Ok(())
}

/// `Date.now()` 传进来的那一刻 → 记录里的毫秒数。**不编造时钟。**
///
/// wasm 里没有系统时钟，时间戳只能由页面给。给不出时页面传 0，而 0 在记录里表现为
/// `null`（见 [`dhampir_core::render::corpus::epoch_seconds`]）——编一个假的时间戳
/// 会让"这份记录是什么时候写的"变成一句谎话。
fn to_epoch_millis(raw: f64) -> u64 {
    if !raw.is_finite() || raw <= 0.0 || raw >= 9_007_199_254_740_992.0 {
        0
    } else {
        raw as u64
    }
}

// ---------------------------------------------------------------------------
// 记录：adapter.json 与整轮摘要
// ---------------------------------------------------------------------------

/// 页面那一次 `navigator.gpu.requestAdapter()` 读到的东西 → 记录里的 `in_page` 块。
/// **纯函数**（输入是一串 JSON 文本，页面给的）。
///
/// # 为什么身份要页面自己去读一遍
///
/// 因为 wgpu 把 `GPUAdapterInfo` 丢掉了：它的 webgpu 后端 `map_adapter_info` 拿
/// `description()` 当 `name`（Chrome 给空串）、`vendor`/`device` 写死 0。于是 `adapter`
/// 那一块在浏览器上**没有任何能认出卡的信息**，而 `adapter` 那一块正是这份记录的全部意义。
/// 浏览器侧的原文（`vendor` / `architecture`）只有页面自己能读——所以页面读一次、原样交上来，
/// 形状由这里定。
///
/// # 只认识下面这几个键
///
/// 页面多给的键**丢掉**：记录的形状由这一份源码定，不由页面定。少给的、类型不对的、
/// 或者 `vendor` 是空串的，一律**拒绝**而不是填个默认值——空 vendor 意味着"哪块卡"
/// 这条链的第一环就断了，那不是能靠默认值补上的事。
pub fn parse_in_page(text: &str) -> Result<Value, String> {
    let raw: Value = serde_json::from_str(text)
        .map_err(|error| format!("页面给的适配器信息不是 JSON：{error}"))?;
    let object = raw
        .as_object()
        .ok_or_else(|| "页面给的适配器信息不是对象".to_string())?;

    let string_key = |key: &str| -> Result<String, String> {
        match object.get(key) {
            Some(Value::String(value)) => Ok(value.clone()),
            Some(other) => Err(format!("in_page.{key} 应是字符串，收到 {other}")),
            None => Err(format!("in_page 里没有 {key}")),
        }
    };
    let vendor = string_key("vendor")?;
    if vendor.is_empty() {
        return Err(
            "浏览器没报出 in_page.vendor（空串）：卡身份的第一环就断了，记录里不能留空".to_string(),
        );
    }
    let architecture = string_key("architecture")?;
    let device = string_key("device")?;
    let description = string_key("description")?;
    let is_fallback_adapter = match object.get("is_fallback_adapter") {
        Some(Value::Bool(value)) => *value,
        Some(other) => return Err(format!("in_page.is_fallback_adapter 应是布尔值，收到 {other}")),
        None => return Err("in_page 里没有 is_fallback_adapter".to_string()),
    };
    // 这两个是 Chrome 的扩展字段（WGSL 的 subgroup 下限），缺了不影响身份，如实给 null。
    let size_key = |key: &str| -> Result<Value, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(Value::Null),
            Some(Value::Number(number)) if number.as_u64().is_some_and(|value| value > 0) => {
                Ok(Value::from(number.as_u64().unwrap_or_default()))
            }
            Some(other) => Err(format!("in_page.{key} 应是正整数，收到 {other}")),
        }
    };
    let subgroup_min_size = size_key("subgroup_min_size")?;
    let subgroup_max_size = size_key("subgroup_max_size")?;

    Ok(json!({
        "vendor": vendor,
        "architecture": architecture,
        "device": device,
        "description": description,
        "is_fallback_adapter": is_fallback_adapter,
        "subgroup_min_size": subgroup_min_size,
        "subgroup_max_size": subgroup_max_size,
        "note": IN_PAGE_NOTE,
    }))
}

/// `in_page` 那一块为什么长这样。写进记录，复核的人不必去读这份源码。
const IN_PAGE_NOTE: &str = "浏览器自己报的适配器身份。Chrome 只给得出 vendor 与 architecture，\
device/description 是空串——这是浏览器的隐私策略，不是我们没读到；而 wgpu 的 webgpu 后端把 \
GPUAdapterInfo 映射成 name=description()（空）、vendor/device=0，所以旁边 `adapter` 那一块在\
浏览器上认不出卡。这一块是页面里**另一次** requestAdapter 的结果：wgpu 内部那个 JS 适配器对象\
不对外暴露，两处无法在页面内对账；把 vendor 对到宿主某一块卡由 host-gpu.json 做（见 gpu_identity）。";

/// 名字为空时写进记录的"身份缺口声明"。
///
/// 浏览器腿的 `adapter.name` 是空串（见 [`IN_PAGE_NOTE`]），这时记录自己**说清**它答不出
/// "用了哪块卡"、该到哪个文件里去找答案。这不是把问题藏起来，而是把它写下来：
/// 服务端会**要求**这个声明存在（`resolves_to` 指的文件也必须真在），
/// 否则拒收整轮记录——一份说不出用哪块卡的浏览器记录，对 M2 毫无用处。
fn unresolved_identity() -> Value {
    json!({
        "state": "unresolved",
        "reason": "浏览器不暴露显卡型号：GPUAdapterInfo.device/description 为空，页面里问不出“哪块卡”",
        "resolved_by": "harness",
        "resolves_to": "host-gpu.json",
    })
}

/// 记录里的"卡名字"：拿不到就是 `None`。
///
/// `""` 与 `None` 在这一层是**同一件事**（浏览器不给名字），所以判一次、收成一处：
/// `adapter.json` 的 `adapter_name` 与 `run.json` 的 `backends[0].adapter_name` 都得是
/// `null`——两处各判一次的话，哪天一边改成 `""`，"两条腿的记录里同一个键不是同一个意思"
/// 这种事只能靠人去逐字节比。
fn adapter_name_of(fields: &[(&'static str, String)]) -> Option<String> {
    fields
        .iter()
        .find(|(key, _)| *key == "name")
        .map(|(_, value)| value.clone())
        .filter(|name| !name.is_empty())
}

/// 浏览器腿的 `adapter.json`。**纯函数**。
///
/// adapter 身份以 [`dhampir_core::gpu::describe_adapter`] 的输出形式传进来——与 native 侧
/// `AdapterIdentity` 是同一形态，也因此**可以被单测直接构造**：否则这份记录就只能靠
/// "跑一次真的浏览器"来验，而那种验法在 CI 上永远不会发生。
///
/// 第二个入参是 [`parse_in_page`] 的产物（页面读到的适配器原文）。
///
/// # 与 native 那份的差别，逐条都有理由
///
/// - `producer`：只有浏览器腿有。见 [`PRODUCER`]。
/// - `naga_version`：`null`。浏览器上 WGSL 由浏览器自己编译，naga 不在那条路径上；
///   旁边的 `naga_version_note` 把这句话写进记录，复核的人不必去读这份源码。
/// - `timing_record`：`null`。本腿不出 `timing.json`（M2 的判据是"两条腿画得一样"，
///   不是"浏览器多快"），但**不许沉默地少一个文件**：少写它就等于让"这份产物里没有计时"
///   这件事只能靠人去数目录。
/// - `backend_slug`：调用方给的腿名，不是后端位标志派生的。见模块文档。
/// - `in_page` / `gpu_identity`：只有浏览器腿有，见上面两条。
pub fn browser_adapter_json(
    fields: &[(&'static str, String)],
    in_page: Value,
    leg_slug: &str,
    epoch_millis: u64,
) -> Value {
    let in_page_name = adapter_name_of(fields);
    // 空名字写 `null` 而不是 `""`：`""` 读起来像"名字叫空"，`null` 才是"这个名字没拿到"。
    // 同一件事在 `gpu_identity` 那里被写成了可被守卫检查的声明。
    let adapter_name = match in_page_name {
        Some(name) => Value::from(name),
        None => Value::Null,
    };
    let identity = if adapter_name.is_null() {
        Some(unresolved_identity())
    } else {
        None
    };
    let adapter: serde_json::Map<String, Value> = fields
        .iter()
        .map(|(key, value)| ((*key).to_string(), Value::from(value.clone())))
        .collect();

    let mut record = json!({
        "schema": corpus::CORPUS_RECORD_SCHEMA,
        // 表的契约版本，不是"产生它的里程碑"：这张表在 M1 冻结，浏览器腿沿用同一个值，
        // 于是"两条腿用的是不是同一张表"由**字节**回答。
        "milestone": corpus::CORPUS_TABLE_MILESTONE,
        "kind": "adapter",
        "producer": PRODUCER,
        "build_profile": build_profile(),
        "adapter": Value::Object(adapter),
        "adapter_name": adapter_name,
        // 页面读到的那份适配器原文。有了它，"记录说不清用了哪块卡"这件事才有据可查。
        "in_page": in_page,
        "requested_backends": gpu::backend_label(gpu::BROWSER_BACKENDS),
        "backend_slug": leg_slug,
        "target_format": format!("{:?}", corpus::CORPUS_TARGET_FORMAT),
        "corpus_target_size": format!(
            "{}x{}",
            dhampir_core::render::SCENE_TARGET_SIZE.0,
            dhampir_core::render::SCENE_TARGET_SIZE.1
        ),
        "crate_version": env!("CARGO_PKG_VERSION"),
        "wgpu_version": gpu::WGPU_VERSION,
        "naga_version": Value::Null,
        "naga_version_note": "浏览器侧 WGSL 由浏览器自己的实现编译（Chrome 是 Dawn/Tint），\
                              naga 不在这条路径上——这一栏不适用，不是漏填",
        "timing_record": Value::Null,
        "timing_note": "本腿不出 timing.json：M2 的判据是两条腿画得一样，不是浏览器多快\
                        （计时在 M6 的验收里）。这里写明，免得复核的人以为它写失败了",
        "probe_digest": format!("{:016x}", dhampir_core::timeline::probe_digest()),
        "probe_format_version": dhampir_core::timeline::PROBE_FORMAT_VERSION,
        "unix_epoch_seconds": corpus::epoch_seconds(epoch_millis),
        "unix_epoch_millis": if epoch_millis == 0 { Value::Null } else { Value::from(epoch_millis) },
        // 记录自己声明自己的非确定项：复核的人不必先读完这个文件才知道哪里会变。
        "nondeterministic_fields": [
            "unix_epoch_seconds",
            "unix_epoch_millis",
            "build_profile（换构建就变）",
            "adapter.* / adapter_name（换机器、换浏览器、换驱动就变）",
            "in_page.*（同上；它记的是浏览器当场报的那块卡）",
        ],
    });

    // 身份缺口只在**真的缺**的时候出现：名字拿得到的时候这个键根本不写，
    // 免得读的人以为"每条腿都有个没解决的'哪块卡'"。
    if let Some(identity) = identity {
        record["gpu_identity"] = identity;
    }
    record
}

/// 当前是 debug 还是 release 构建。**必须进记录**：debug 下的浏览器渲染慢一个数量级，
/// 将来对时间时读的人不该被迫自己猜。
pub fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    }
}

/// 交给页面的整轮摘要。**纯函数**。
///
/// 页面拿它做四件事：显示、把 `run_json` / `readings` 原样 POST、按 `frames` 逐帧取 PNG、
/// 与 `?expect=` 的期望摘要对照。所以它里面**必须**包含那两个文本的原文——
/// 让页面自己拼 `run.json` 就等于给同一份记录写了第二个作者。
///
/// `frames` 那一栏是**从 `leg_json` 里投影出来的**（不是另算一遍）：页面要的
/// `(场景, 帧号, 路径, 摘要)` 四元组在 `frame_json` 里已有唯一答案，另写一遍就是等着
/// "记录里那一行"与"页面 POST 的那一帧"对不上。
pub fn run_summary(
    run: &SceneRun,
    specs: &[&'static SceneSpec],
    frames: (u32, u32),
    leg_slug: &str,
    requested: &str,
    adapter_name: Option<&str>,
    run_json: &str,
    readings: &str,
) -> Value {
    let leg = corpus::leg_json(run, specs, requested, adapter_name, frames);
    let backend = &leg["backends"][0];
    let frames_json: Vec<Value> = backend["frames"]
        .as_array()
        .map(|all| {
            all.iter()
                .map(|frame| {
                    json!({
                        "scene": frame["scene"],
                        "frame": frame["frame"],
                        "png": frame["png"],
                        "png_digest": frame["png_digest"],
                        "png_bytes": frame["png_bytes"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "schema": corpus::CORPUS_RECORD_SCHEMA,
        "milestone": corpus::CORPUS_TABLE_MILESTONE,
        "kind": corpus::CORPUS_RECORD_KIND,
        "producer": PRODUCER,
        "leg_slug": leg_slug,
        "requested_backends": requested,
        "adapter_name": adapter_name,
        "scenes": leg["scenes"],
        "frame_range": leg["frame_range"],
        "frames_per_scene": leg["frames_per_scene"],
        "frame_count": run.frames.len(),
        "frames_digest": backend["frames_digest"],
        "counts": backend["counts"],
        "repeat_mismatches": backend["repeat_mismatches"],
        "frames": frames_json,
        // 页面直接把这两份文本 POST 出去；它们由 core 生成，页面一个字都不改。
        "run_json": run_json,
        "readings": readings,
    })
}

// ---------------------------------------------------------------------------
// 宿主
// ---------------------------------------------------------------------------

thread_local! {
    /// 浏览器的 device 只申请一次。用 `thread_local` 而不是 `static` 的理由与
    /// [`crate::web`] 里的 `CANVAS_HOST` 相同：wgpu 的类型不是 `Sync`，
    /// wasm 目前也是单线程的。
    static CORPUS_HOST: RefCell<Option<CorpusHost>> = const { RefCell::new(None) };
}

/// corpus 取证所需的一切。
///
/// `Instance` 必须比 `GpuContext` 活得久（adapter/device 由它创建），Rust 的 drop 顺序
/// 正好是声明顺序的逆序，所以它放最前——与 `web::CanvasHost` 同一个理由。
struct CorpusHost {
    _instance: wgpu::Instance,
    ctx: GpuContext,
    /// 本轮腿名。`backend_slug` 写它，产物目录名也是它。
    leg_slug: String,
    /// 打开时就定稿的 adapter.json 文本（时间戳也冻在那一刻，与 native 一样：
    /// 两份记录共用同一个时间戳，谁都能看出它们属于同一次运行）。
    adapter_json: String,
    /// 与 native `adapter.name()` 同一个来源（`describe_adapter` 的 `name` 那一栏）。
    adapter_name: Option<String>,
    /// 最近一轮的帧，供 [`dhampir_corpus_frame_png`] 取字节。
    last: Option<SceneRun>,
}

// ---------------------------------------------------------------------------
// 导出给页面
// ---------------------------------------------------------------------------

/// 可选场景名，JSON 数组。页面拿它建下拉框，免得把场景清单抄进 HTML。
#[wasm_bindgen]
pub fn dhampir_corpus_scene_names() -> String {
    Value::from(scene_names()).to_string()
}

/// 建（或复用）corpus host，返回这个 **adapter.json 的文本**（已带结尾换行）。
///
/// 页面拿到它就该**立刻** POST 给服务端——与 native 一致，环境记录先落盘：
/// 后面那 80 帧里任何一帧失败时，"这是在什么环境里失败的"必须已经查得到。
///
/// `epoch_millis` 由页面从 `Date.now()` 取（wasm 里没有系统时钟）。
/// `in_page_json` 是页面那一次 `navigator.gpu.requestAdapter()` 读到的 `GPUAdapterInfo`
/// 原文（见 [`parse_in_page`]：为什么非要在页面里另读一次，以及形状由谁定）。
#[wasm_bindgen]
pub async fn dhampir_corpus_open(
    leg_slug: String,
    epoch_millis: f64,
    in_page_json: String,
) -> Result<String, JsValue> {
    check_leg_slug(&leg_slug).map_err(web::js_err)?;
    // 先解析页面给的那份：形状不对时**连 GPU 都不用起**，错误里直接说清缺了什么。
    let in_page = parse_in_page(&in_page_json).map_err(web::js_err)?;

    if let Some(text) = CORPUS_HOST.with(|host| {
        host.borrow()
            .as_ref()
            .map(|host| host.adapter_json.clone())
    }) {
        return Ok(text);
    }

    let instance = web::new_instance();
    let ctx = gpu::request_context(&instance, None)
        .await
        .map_err(|error| web::js_err(error.to_string()))?;

    let fields = gpu::describe_adapter(&ctx.adapter_info);
    let adapter_name = adapter_name_of(&fields);
    let json = browser_adapter_json(&fields, in_page, &leg_slug, to_epoch_millis(epoch_millis));
    let text = corpus::record_text(&json).map_err(web::js_err)?;

    let host = CorpusHost {
        _instance: instance,
        ctx,
        leg_slug,
        adapter_json: text.clone(),
        adapter_name,
        last: None,
    };
    CORPUS_HOST.with(|slot| *slot.borrow_mut() = Some(host));
    Ok(text)
}

/// 跑一轮 corpus，返回摘要 JSON（见 [`run_summary`]）。**帧的顺序与 native 一致**：
/// 场景按注册表顺序、帧号递增——摘要本身对顺序敏感，顺序变了摘要就会变。
///
/// `frames_start` / `frames_end` 原样透传给渲染与记录：汇总跑的是哪一段、记录里写的是哪一段，
/// 必须是同一个数。
#[wasm_bindgen]
pub async fn dhampir_corpus_run(
    scene: String,
    frames_start: u32,
    frames_end: u32,
) -> Result<String, JsValue> {
    let frames = (frames_start, frames_end);
    check_frames(frames).map_err(web::js_err)?;
    let specs = select_specs(&scene).map_err(web::js_err)?;

    // 取出来再放回去：`render_run` 是异步的，跨 await 持着 RefCell 的借用时，
    // 任何一次重入（页面在别处调本模块的同步导出）都会撞上 panic。
    // 这与 `web::dhampir_probe_render_canvas` 是同一个写法，理由也相同。
    let mut host = CORPUS_HOST
        .with(|slot| slot.borrow_mut().take())
        .ok_or_else(|| {
            web::js_err(
                "corpus host 不在：要么还没 dhampir_corpus_open，要么上一轮还没跑完（或它失败了）",
            )
        })?;

    let requested = gpu::backend_label(gpu::BROWSER_BACKENDS);
    // 拿所有权副本再 `.as_deref()`：下面失败时要把 `host` 整个放回去，
    // 跨着那个移动持一份从 `host` 借出来的 `&str` 是过不了借用检查的。
    let adapter_name = host.adapter_name.clone();
    let adapter_name = adapter_name.as_deref();

    let run = match corpus::render_run(&host.ctx, &specs, frames).await {
        Ok(run) => run,
        Err(error) => {
            // **失败也要把 host 放回去**。不放的话这一次的腿名与时间戳就随 drop 消失了，
            // 页面重试只会走 `dhampir_corpus_open` 建一个新 host——于是
            // "重试前后是同一条腿"这句话就不成立了，而刚 POST 上来的 adapter.json
            // 还写着旧的时间戳。渲染失败是**这一轮**失败，不是这台设备没了。
            CORPUS_HOST.with(|slot| *slot.borrow_mut() = Some(host));
            return Err(web::js_err(format!("corpus 渲染失败：{error}")));
        }
    };

    let leg = corpus::leg_json(&run, &specs, &requested, adapter_name, frames);
    let run_json = corpus::record_text(&leg).map_err(web::js_err)?;
    let readings = corpus::report_text(&run);
    let summary = run_summary(
        &run,
        &specs,
        frames,
        &host.leg_slug,
        &requested,
        adapter_name,
        &run_json,
        &readings,
    );

    host.last = Some(run);
    CORPUS_HOST.with(|slot| *slot.borrow_mut() = Some(host));
    Ok(summary.to_string())
}

/// 取一轮里某一帧的 PNG 字节（**就是记录里那一份**，不是重新渲染的）。
///
/// 重新渲染一遍看似更"干净"，实际是把"页面上传的字节"与"记录里写的摘要"之间
/// 插进了一次不确定的渲染：两者一旦不一致，就再也说不清是哪一边错了。
#[wasm_bindgen]
pub fn dhampir_corpus_frame_png(scene: String, frame: u32) -> Result<Vec<u8>, JsValue> {
    CORPUS_HOST.with(|slot| {
        let borrowed = slot.borrow();
        let host = borrowed
            .as_ref()
            .ok_or_else(|| web::js_err("corpus host 不在：先 dhampir_corpus_open"))?;
        let run = host
            .last
            .as_ref()
            .ok_or_else(|| web::js_err("还没跑过一轮：先 dhampir_corpus_run"))?;
        run.frames
            .iter()
            .find(|candidate| candidate.spec.name == scene && candidate.frame == frame)
            .map(|found| found.png.clone())
            .ok_or_else(|| {
                web::js_err(format!(
                    "这一轮里没有 {scene} f{frame:03}——页面只该取摘要里列出的帧"
                ))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // 只有这几条测试要拼一条手工的 `SceneRun`，所以 `SceneFrame` 的导入留在这里：
    // 放到模块顶层的话，非测试构建会出现一条 unused import 警告——那是噪音，
    // 而"警告可以忍"一旦成立，真的警告也就没人看了。
    use dhampir_core::render::SceneFrame;
    use wasm_bindgen_test::wasm_bindgen_test;

    // 为什么是 `#[wasm_bindgen_test]` 而不是 `#[test]`：wasm32 下**只有**被
    // `#[wasm_bindgen_test]` 注册的测试会跑，普通 `#[test]` 编得进去却永不执行
    // （`scripts/run-wasm-tests.mjs` 按源码里的属性条数与运行清单对账，正是为了
    // 拦住这种"看起来有测试"）。所以这一模块里每一条都必须是前者。

    #[wasm_bindgen_test]
    fn leg_slug_allows_what_it_should_and_nothing_else() {
        assert!(check_leg_slug("browser-nvidia").is_ok());
        assert!(check_leg_slug("m2").is_ok());
        assert!(check_leg_slug("a1").is_ok());
        for bad in [
            "",
            "-x",
            "x-",
            "Browser",
            "browser_nvidia",
            "browser/../..",
            "browser\\nvidia",
            "browser nvidia",
            "浏览器",
        ] {
            assert!(check_leg_slug(bad).is_err(), "{bad:?} 本该被拒绝");
        }
        assert!(check_leg_slug(&"a".repeat(65)).is_err());
        assert!(check_leg_slug(&"a".repeat(64)).is_ok());
    }

    #[wasm_bindgen_test]
    fn a_missing_clock_or_a_negative_one_stays_zero() {
        assert_eq!(to_epoch_millis(0.0), 0);
        assert_eq!(to_epoch_millis(-1.0), 0);
        assert_eq!(to_epoch_millis(f64::NAN), 0);
        assert_eq!(to_epoch_millis(f64::INFINITY), 0);
        assert_eq!(to_epoch_millis(1.0e300), 0);
        assert_eq!(to_epoch_millis(1_790_000_000_123.0), 1_790_000_000_123);
    }

    /// 浏览器腿的 `adapter.json` 与 native 那份的四处差异，逐条钉住。
    ///
    /// 这四处都是"**如实标注不成立的东西**"，不是随手加的键：把它们写成测试，
    /// 是因为将来有人看到 `naga_version: null` 时会想"顺手填上 native 的版本号吧"
    /// —— 那一填就是一句假话。反向也要钉：`backend_slug` 必须**等于**调用方给的
    /// 腿名（而不是后端点标志派生的），否则"记录里的腿名 == 产物目录名"这条
    /// 可供守卫逐条检查的不变量就断了。
    #[wasm_bindgen_test]
    fn browser_adapter_json_declares_its_differences() {
        let fields = vec![
            ("name", "Fake Adapter".to_string()),
            ("backend", "BrowserWebGpu".to_string()),
        ];
        let in_page = parse_in_page(
            r#"{"vendor":"nvidia","architecture":"lovelace","device":"","description":"",
                "is_fallback_adapter":false,"subgroup_min_size":32}"#,
        )
        .expect("这份 in_page 是合法的");
        let record = browser_adapter_json(&fields, in_page, "m2", 1_790_000_000_123);

        assert_eq!(record["producer"], PRODUCER);
        assert_eq!(record["kind"], "adapter");
        // ① naga 不在浏览器的 WGSL 路径上：写 null，并且**说清为什么**，
        //    否则复核的人只能猜这是漏填还是故意。
        assert_eq!(record["naga_version"], Value::Null);
        assert!(
            record["naga_version_note"]
                .as_str()
                .is_some_and(|note| !note.is_empty()),
            "null 必须配一句说明"
        );
        // ② 本腿不出 timing.json，同样不许沉默地少一个文件。
        assert_eq!(record["timing_record"], Value::Null);
        assert!(
            record["timing_note"]
                .as_str()
                .is_some_and(|note| !note.is_empty())
        );
        // ③ 腿名由调用方给，不是后端位标志派生。
        assert_eq!(record["backend_slug"], "m2");
        assert_eq!(
            record["requested_backends"],
            gpu::backend_label(gpu::BROWSER_BACKENDS)
        );
        // ④ 两条腿沿用同一张表：`milestone` 是表的契约版本，不是产生它的里程碑。
        assert_eq!(record["milestone"], corpus::CORPUS_TABLE_MILESTONE);
        // 表里有两处名字不同的东西，别让 `adapter_name` 与 `adapter.name` 漂开。
        assert_eq!(record["adapter_name"], "Fake Adapter");
        assert_eq!(record["adapter"]["name"], "Fake Adapter");
        // 名字拿得到的时候**不该**出现身份缺口声明：那个键是"我答不出"的意思，
        // 每条腿都挂一个的话，读的人就再也分不清哪一条真的答不出了。
        assert!(
            record.get("gpu_identity").is_none(),
            "名字有了就不写 gpu_identity，收到 {:?}",
            record.get("gpu_identity")
        );
        // 时间戳：给了就两个键都给，`epoch_seconds` 由 core 截断（与 native 同一份）。
        assert_eq!(record["unix_epoch_millis"], 1_790_000_000_123_u64);
        assert_eq!(record["unix_epoch_seconds"], corpus::epoch_seconds(1_790_000_000_123));
    }

    /// Chrome 上 `adapter.name` 是空串（见 [`IN_PAGE_NOTE`]）：这份记录必须**自己说清**
    /// 它答不出"用了哪块卡"、答案该去哪找，而不是留一个空键让人猜。
    ///
    /// 反向也钉住：`in_page.vendor` 也有的时候，`gpu_identity` 就不该出现——它出现
    /// 意味着"这个名字没拿到"，把有名字的腿也标成没身份，等于让守卫无从判断。
    #[wasm_bindgen_test]
    fn an_empty_name_becomes_a_declared_identity_gap() {
        let fields = vec![
            ("name", String::new()),
            ("vendor", "0".to_string()),
            ("backend", "BrowserWebGpu".to_string()),
        ];
        let in_page = parse_in_page(
            r#"{"vendor":"nvidia","architecture":"lovelace","device":"","description":"",
                "is_fallback_adapter":false}"#,
        )
        .expect("这份 in_page 是合法的");
        let record = browser_adapter_json(&fields, in_page, "m2", 1_790_000_000_123);

        // 空名字写 null 而不是空串：这两个在 JSON 里读起来是两件事。
        assert_eq!(record["adapter"]["name"], "");
        assert_eq!(record["adapter_name"], Value::Null);
        // 缺口声明四件套，缺一条这份记录就没法被守卫检查。
        assert_eq!(record["gpu_identity"]["state"], "unresolved");
        assert!(
            record["gpu_identity"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("GPUAdapterInfo")),
            "理由里要说清是浏览器不给，而不是我们没读"
        );
        assert_eq!(record["gpu_identity"]["resolved_by"], "harness");
        // 指向的文件名必须与真写下去的那份**逐字相同**：这里的 `host-gpu.json`
        // 与驱动 POST 的文件名、与归档里的文件是同一个字符串。
        assert_eq!(record["gpu_identity"]["resolves_to"], "host-gpu.json");
        // 页面读到的那份原文必须原样在记录里：它是把 vendor 对到宿主某块卡的唯一依据。
        assert_eq!(record["in_page"]["vendor"], "nvidia");
        assert_eq!(record["in_page"]["architecture"], "lovelace");
        assert_eq!(record["in_page"]["device"], "");
        assert_eq!(record["in_page"]["is_fallback_adapter"], false);
        assert_eq!(record["in_page"]["subgroup_min_size"], Value::Null);
        assert!(
            record["in_page"]["note"]
                .as_str()
                .is_some_and(|note| note.contains("隐私")),
            "那一块得自己解释为什么 device/description 是空的"
        );
    }

    /// `parse_in_page` 的拒绝面：空 vendor、类型不对、少键、不是对象。
    ///
    /// 空的 vendor 是这里最要紧的一条——它是"把记录里的卡对到宿主某块卡"的第一环，
    /// 填个默认值就等于让后面所有环节都在一个假前提上跑。
    #[wasm_bindgen_test]
    fn a_broken_in_page_is_rejected_not_defaulted() {
        for (label, text, keyword) in [
            ("空 vendor", r#"{"vendor":"","architecture":"a","device":"","description":"","is_fallback_adapter":false}"#, "vendor"),
            ("少 vendor", r#"{"architecture":"a","device":"","description":"","is_fallback_adapter":false}"#, "vendor"),
            ("vendor 不是字符串", r#"{"vendor":1,"architecture":"a","device":"","description":"","is_fallback_adapter":false}"#, "vendor"),
            ("少 is_fallback_adapter", r#"{"vendor":"nvidia","architecture":"a","device":"","description":""}"#, "is_fallback_adapter"),
            ("is_fallback_adapter 不是布尔", r#"{"vendor":"nvidia","architecture":"a","device":"","description":"","is_fallback_adapter":"no"}"#, "is_fallback_adapter"),
            ("少 architecture", r#"{"vendor":"nvidia","device":"","description":"","is_fallback_adapter":false}"#, "architecture"),
            ("少 device", r#"{"vendor":"nvidia","architecture":"a","description":"","is_fallback_adapter":false}"#, "device"),
            ("少 description", r#"{"vendor":"nvidia","architecture":"a","device":"","is_fallback_adapter":false}"#, "description"),
            ("subgroup 不是整数", r#"{"vendor":"nvidia","architecture":"a","device":"","description":"","is_fallback_adapter":false,"subgroup_min_size":1.5}"#, "subgroup_min_size"),
            ("不是 JSON", "nvidia", "不是 JSON"),
            ("不是对象", "[]", "不是对象"),
        ] {
            let error = parse_in_page(text).expect_err(&format!("{label} 本该被拒绝"));
            assert!(
                error.contains(keyword),
                "{label}：拒绝理由要点名 {keyword}，收到 {error:?}"
            );
        }
        // 反面：缺 subgroup 是**允许**的（那是 Chrome 的扩展字段，别的浏览器没有），
        // 写成 null 而不是编一个数——不然"下限 32"会被抄到没有这个概念的浏览器上。
        let ok = parse_in_page(
            r#"{"vendor":"nvidia","architecture":"a","device":"","description":"","is_fallback_adapter":true,"extra":"丢掉"}"#,
        )
        .expect("缺 subgroup 不该被拒");
        assert_eq!(ok["subgroup_min_size"], Value::Null);
        assert_eq!(ok["subgroup_max_size"], Value::Null);
        assert_eq!(ok["is_fallback_adapter"], true);
        // 页面多给的键丢掉：记录的形状由这一份源码定，不由页面定。
        assert!(ok.get("extra").is_none());
    }

    /// 摘要里的 `frames[]` 必须与 `run.json` 的 `frames[]` **是同一批行**。
    ///
    /// 页面就是照摘要去逐帧 POST 的：摘要里的路径/摘要若与记录对不上，落盘的
    /// 文件名和记录里写的那一行就会各说各话，而两边的字节都"没错"。这里用一条
    /// 手工搭的 `SceneRun` 把它钉住——不需要 GPU，所以它在 CI 上也会跑。
    #[wasm_bindgen_test]
    fn the_summary_projects_the_same_rows_as_the_record() {
        let spec = SELECTABLE_SCENES.first().expect("场景注册表不该是空的");
        let run = SceneRun {
            frames: vec![SceneFrame {
                spec,
                frame: 3,
                digest: 0x1111_2222_3333_4444,
                repeat_digest: 0x1111_2222_3333_4444,
                png_digest: 0x5555_6666_7777_8888,
                png: vec![0x89, b'P', b'N', b'G'],
                points: Vec::new(),
            }],
        };
        let specs: Vec<&'static SceneSpec> = vec![spec];
        let frames = (3, 4);

        let leg = corpus::leg_json(&run, &specs, "BROWSER_WEBGPU", Some("Fake"), frames);
        let run_json = corpus::record_text(&leg).expect("记录必须可序列化");
        let readings = corpus::report_text(&run);
        let summary = run_summary(
            &run,
            &specs,
            frames,
            "m2",
            "BROWSER_WEBGPU",
            Some("Fake"),
            &run_json,
            &readings,
        );

        let from_record = leg["backends"][0]["frames"].as_array().expect("记录里该有帧表");
        let from_summary = summary["frames"].as_array().expect("摘要里该有帧表");
        assert_eq!(from_summary.len(), from_record.len());
        assert_eq!(from_summary.len(), 1, "这一条只搭了一帧");
        for (in_summary, in_record) in from_summary.iter().zip(from_record) {
            for key in ["scene", "frame", "png", "png_digest", "png_bytes"] {
                assert_eq!(
                    in_summary[key], in_record[key],
                    "摘要里的 {key} 与记录不一样"
                );
            }
        }
        assert_eq!(summary["frames_digest"], leg["backends"][0]["frames_digest"]);
        assert_eq!(summary["frames_per_scene"], leg["frames_per_scene"]);
        assert_eq!(summary["frame_range"], leg["frame_range"]);
        assert_eq!(summary["frames_per_scene"], 1);
        // 两份文本**原样**带出去：让页面自己拼 `run.json` 就等于给同一份记录
        // 写了第二个作者。
        assert_eq!(summary["run_json"].as_str(), Some(run_json.as_str()));
        assert_eq!(summary["readings"].as_str(), Some(readings.as_str()));
        assert_eq!(summary["leg_slug"], "m2");
    }
}
