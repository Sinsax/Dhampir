//! 离屏渲染 → 读回 → 写记录。
//!
//! 这是 M0 里唯一"真的出了一个像素"的地方，也是 M1 的种子：M1 要把它扩成
//! corpus 场景集 + 计时表 + 四种环境矩阵。
//!
//! # 记录文件是产物，不是日志
//!
//! 写出来的 `adapter.json` 会被长期留存、跨版本比较。所以：
//!
//! - 字段命名由本模块钉死（[`dhampir_core::gpu::describe_adapter`] 的原因）。
//!   **对象键会按字母序输出**：`serde_json` 默认用 `BTreeMap`，本仓库没开
//!   `preserve_order`。这不是缺陷——字母序同样是确定的、跨版本稳定的，
//!   而且更不容易因为有人调整插入顺序就产生无意义的 diff。别把它当插入序读，
//!   也别依赖插入序。
//! - 不含时间戳以外的任何非确定性内容
//! - 渲染两次的字节比较结果也写进去——**不一致要当发现记录，不是当失败吞掉**

use std::path::{Path, PathBuf};

use dhampir_core::gpu::{GpuContext, request_context};
use dhampir_core::render::{PROBE_CLEAR_COLOR, ProbeRenderer, ProbeSample, SampleExpectation};
use dhampir_core::wgpu;
use dhampir_core::{readback, timeline};

/// 重导出探针目标尺寸：`run.json` 里要写它，从 core 取能保证不会有人另抄一个常量。
pub use dhampir_core::render::PROBE_TARGET_SIZE;

/// 探针渲染用的目标格式。
///
/// 两端必须是同一个值。选 `Rgba8UnormSrgb` 而不是 `Bgra8UnormSrgb`：
/// 后者在 Windows 上更"原生"，但 M2 的两端要求像素格式统一，而 canvas
/// 的首选格式各平台不同——统一成 RGBA 可以少一次通道重排。
pub const PROBE_TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// 一次探针运行的完整结果。全部字段都要落盘。
#[derive(Debug)]
pub struct ProbeRun {
    /// 请求的后端。`None` 表示让 wgpu 自己选。
    pub requested_backend: Option<wgpu::Backends>,
    /// 渲染出的图像（紧密打包 RGBA8）。
    pub image: readback::Rgba8Image,
    /// 同一进程内渲染两次是否逐字节相同。
    pub deterministic_in_process: bool,
    /// `adapter.json` 的内容。
    pub adapter_json: serde_json::Value,
}

impl ProbeRun {
    /// 记录文件名（不带目录），后端名参与命名，避免不同后端的产物互相覆盖。
    pub fn stem(&self) -> String {
        let backend = self
            .adapter_json
            .get("backend")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        format!("probe-native-{backend}")
    }
}

/// 出图一次，读回，并做进程内确定性检查。
pub fn run_probe(backends: wgpu::Backends) -> Result<ProbeRun, Box<dyn std::error::Error>> {
    // `Instance::new` 按值收 descriptor（wgpu 30 起），且没有 `Default`——
    // 用 `new_without_display_handle()` 起底，只覆盖 `backends` 这一个字段。
    // 这也是**唯一**把后端选择写进代码的地方，宿主与宿主之间就分叉这么一行。
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });

    // 这个 `block_on` 之所以合法，是因为 `request_context` 里的 future 在 native 上
    // 第一次 poll 就会就绪（wgpu-core 的 adapter/device 申请本体是同步的）。
    // 真正需要等待的是读回，见 `readback::MapWait` 的文档。
    let ctx = pollster::block_on(request_context(&instance, None))?;
    let (first, second) = (render_once(&ctx)?, render_once(&ctx)?);
    let deterministic_in_process = first.pixels == second.pixels;

    Ok(ProbeRun {
        requested_backend: Some(backends),
        image: first,
        deterministic_in_process,
        adapter_json: adapter_json(&ctx, backends),
    })
}

/// 渲染一帧探针图并读回。每次都用全新的纹理，避免复用掩盖掉"帧间差异"。
fn render_once(ctx: &GpuContext) -> Result<readback::Rgba8Image, Box<dyn std::error::Error>> {
    let (width, height) = PROBE_TARGET_SIZE;
    let texture = ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dhampir probe target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: PROBE_TARGET_FORMAT,
        // COPY_SRC 是读回的前提。canvas 纹理通常没有这个用途——
        // 这正是 M2 要求"wasm 侧也不要从 canvas 抄像素"的原因。
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

    let renderer = ProbeRenderer::new(&ctx.device, PROBE_TARGET_FORMAT);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("dhampir probe encoder"),
        });
    renderer.render(&mut encoder, &view, wgpu::LoadOp::Clear(PROBE_CLEAR_COLOR));
    ctx.queue.submit([encoder.finish()]);

    Ok(pollster::block_on(readback::read_texture_rgba8(
        &ctx.device,
        &ctx.queue,
        &texture,
    ))?)
}

/// 组装 `adapter.json`。
///
/// 除了 adapter 自身的信息，还带上"这次出图是在什么条件下发生的"——
/// 少了后半截，这张 JSON 就只是一份硬件清单，没法用来解释任何差异。
fn adapter_json(ctx: &GpuContext, requested: wgpu::Backends) -> serde_json::Value {
    let mut map = serde_json::Map::new();

    for (key, value) in dhampir_core::gpu::describe_adapter(&ctx.adapter_info) {
        map.insert(key.to_string(), serde_json::Value::String(value));
    }

    map.insert(
        "requested_backends".into(),
        serde_json::Value::String(format!("{requested:?}")),
    );
    map.insert(
        "target_format".into(),
        serde_json::Value::String(format!("{PROBE_TARGET_FORMAT:?}")),
    );
    map.insert(
        "target_size".into(),
        serde_json::Value::String(format!("{}x{}", PROBE_TARGET_SIZE.0, PROBE_TARGET_SIZE.1)),
    );
    map.insert(
        "crate_version".into(),
        serde_json::Value::String(env!("CARGO_PKG_VERSION").into()),
    );
    map.insert(
        "probe_digest".into(),
        serde_json::Value::String(format!("{:016x}", timeline::probe_digest())),
    );
    map.insert(
        "probe_format_version".into(),
        serde_json::Value::Number(timeline::PROBE_FORMAT_VERSION.into()),
    );
    map.insert(
        "unix_epoch_seconds".into(),
        serde_json::Value::Number(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .into(),
        ),
    );

    serde_json::Value::Object(map)
}

/// 把一次运行的全部产物写进 `dir`。
///
/// 返回写出的文件列表——**调用方要把它打进记录**，否则"跑了什么"只存在于终端回滚里。
pub fn write_run_artifacts(dir: &Path, run: &ProbeRun) -> Result<Vec<PathBuf>, std::io::Error> {
    std::fs::create_dir_all(dir)?;
    let stem = run.stem();
    let mut written = Vec::new();

    let png = dir.join(format!("{stem}.png"));
    run.image
        .write_png(&png)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    written.push(png);

    let json = dir.join(format!("{stem}.adapter.json"));
    std::fs::write(&json, serde_json::to_string_pretty(&run.adapter_json)?)?;
    written.push(json);

    Ok(written)
}

/// 一个采样点的**声明 + 实测值**。
///
/// 声明（名字、坐标、该点必须成立的性质）来自
/// [`dhampir_core::render::probe_samples`]——**本模块既不发明坐标，也不发明期望**。
/// 两边各写一份的话，就会出现"记录下的点"和"断言的点"悄悄不是同一个点，
/// 而那正是本模块先前踩过的坑：三个"顶点附近"的点全落在三角形外面，
/// 断言报的是"渲染错了"，PNG 里三角形却画得好好的。
#[derive(Clone, Copy, Debug)]
pub struct SampleReading {
    pub sample: ProbeSample,
    /// 实测像素。`None` = 坐标越界（`.pixel()` 取不到）。
    pub rgba: Option<[u8; 4]>,
}

/// 探针的采样点 + 该点实际读到的像素值。
///
/// 本函数只负责**把值读出来**；判定在 [`check_samples`]，记录在调用方。
pub fn sample_points(image: &readback::Rgba8Image) -> Vec<SampleReading> {
    dhampir_core::render::probe_samples((image.width, image.height))
        .into_iter()
        .map(|sample| SampleReading {
            sample,
            rgba: image.pixel(sample.x, sample.y),
        })
        .collect()
}

/// 清屏色（线性 0.05）经 sRGB 编码到 8 位之后的落点区间。
///
/// 精确值（62 还是 63）**不在这里钉**——那属于容差与阈值的范畴，归 M2 的
/// `thresholds.toml`。这里只要"是灰的，既不是黑也不是白"。
pub const BACKGROUND_MIN: u8 = 40;
pub const BACKGROUND_MAX: u8 = 90;

/// 主导通道的下限。
///
/// 从**被钉死的几何**推出来：顶点内侧采样点的主导重心权重是 0.933、另两个各
/// 0.033（见 `probe_samples` 的文档表），于是 sRGB 8 位下主导通道约 247、
/// 另两个约 50。取 200 是给插值与编码留实现余量，同时离"混色"的 50 很远——
/// **这条线说的是"必须真的占优"，不是"约等于 247"**。
pub const DOMINANT_MIN: u8 = 200;

/// 主导通道相对最强次通道的最小领先量。按上述权重实测约 197。
pub const DOMINANT_MARGIN: u8 = 64;

/// 判定单个采样点是否满足它声明的性质。
///
/// **纯函数，不碰 GPU。** 判定逻辑必须能被单测钉住：否则"这条断言会不会红"
/// 只能靠真的把渲染搞坏来验，而没有人会去验——于是恒绿的守卫会一直看起来很正常。
/// 本模块 `tests` 里的 [`dominant_channel_must_dominate`] 与
/// [`background_must_be_neutral_and_clear_coloured`] 就钉着那些**必须判红**的反例。
pub fn judge_sample(expect: SampleExpectation, rgba: [u8; 4]) -> Result<(), String> {
    match expect {
        SampleExpectation::Background => {
            if rgba[0] < BACKGROUND_MIN || rgba[0] > BACKGROUND_MAX {
                return Err(format!(
                    "读到的像素是 {rgba:?}，不像清屏色（线性 0.05 编码到 sRGB 8 位应落在 \
                     {BACKGROUND_MIN}..={BACKGROUND_MAX}）"
                ));
            }
            if rgba[0] != rgba[1] || rgba[1] != rgba[2] {
                return Err(format!(
                    "读到的像素是 {rgba:?}，不是中性灰——sRGB 编码或通道顺序有问题"
                ));
            }
            Ok(())
        }
        SampleExpectation::Dominant(channel) => {
            let i = usize::from(channel);
            if i >= 3 {
                return Err(format!("主导通道下标越界：{channel}"));
            }
            let value = rgba[i];
            let runner_up = (0..3)
                .filter(|c| *c != i)
                .map(|c| rgba[c])
                .max()
                .unwrap_or(0);
            let lead = value.saturating_sub(runner_up);
            if value < DOMINANT_MIN {
                return Err(format!(
                    "读到的像素是 {rgba:?}，通道 {channel} 只有 {value}，低于下限 {DOMINANT_MIN}"
                ));
            }
            if lead < DOMINANT_MARGIN {
                return Err(format!(
                    "读到的像素是 {rgba:?}，通道 {channel} 领先最强的次通道 {runner_up} \
                     只有 {lead}，低于 {DOMINANT_MARGIN}——顶点数据或插值有问题"
                ));
            }
            Ok(())
        }
    }
}

/// 判定整组采样点。**纯函数**：给一组"声明 + 实测值"就能判，不需要 GPU。
///
/// 除了逐点判定，还要检查采样集合自己的**覆盖度**：`probe_samples` 如果哪天少给了
/// 一个点，上面那个逐点循环会安安静静地少查一项——"少了一个采样点"不会有任何症状。
pub fn check_samples(samples: &[SampleReading], image_size: (u32, u32)) -> Result<(), String> {
    if image_size != PROBE_TARGET_SIZE {
        return Err(format!(
            "渲染目标是 {image_size:?}，与钉死的 {PROBE_TARGET_SIZE:?} 不符——\
             采样坐标是随尺寸算出来的，历史记录会跟着漂"
        ));
    }

    let mut backgrounds = 0_usize;
    let mut channels_seen = [false; 3];
    for reading in samples {
        match reading.sample.expect {
            SampleExpectation::Background => backgrounds += 1,
            SampleExpectation::Dominant(channel) => {
                match channels_seen.get_mut(usize::from(channel)) {
                    Some(seen) => *seen = true,
                    None => {
                        return Err(format!(
                            "采样点 {} 的主导通道下标越界：{channel}",
                            reading.sample.name
                        ));
                    }
                }
            }
        }
    }
    // 背景点要两个：只取一个的话，"画面整块平移"这类错误会被漏掉（见 `probe_samples`）。
    if backgrounds < 2 {
        return Err(format!(
            "只有 {backgrounds} 个背景采样点——至少要两个，否则整块平移的错误会被漏掉"
        ));
    }
    for (channel, seen) in channels_seen.iter().enumerate() {
        if !seen {
            return Err(format!(
                "没有任何采样点声明通道 {channel} 占优——那个通道的颜色没被验过"
            ));
        }
    }

    for reading in samples {
        let (x, y) = (reading.sample.x, reading.sample.y);
        let rgba = reading
            .rgba
            .ok_or_else(|| format!("采样点 {} ({x}, {y}) 越界", reading.sample.name))?;
        judge_sample(reading.sample.expect, rgba)
            .map_err(|e| format!("采样点 {} ({x}, {y})：{e}", reading.sample.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifacts_use_backend_specific_names() {
        let run = ProbeRun {
            requested_backend: Some(wgpu::Backends::DX12),
            image: readback::Rgba8Image {
                width: 1,
                height: 1,
                pixels: vec![0, 0, 0, 255],
            },
            deterministic_in_process: true,
            adapter_json: serde_json::json!({ "backend": "Vulkan" }),
        };
        // 覆盖不同后端的产物是最容易犯的错之一：两个后端都跑完了，
        // 打开 records/ 却只剩一个文件——因为后跑的把先跑的盖了。
        assert_eq!(run.stem(), "probe-native-vulkan");
    }

    /// 探针图上真实读到的值（RTX 4070 / DX12，`records/m0` 那一轮）。
    ///
    /// 写成一个具名常量而不是散在各处的字面量：下面所有反例都是"相对正确值
    /// 改动一处"，改错了基准值会让整组测试一起失去意义。
    const SEEN_RED: [u8; 4] = [247, 52, 50, 255];
    const SEEN_GREEN: [u8; 4] = [50, 247, 50, 255];
    const SEEN_BLUE: [u8; 4] = [50, 50, 247, 255];
    const SEEN_BACKGROUND: [u8; 4] = [63, 63, 63, 255];

    /// **这条测试的作用是证明断言不是恒绿的。**
    ///
    /// 每个反例都对应一种真实故障：三角形没画出来、顶点颜色被置换、
    /// 插值坏的、通道顺序反的、目标纹理压根没被写过。没有这组反例，
    /// "判定写对了"就只能靠"它这次没报错"来推断——那正是恒绿守卫的温床。
    #[test]
    fn dominant_channel_must_dominate() {
        // 正确值必须过：不是恒红，得先证明它会绿。
        assert!(judge_sample(SampleExpectation::Dominant(0), SEEN_RED).is_ok());
        assert!(judge_sample(SampleExpectation::Dominant(1), SEEN_GREEN).is_ok());
        assert!(judge_sample(SampleExpectation::Dominant(2), SEEN_BLUE).is_ok());

        // 三角形没画出来：三个采样点读到的都是清屏色。**M0 第一轮的真实症状。**
        let reason = judge_sample(SampleExpectation::Dominant(0), SEEN_BACKGROUND).unwrap_err();
        assert!(
            reason.contains("低于下限"),
            "报错要说清楚是低于下限：{reason}"
        );

        // 顶点颜色被置换（红蓝倒过来）。
        assert!(judge_sample(SampleExpectation::Dominant(1), SEEN_RED).is_err());
        // 插值坏掉：两通道同高，谁也不占优。
        assert!(judge_sample(SampleExpectation::Dominant(0), [200, 200, 50, 255]).is_err());
        // 只"稍微大一点"：够大，但不够"占优"。
        assert!(judge_sample(SampleExpectation::Dominant(0), [210, 190, 60, 255]).is_err());
        // 通道下标越界不能 panic——它是一条会走到错误分支的输入。
        assert!(judge_sample(SampleExpectation::Dominant(7), SEEN_RED).is_err());
    }

    #[test]
    fn background_must_be_neutral_and_clear_coloured() {
        assert!(judge_sample(SampleExpectation::Background, SEEN_BACKGROUND).is_ok());

        // 全黑：`Clear` 没生效，或者目标纹理压根没被写过。
        assert!(judge_sample(SampleExpectation::Background, [0, 0, 0, 255]).is_err());
        // 偏色：通道顺序或 sRGB 编码有问题。注意 70 仍在 40..=90 里，
        // 拦住它的是"中性灰"那一条——两个条件都得有。
        let reason = judge_sample(SampleExpectation::Background, [63, 63, 70, 255]).unwrap_err();
        assert!(reason.contains("中性灰"), "报错要指向中性灰：{reason}");
        // 背景采样点其实落在三角形里 = 采样坐标又算错了。**也是真实发生过的。**
        assert!(judge_sample(SampleExpectation::Background, SEEN_RED).is_err());
        // 白：清屏色不可能编码到 255。
        assert!(judge_sample(SampleExpectation::Background, [255, 255, 255, 255]).is_err());
    }

    /// 一块清屏灰的目标图像，只用于"覆盖度 / 越界 / 报错格式"这类与颜色无关的用例。
    fn gray_target() -> readback::Rgba8Image {
        let (w, h) = PROBE_TARGET_SIZE;
        readback::Rgba8Image {
            width: w,
            height: h,
            pixels: [63, 63, 63, 255].repeat((w * h) as usize),
        }
    }

    /// 覆盖度检查：采样点**少了一个**是无声的，必须有东西喊出来。
    #[test]
    fn missing_samples_are_caught_by_the_coverage_check() {
        let full = sample_points(&gray_target());
        assert_eq!(full.len(), 5, "钉死的采样点就是五个");

        let verdict = |samples: &[SampleReading]| check_samples(samples, PROBE_TARGET_SIZE);

        // 完整集合：错的只该是像素值，不该是"缺采样点"。
        let reason = verdict(&full).unwrap_err();
        assert!(
            reason.contains("near_vertex_red"),
            "灰底图应当报在具体的采样点上：{reason}"
        );
        assert!(!reason.contains("背景采样点"), "{reason}");
        assert!(!reason.contains("没有任何采样点"), "{reason}");

        // 抽掉一个通道的采样点 → 那个通道再也没人验。
        let missing_blue: Vec<SampleReading> = full
            .iter()
            .copied()
            .filter(|r| r.sample.name != "near_vertex_blue")
            .collect();
        let reason = verdict(&missing_blue).unwrap_err();
        assert!(
            reason.contains("通道 2"),
            "抽掉蓝点要在通道 2 上报错：{reason}"
        );

        // 只剩一个背景点 → "画面整块平移"会被漏掉。
        let one_background: Vec<SampleReading> = full
            .iter()
            .copied()
            .filter(|r| r.sample.name != "background_bottomright")
            .collect();
        let reason = verdict(&one_background).unwrap_err();
        assert!(
            reason.contains("背景采样点"),
            "只剩一个背景点要报错：{reason}"
        );

        // 尺寸不是钉死的那个 → 记录会漂，直接拒绝。
        let reason = check_samples(&full, (128, 128)).unwrap_err();
        assert!(reason.contains("与钉死的"), "{reason}");
    }

    /// 越界的采样点必须报"越界"，而不是 panic、也不是被当成颜色错。
    ///
    /// 覆盖度检查跑在值检查**前面**，所以这里要给一组覆盖度合格的采样点，
    /// 只把其中一个的实测值改成 `None`——这正是 `.pixel()` 越界时的样子。
    #[test]
    fn out_of_range_samples_are_reported_as_such() {
        let mut samples = sample_points(&gray_target());
        samples[0].rgba = None;
        let reason = check_samples(&samples, PROBE_TARGET_SIZE).unwrap_err();
        assert!(reason.contains("越界"), "{reason}");
    }
}
