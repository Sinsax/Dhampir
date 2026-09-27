//! 音频通路：**与视频同源求值**的 AudioPlan，以及帧号 ↔ 采样点之间的**有理数**换算。
//!
//! # 这个模块为什么是纯的
//!
//! 它一个子进程都不起，也不碰 GPU 与文件。于是"帧号换成第几个采样点"这件事
//! 可以被脱离显卡、脱离 ffmpeg 地单测 —— 而那正是整条音频腿上**唯一可能悄悄错**的地方：
//! 差一个采样点没人看得出来，差半秒就是音画不同步，而音画不同步是查起来最费劲的一类。
//!
//! ffmpeg 那一侧在 [`crate::pipeline`]（读采样、编 AAC、与视频复用）。
//!
//! # 有理数，不是浮点秒
//!
//! 「帧号与采样数用有理数换算」是硬要求，不是风格偏好。理由与视频那次一样：
//! 30000/1001 这类帧率下，`frames as f64 / 29.97 * 48000.0` 会在第 30000 帧附近
//! 攒出**一个采样点**的偏移；而"差一个采样点"在两端各算一次就会变成"差两个"。
//! 这里全程走 `i128` 中间量：`采样点 = frame * den * rate / num`，**向下取整**
//! （与 `source_frame_at` 同一个取整方向，理由见那里的注释）。
//!
//! # 「同源求值」这句话的确切含义
//!
//! 音频段与视频层用**同一条**选片规则（`Layer::covers`，左闭右开）与同一个起点
//! （`source_in`），区别只有一处：
//!
//! * 视频把时间**量化到素材的帧**（`source_frame_at`，向下取整）；
//! * 音频不量化 —— 它按**连续时间**取采样点。
//!
//! 前者是对的（画面只能以帧为单位），后者也是对的（声音没有帧）。
//! 两者取自同一个时间点，所以不会漂：`source_in` 的时间 + 片段内偏移的时间。
//! 单元测试里有一条专门盯着这个等式。

use std::path::PathBuf;

use dhampir_core::timeline::layer::{AssetTimebases, Layer, TimelineV2};
use dhampir_core::timeline::schema::{Frame, Issue, TimebaseDto, TrackKind};
use dhampir_media::{AudioInfo, SampleFormat};

use crate::pipeline::SourceTable;

/// 输出音轨的采样率。**写死一个值**，理由：混音要求所有输入同率，
/// 而"跟着第一个素材走"会让同一份工程在不同素材顺序下产出不同规格的产物 ——
/// 那种不确定性换不来任何东西。
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;

/// 输出音轨的声道数。同上：不跟着素材走。
pub const AUDIO_CHANNELS: u16 = 2;

/// 一路音频解码器吐出来的采样格式。
///
/// **与契约里的 [`SampleFormat::F32Planar`] 是同一个词吗？不完全是。**
/// 契约那个词说的是 in-process 实现（拿 `Vec<f32>` 分平面给编码器）；
/// 而子进程这条腿上，ffmpeg 的裸格式名 `f32le` 是**交错**的。
/// 差别是"平面 / 交错"，两者都是 32 位浮点。写清楚，不假装是同一个东西。
pub const AUDIO_PCM_FORMAT: &str = "f32le";

/// 输出音频的规格。**它就是 [`dhampir_media::AudioInfo`]** ——
/// 契约里已经声明过这个词表，音频腿不该另造一套平行的。
pub const fn output_audio_info() -> AudioInfo {
    AudioInfo {
        sample_rate: AUDIO_SAMPLE_RATE,
        channels: AUDIO_CHANNELS,
        sample_format: SampleFormat::F32Planar,
    }
}

/// 第 `frame` 帧**起点**对应的采样点号。
///
/// `采样点 = frame * den * rate / num`，`i128` 中间量、`div_euclid` 向下取整。
/// **不经过浮点秒**：`f64` 在 30000/1001 上攒得出一个采样点的偏移。
pub fn sample_at_frame(
    frame: Frame,
    timebase: &TimebaseDto,
    sample_rate: u32,
) -> Result<i64, String> {
    if timebase.num == 0 {
        return Err(format!(
            "时间基不合法：{}/{}（分母为 0 时算不出采样点）",
            timebase.num, timebase.den
        ));
    }
    let numerator =
        i128::from(frame) * i128::from(timebase.den) * i128::from(sample_rate);
    let denominator = i128::from(timebase.num);
    let scaled = numerator.div_euclid(denominator);
    Ok(scaled.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// `[from, to]` 这段**闭区间帧**一共多少个采样点。
///
/// 按 `to + 1` 再相减算，而不是"每帧的采样数乘帧数" ——
/// 后者在非整数帧率下会把每帧的余数各自丢掉，攒起来就是可听见的偏移。
pub fn samples_in_range(
    from: Frame,
    to: Frame,
    timebase: &TimebaseDto,
    sample_rate: u32,
) -> Result<i64, String> {
    if to < from {
        return Ok(0);
    }
    let start = sample_at_frame(from, timebase, sample_rate)?;
    let end = sample_at_frame(to.saturating_add(1), timebase, sample_rate)?;
    Ok(end.saturating_sub(start))
}

/// 时间线上的一段音频：**在输出轨上占哪一段、从素材的哪个采样点开始读**。
///
/// 两套坐标都在这里，因为"同源求值"这句话要能核对：
/// `timeline_start..timeline_end` 是输出坐标，`source_start_sample` 是素材坐标。
///
/// **不派生 `Eq`**：`gain` 是 `f32`（T13 引入）。与 `AudioPlan` 同一条理由 ——
/// 判等用 `PartialEq` 就够，强行造 `Eq`（比如把增益比成位模式）
/// 会让 `NaN != NaN` 这类事变成"两段看起来一样却不相等"。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSegment {
    /// 音轨 id 与图层 id。**只用于报错与去重**，不参与运算。
    pub track: String,
    pub layer: String,
    pub asset_id: String,
    pub file: PathBuf,
    /// 输出坐标：这一段覆盖 `[timeline_start, timeline_end)`（左闭右开，与 `covers` 同）。
    pub timeline_start: Frame,
    pub timeline_end: Frame,
    /// 这一段从输出轨的第几个采样点开始写。
    ///
    /// **记成偏移而不是让消费方再算一遍**：组装音轨的人（pipeline）手上只有这一段，
    /// 让他自己去查时间基换算，等于把同一个算式的第二份实现放出去 ——
    /// 而那正是"两处算法慢慢分叉"最常见的起点。
    pub output_start_sample: i64,
    /// 这一段在输出轨上占多少采样点（时间线侧有理数算出）。
    pub output_samples: i64,
    /// 素材坐标：从素材的第几个采样点开始读（`source_in` 的时间 + 片段内偏移的时间）。
    ///
    /// **是"起始点"，不是"起点"** —— 这个采集点之前的声音不会被这一趟用到。
    pub source_start_sample: i64,
    /// 这一段的增益（线性倍数，1.0 = 原样）。
    ///
    /// # 为什么带增益而不是"混音时再乘"
    ///
    /// 音效（SFX）天生需要它：一个"叮"压在背景人声上时，两段相加会削顶。
    /// 削顶是**听着像坏了的**那种错（爆音），而不是"稍微响了一点"。
    /// 把倍数放在**计划**里，它就与采样点、时长一样是可单测的纯数据；
    /// 放到混音那一层就变成"只有跑了 ffmpeg 才知道对不对"。
    pub gain: f32,
}

impl AudioSegment {
    /// 这一段的时长（秒）。**给诊断用**：报错时"这一段多长"是最常问的一句。
    pub fn seconds(&self, sample_rate: u32) -> f64 {
        if sample_rate == 0 {
            return 0.0;
        }
        self.output_samples as f64 / f64::from(sample_rate)
    }
}

impl AudioSegment {
    /// 给"这一段为什么在这儿"留一句人能读的话。
    pub fn describe(&self) -> String {
        format!(
            "{}/{}: 时间线 [{}..{}) = 输出采样点 [{}..{}) 共 {} 个, 素材 {} 起于采样点 {}",
            self.track,
            self.layer,
            self.timeline_start,
            self.timeline_end,
            self.output_start_sample,
            self.output_start_sample + self.output_samples,
            self.output_samples,
            self.asset_id,
            self.source_start_sample
        )
    }
}

/// 这一趟出片的音频计划。
///
/// `total_samples` 是**算出来的**（由 `from/to` 与时间基有理数算出），
/// 不是"把各段加起来" —— 于是「音轨时长 == 视频时长」是结构保证，
/// 而不是靠事后对齐。段与段之间的空档由**静音**填，填多少也是算出来的。
/// 不派生 `Eq`：问题清单里的 `Issue` 只实现了 `PartialEq`（它是给人读的文本，
/// 不是可比较的键）。段那一层是 `Eq`，判等用它。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioPlan {
    pub info: AudioInfo,
    pub from: Frame,
    pub to: Frame,
    /// 这一趟输出的第一个采样点在时间线上的绝对位置。
    pub start_sample: i64,
    pub total_samples: i64,
    pub segments: Vec<AudioSegment>,
    /// 哪些段在时间线上叠在一起（会被**相加**，不是二选一）。
    ///
    /// 它不参与"能不能成功"的判定 —— 音效叠在背景音上是正常的（那是 SFX 的定义）。
    /// 记下来是因为**加法会削顶**，而削顶要看得见才知道该不该调增益。
    pub overlaps: Vec<AudioOverlap>,
    /// 装载阶段就发现的问题（音轨缺素材、音轨上没有素材的图层）。
    /// 非空 = 这次出片**不许成功** —— 与其它问题清单同一条纪律。
    pub issues: Vec<Issue>,
}

impl AudioPlan {
    /// 什么都不出声的那一份。视频照常出，音轨不做。
    ///
    /// 它**不是**"空对象"：`total_samples` 仍然是这一趟的采样数，
    /// 于是"有没有音轨"和"这片子多长"是两个可以分开问的问题。
    pub fn silent(from: Frame, to: Frame, timebase: &TimebaseDto) -> Result<Self, String> {
        let start_sample = sample_at_frame(from, timebase, AUDIO_SAMPLE_RATE)?;
        Ok(Self {
            info: output_audio_info(),
            from,
            to,
            start_sample,
            total_samples: samples_in_range(from, to, timebase, AUDIO_SAMPLE_RATE)?,
            segments: Vec::new(),
            overlaps: Vec::new(),
            issues: Vec::new(),
        })
    }

    /// 有没有一段真的要出声。
    pub fn is_silent(&self) -> bool {
        self.segments.is_empty()
    }

    /// 各段覆盖不到的采样点数 —— 会被静音填上。
    pub fn gap_samples(&self) -> i64 {
        let covered: i64 = self.segments.iter().map(|s| s.output_samples).sum();
        (self.total_samples - covered).max(0)
    }

    /// 需要开几路素材（按 id 去重，给"这一趟要起几个进程"用）。
    pub fn distinct_assets(&self) -> usize {
        let mut ids: Vec<&str> = self.segments.iter().map(|s| s.asset_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len()
    }
}

/// 按工程文件与帧区间摊出 AudioPlan。**纯的**：不碰 GPU、不碰 ffmpeg、不读文件。
///
/// 选片规则与视频**逐条相同**：只看 `kind == Audio` 的轨，
/// 只看 `Layer::covers` 那一套左闭右开，只取与 `[from, to + 1)` 的交集。
pub fn plan_audio(
    timeline: &TimelineV2,
    sources: &SourceTable,
    asset_timebases: &AssetTimebases,
    from: Frame,
    to: Frame,
) -> Result<AudioPlan, String> {
    let timebase = &timeline.timebase;
    let mut plan = AudioPlan::silent(from, to, timebase)?;
    if to < from {
        return Ok(plan);
    }
    let last = to.saturating_add(1);

    for track in timeline
        .tracks
        .iter()
        .filter(|track| track.kind == TrackKind::Audio)
    {
        for layer in &track.layers {
            // 与视频同一条规则：左闭右开，取交集。
            let low = layer.start.max(from);
            let high = layer.end.min(last);
            if low >= high {
                continue;
            }
            let path = format!("{}[{}]", track.id, layer.id);
            let source = match &layer.source {
                Some(source) => source,
                None => {
                    // 音轨上没有素材的图层：视频那边那是"调整图层"（只有特效），
                    // 音频这边没有任何等价物。**不许静默跳过** —— 静默跳过等于
                    // "这条音轨安静地不出声"，而人会以为它出了声。
                    plan.issues.push(Issue::new(
                        "audio_layer_without_source",
                        &path,
                        "音轨上的图层没有素材引用 —— 音频没有「调整图层」这回事，请删掉它或给它一个素材".to_string(),
                    ));
                    continue;
                }
            };
            let file = match sources.file_for(&source.asset_id) {
                Some(file) => file.to_path_buf(),
                None => {
                    plan.issues.push(Issue::new(
                        "audio_source_missing",
                        &path,
                        format!(
                            "音轨引用的素材 {} 没有登记文件位置 —— 出不了声音，不许当成静音",
                            source.asset_id
                        ),
                    ));
                    continue;
                }
            };
            // 素材自己的时间基（工程文件的 assets 表里那条）。
            // 缺了就退回时间线的时间基 —— 那是"素材帧率与时间线一致"的旧语义。
            let asset_timebase = asset_timebases
                .get(&source.asset_id)
                .unwrap_or(timebase);

            // 「同源求值」的落点：起点 = source_in 的**时间** + 片段内偏移的**时间**。
            // 视频那边取的是同一个时间点、再量化到素材的帧；音频不量化。
            let source_start_sample = sample_at_frame(
                source.source_in,
                asset_timebase,
                AUDIO_SAMPLE_RATE,
            )?
            .saturating_add(sample_at_frame(
                low.saturating_sub(layer.start),
                timebase,
                AUDIO_SAMPLE_RATE,
            )?);
            let output_samples = sample_at_frame(high, timebase, AUDIO_SAMPLE_RATE)?
                .saturating_sub(sample_at_frame(low, timebase, AUDIO_SAMPLE_RATE)?);
            let output_start_sample = sample_at_frame(low, timebase, AUDIO_SAMPLE_RATE)?
                .saturating_sub(plan.start_sample);

            plan.segments.push(AudioSegment {
                track: track.id.clone(),
                layer: layer.id.clone(),
                asset_id: source.asset_id.clone(),
                file,
                timeline_start: low,
                timeline_end: high,
                output_start_sample,
                output_samples,
                source_start_sample,
                // 契约里每层增益在 `Layer.gain`（默认 1.0），再乘**轨道的母线增益**
                // （`TrackV2.gain`，默认也是 1.0）。两者相乘 —— 图层是"这一段多响"，
                // 轨道是"这一整条一起调"。
                gain: layer.gain * track.gain,
            });
        }
    }

    // 确定性：先按时间线位置，再按轨/图层名。同一份工程跑两次必须逐字节一样。
    plan.segments
        .sort_by(|a, b| (&a.timeline_start, &a.track, &a.layer).cmp(&(&b.timeline_start, &b.track, &b.layer)));

    // 同区段重叠 = 要混音。
    //
    // **曾经这里是"判失败"**，理由是"静默取一条会产出一份听着像对的错产物"。
    // 那个理由至今成立 —— 所以这里仍然不许静默丢；但现在改成**真的混**，
    // 因为音效（SFX）天生就是叠在背景音上的：一个"叮"不与任何人声重叠才是怪事。
    // 判失败等于"音效这个功能永远做不了"。
    //
    // 混音本身在 `pipeline::build_audio_track`（那里才碰文件与字节）。
    // 这里只把"这一段要与谁相加"标出来，于是它是纯数据、可单测。
    for pair in plan.segments.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if b.timeline_start < a.timeline_end {
            // 重叠仍然要**记一笔**：混音是加法，加多了会削顶，
            // 而削顶是"听着像坏了"的那类错。记下来让人能看见它发生了。
            plan.overlaps.push(AudioOverlap {
                first: format!("{}[{}]", a.track, a.layer),
                second: format!("{}[{}]", b.track, b.layer),
                at: b.timeline_start,
                samples: a.timeline_end.min(b.timeline_end) - b.timeline_start,
            });
        }
    }

    Ok(plan)
}

/// 两段音频在时间线上叠在一起 —— **会被相加**，记下来供诊断。
///
/// 它**不是**错误：音效就是叠上去的。它是"这里做了加法"的一条记录，
/// 因为加法可能削顶，而削顶只有看见了才知道该不该调增益。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioOverlap {
    pub first: String,
    pub second: String,
    /// 从时间线的哪一帧开始叠。
    pub at: Frame,
    /// 叠了多少帧。
    pub samples: Frame,
}

/// 视频那一侧给某一帧取的是素材的哪一帧 —— 音频用它来核对"同一个时间点"。
///
/// 这个函数**不参与出片**，只给单元测试与诊断用：它让"音视频取自同一时间点"
/// 这句话有一个可执行的判据，而不是一句注释。
pub fn video_source_frame_at(
    layer: &Layer,
    frame: Frame,
    timeline: &TimebaseDto,
    asset: &TimebaseDto,
) -> Result<Frame, String> {
    let Some(source) = &layer.source else {
        return Err("这一层没有素材".to_string());
    };
    let local = frame.saturating_sub(layer.start);
    dhampir_core::timeline::layer::source_frame_at(source.source_in, local, timeline, asset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dhampir_core::timeline::layer::{Layer, SourceRef, TimelineV2, TrackV2};
    use dhampir_core::timeline::schema::TimebaseDto;

    fn tb(num: u32, den: u32) -> TimebaseDto {
        TimebaseDto { num, den }
    }

    fn asset_tb(asset_id: &str, num: u32, den: u32) -> AssetTimebases {
        let mut table = AssetTimebases::new();
        table.insert(asset_id, tb(num, den));
        table
    }

    fn sources(rows: &[(&str, &str)]) -> SourceTable {
        let mut table = SourceTable::new();
        for (id, file) in rows {
            table.insert(*id, *file);
        }
        table
    }

    /// 一条**实拍**图层（有素材）。`Layer` 没有 `Default`，所以在这里补齐那些
    /// 与音频无关的字段 —— 一次，而不是每个用例各写一遍。
    fn clip_layer(id: &str, start: Frame, end: Frame, asset_id: &str, source_in: Frame) -> Layer {
        Layer {
            id: id.to_string(),
            start,
            end,
            transform: Default::default(),
            opacity: 1.0,
            blend: Default::default(),
            enabled: true,
            recorded: Default::default(),
            gain: 1.0,
            source: Some(SourceRef {
                asset_id: asset_id.to_string(),
                source_in,
            }),
            loop_source: false,
            effects: Vec::new(),
            transition_in: None,
            keyframes: Vec::new(),
        }
    }

    /// 一条音轨、一个片段的工程。
    fn one_clip(
        start: Frame,
        end: Frame,
        source_in: Frame,
        asset_id: &str,
        timebase: TimebaseDto,
    ) -> TimelineV2 {
        TimelineV2 {
            schema: 3,
            timebase,
            markers: Vec::new(),
            tracks: vec![TrackV2 {
                id: "a1".to_string(),
                kind: TrackKind::Audio,
                layers: vec![clip_layer("clip", start, end, asset_id, source_in)],
                subtitle: None,
                danmaku: None,
                gain: 1.0,
            }],
        }
    }

    #[test]
    fn 一帧是多少采样点是有理数算出来的() {
        // 30fps：一帧 = 1/30 秒 = 1600 个采样点（48000 / 30）。
        assert_eq!(sample_at_frame(0, &tb(30, 1), 48_000).unwrap(), 0);
        assert_eq!(sample_at_frame(1, &tb(30, 1), 48_000).unwrap(), 1600);
        assert_eq!(sample_at_frame(30, &tb(30, 1), 48_000).unwrap(), 48_000);
    }

    #[test]
    fn 非整数帧率上也是精确的而不是浮点攒出来的() {
        // 30000/1001（29.97）：第 30000 帧应当是整整 1001 秒 * 48000。
        let exact = 1001i64 * 48_000;
        assert_eq!(sample_at_frame(30_000, &tb(30_000, 1001), 48_000).unwrap(), exact);

        // 现在把浮点抓出来。**比的是同一个取整方向**（都向下取整），
        // 否则量到的是"round 与 floor 的差"，那是另一件事（见 source_frame_at 的注释）。
        let floor_via_float = |frame: i64| -> i64 {
            (frame as f64 * 1001.0 / 30_000.0 * 48_000.0).floor() as i64
        };
        let precise = sample_at_frame(15, &tb(30_000, 1001), 48_000).unwrap();
        let noisy = floor_via_float(15);
        // 实测：29.97 下**第 15 帧（半秒）**浮点就已经少了一个采样点。
        // 这不是"很久以后才会偏"—— 半秒就偏。这就是"不许用浮点秒"的理由。
        assert_ne!(
            noisy, precise,
            "浮点在第 15 帧就该已经偏了；如果这条不再成立，说明这条判据失去了示例，请换一档帧率"
        );
        assert_eq!(noisy + 1, precise, "偏的方向是少一个采样点");
    }

    #[test]
    fn 帧区间换算与逐帧累加一致() {
        // 30fps 下 [0, 89] 共 90 帧 = 3 秒 = 144000 个采样点。
        assert_eq!(samples_in_range(0, 89, &tb(30, 1), 48_000).unwrap(), 144_000);
        // 非整数帧率：区间换算与"每帧各自取整再相加"**不该**一致 ——
        // 后者是那个会攒出偏移的做法。
        let tb2997 = tb(30_000, 1001);
        let range = samples_in_range(0, 89, &tb2997, 48_000).unwrap();
        let mut per_frame = 0i64;
        for frame in 0..90 {
            let a = sample_at_frame(frame, &tb2997, 48_000).unwrap();
            let b = sample_at_frame(frame + 1, &tb2997, 48_000).unwrap();
            per_frame += b - a; // 这里其实等价，写出来是为了对照"每帧固定 1601.6 个"那种做法
        }
        assert_eq!(range, per_frame, "区间换算必须与逐帧累加一致（都不丢余数）");
        let naive = 90 * (48_000 * 1001 / 30_000); // 每帧固定取整 = 1601
        assert_ne!(range, naive, "每帧固定取整会丢余数 —— 90 帧就少了 54 个采样点");
    }

    #[test]
    fn 零时间基是错误不是零() {
        assert!(sample_at_frame(10, &tb(0, 1), 48_000).is_err());
        assert!(samples_in_range(0, 10, &tb(0, 1), 48_000).is_err());
    }

    #[test]
    fn 空工程不出声但时长仍然算得出来() {
        let timeline = TimelineV2 {
            schema: 3,
            timebase: tb(30, 1),
            markers: Vec::new(),
            tracks: Vec::new(),
        };
        let plan = plan_audio(&timeline, &sources(&[]), &AssetTimebases::new(), 0, 89).unwrap();
        assert!(plan.is_silent());
        assert_eq!(plan.total_samples, 144_000);
        assert_eq!(plan.gap_samples(), 144_000, "全是空档 = 全是静音");
        assert!(plan.issues.is_empty());
    }

    #[test]
    fn 一段音频的起点是素材时间加片段内偏移() {
        // 时间线 30fps、素材 30fps、source_in = 30（= 素材里第 1 秒）。
        let timeline = one_clip(10, 40, 30, "tone", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            89,
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 1);
        let segment = &plan.segments[0];
        // source_in=30 帧 = 1 秒 = 48000；片段内偏移 10-10=0 -> 起于 48000。
        assert_eq!(segment.source_start_sample, 48_000);
        // [10, 40) 共 30 帧 = 1 秒 = 48000 个采样点。
        assert_eq!(segment.output_samples, 48_000);
        assert_eq!(segment.timeline_start, 10);
        assert_eq!(segment.timeline_end, 40);
        // 输出轨从第 0 帧起算，所以这一段落在输出采样点 [16000, 64000)。
        assert_eq!(segment.output_start_sample, 16_000);
        // 这一趟出 [0, 89] 共 90 帧 = 144000 个采样点；只有 [10, 40) 有声音，
        // 于是空档 = 144000 - 48000 = 96000（前 10 帧 + 后 50 帧）。
        assert_eq!(plan.gap_samples(), 96_000);
        assert_eq!(plan.total_samples, 144_000);
    }

    #[test]
    fn 片段内偏移也进起点() {
        let timeline = one_clip(0, 30, 0, "tone", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            10,
            19,
        )
        .unwrap();
        let segment = &plan.segments[0];
        // 这一趟只出 [10, 19]：片段内偏移 10 帧 = 16000，source_in=0 -> 起于 16000。
        assert_eq!(segment.source_start_sample, 16_000);
        assert_eq!(segment.output_samples, 16_000);
        assert_eq!(segment.timeline_start, 10);
        assert_eq!(segment.timeline_end, 20);
        // 这一趟从第 10 帧起算，段也正好从第 10 帧起 —— 于是它落在输出轨的最前面。
        assert_eq!(segment.output_start_sample, 0);
    }

    #[test]
    fn 段落在输出轨的位置就是时间线位置减这一趟的起点() {
        let mut timeline = one_clip(0, 20, 0, "tone", tb(30, 1));
        timeline.tracks[0]
            .layers
            .push(clip_layer("clip2", 40, 60, "tone", 0));
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            5,
            54,
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 2);
        // 这一趟从第 5 帧起算：
        //   第一段原区间 [0, 20) 与 [5, 55) 的交 = [5, 20) -> 输出偏移 0
        //   第二段 [40, 60) 与 [5, 55) 的交 = [40, 55) -> 输出偏移 (40-5) 帧 = 56000
        assert_eq!(plan.segments[0].output_start_sample, 0);
        assert_eq!(plan.segments[0].output_samples, 24_000);
        assert_eq!(plan.segments[1].output_start_sample, 56_000);
        assert_eq!(plan.segments[1].output_samples, 24_000);
        // 位置必须与"时间线位置"对得上：段在输出轨上的起点帧 = 5 + 偏移/1600。
        for segment in &plan.segments {
            let frame = 5 + segment.output_start_sample / 1600;
            assert_eq!(frame, segment.timeline_start, "输出位置与时间线位置对不上");
        }
    }

    #[test]
    fn 素材帧率与时间线不同时按时间取而不是按帧数取() {
        // 素材 60fps、时间线 30fps。时间线 [0, 29] 共 30 帧 = 1 秒。
        // 按帧数取会得到"30 个素材帧 = 0.5 秒"—— 那就**慢了一半**。
        let timeline = one_clip(0, 30, 0, "fast", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[("fast", "fast.mp4")]),
            &asset_tb("fast", 60, 1),
            0,
            29,
        )
        .unwrap();
        assert_eq!(plan.segments[0].output_samples, 48_000, "一秒就是 48000 个采样点");
        // source_in = 60（素材的第 1 秒）-> 仍是 48000，因为素材自己的时间基是 60fps。
        let timeline = one_clip(0, 30, 60, "fast", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[("fast", "fast.mp4")]),
            &asset_tb("fast", 60, 1),
            0,
            29,
        )
        .unwrap();
        assert_eq!(plan.segments[0].source_start_sample, 48_000);
    }

    #[test]
    fn 音频起点与视频取的素材帧是同一个时间点() {
        // 「同源求值」这句话的可执行判据：把音频起点换算回素材的秒，
        // 与视频 source_frame_at 取到的那一帧的秒，差不超过**一个素材帧**。
        let timeline_tb = tb(30, 1);
        for asset in [tb(30, 1), tb(60, 1), tb(30_000, 1001)] {
            let timeline = one_clip(0, 60, 12, "x", timeline_tb.clone());
            let mut timebases = AssetTimebases::new();
            timebases.insert("x", asset.clone());
            let plan = plan_audio(
                &timeline,
                &sources(&[("x", "x.mp4")]),
                &timebases,
                0,
                59,
            )
            .unwrap();
            let audio_start = plan.segments[0].source_start_sample;
            let layer = &timeline.tracks[0].layers[0];
            let video_frame =
                video_source_frame_at(layer, 0, &timeline_tb, &asset).unwrap();
            let video_sample =
                sample_at_frame(video_frame, &asset, AUDIO_SAMPLE_RATE).unwrap();
            let one_asset_frame = sample_at_frame(1, &asset, AUDIO_SAMPLE_RATE).unwrap();
            assert!(
                (audio_start - video_sample).abs() <= one_asset_frame,
                "素材时间基 {}/{}：音频起于 {audio_start}，视频那一帧在 {video_sample}，差得超过一个素材帧",
                asset.num,
                asset.den
            );
        }
    }

    #[test]
    fn 同一素材的两段按时间线顺序排好且各自起点不同() {
        let mut timeline = one_clip(0, 30, 0, "tone", tb(30, 1));
        timeline.tracks[0]
            .layers
            .push(clip_layer("clip2", 45, 90, "tone", 30));
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            89,
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 2);
        assert_eq!(plan.segments[0].timeline_start, 0);
        assert_eq!(plan.segments[0].source_start_sample, 0);
        assert_eq!(plan.segments[1].timeline_start, 45);
        // 第二段 source_in=30 -> 起于 48000；片段内偏移 45-45=0。
        assert_eq!(plan.segments[1].source_start_sample, 48_000);
        // 空档 [30, 45) 共 15 帧 = 24000 个采样点。
        assert_eq!(plan.gap_samples(), 24_000);
        assert_eq!(plan.distinct_assets(), 1);
        assert!(plan.issues.is_empty());
    }

    #[test]
    fn 音轨缺素材要报出来而不是静音() {
        let timeline = one_clip(0, 30, 0, "ghost", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[]),
            &AssetTimebases::new(),
            0,
            29,
        )
        .unwrap();
        assert!(plan.is_silent());
        assert_eq!(plan.issues.len(), 1);
        assert_eq!(plan.issues[0].code, "audio_source_missing");
    }

    #[test]
    fn 音轨上没有素材的图层要报出来() {
        let mut timeline = one_clip(0, 30, 0, "tone", tb(30, 1));
        timeline.tracks[0].layers[0].source = None;
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            29,
        )
        .unwrap();
        assert_eq!(plan.issues.len(), 1);
        assert_eq!(plan.issues[0].code, "audio_layer_without_source");
    }

    #[test]
    fn 两段重叠是相加而不是判失败() {
        // **这条在 T13 变了。** 从前重叠判失败（"静默取一条会产出一份听着像对的
        // 错产物"）—— 那个理由至今成立，所以这里仍然不许静默丢；
        // 但改成**真的混**，因为音效（SFX）天生就叠在背景音上：
        // 判失败等于"音效这个功能永远做不了"。
        let mut timeline = one_clip(0, 60, 0, "tone", tb(30, 1));
        timeline.tracks.push(TrackV2 {
            id: "a2".to_string(),
            kind: TrackKind::Audio,
            layers: vec![clip_layer("other", 30, 90, "tone", 0)],
            subtitle: None,
            danmaku: None,
            gain: 1.0,
        });
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            89,
        )
        .unwrap();
        // 不再报错：叠加是正常操作。
        assert!(
            plan.issues.is_empty(),
            "重叠不该再判失败：{:?}",
            plan.issues.iter().map(|i| &i.code).collect::<Vec<_>>()
        );
        // 但**要记下来**：加法会削顶，削顶要看得见才知道该不该调增益。
        assert_eq!(plan.overlaps.len(), 1, "重叠要记一笔，实得 {:?}", plan.overlaps);
        assert_eq!(plan.overlaps[0].at, 30);
        // 两段都还在计划里（不许静默丢掉任何一段）。
        assert_eq!(plan.segments.len(), 2);
    }

    #[test]
    fn 不重叠的段不产生叠加记录() {
        // 反向用例：把"记一笔"写成无条件的，会让每份多轨工程都报一堆假重叠。
        let mut timeline = one_clip(0, 30, 0, "tone", tb(30, 1));
        timeline.tracks.push(TrackV2 {
            id: "a2".to_string(),
            kind: TrackKind::Audio,
            layers: vec![clip_layer("other", 30, 60, "tone", 0)],
            subtitle: None,
            danmaku: None,
            gain: 1.0,
        });
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            59,
        )
        .unwrap();
        assert!(plan.overlaps.is_empty(), "首尾相接不是重叠：{:?}", plan.overlaps);
        assert_eq!(plan.segments.len(), 2);
    }

    #[test]
    fn 视频轨不会被当成音频() {
        let mut timeline = one_clip(0, 30, 0, "tone", tb(30, 1));
        timeline.tracks[0].kind = TrackKind::Video;
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            0,
            29,
        )
        .unwrap();
        assert!(plan.is_silent());
    }

    #[test]
    fn 片段与帧区间只在交叠处取() {
        // 片段 [20, 50)，这一趟出 [40, 79] -> 应当只取 [40, 50)。
        let timeline = one_clip(20, 50, 0, "tone", tb(30, 1));
        let plan = plan_audio(
            &timeline,
            &sources(&[("tone", "tone.m4a")]),
            &asset_tb("tone", 30, 1),
            40,
            79,
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].timeline_start, 40);
        assert_eq!(plan.segments[0].timeline_end, 50);
        assert_eq!(plan.segments[0].output_samples, 16_000);
        // 片段内偏移 40-20=20 帧 = 32000。
        assert_eq!(plan.segments[0].source_start_sample, 32_000);
    }

    #[test]
    fn 音轨时长与视频时长是同一个数() {
        // 这一条的实质：`total_samples` 由 from/to 算出，跟有没有音频段无关。
        // 于是"时长与视频一致"是结构保证，不是事后对齐。
        let with = one_clip(0, 90, 0, "tone", tb(30, 1));
        let without = TimelineV2 {
            schema: 3,
            timebase: tb(30, 1),
            markers: Vec::new(),
            tracks: Vec::new(),
        };
        let table = sources(&[("tone", "tone.m4a")]);
        let timebases = asset_tb("tone", 30, 1);
        for (from, to) in [(0, 89), (0, 29), (17, 42), (5, 5)] {
            let a = plan_audio(&with, &table, &timebases, from, to).unwrap();
            let b = plan_audio(&without, &table, &timebases, from, to).unwrap();
            assert_eq!(
                a.total_samples, b.total_samples,
                "[{from}, {to}] 两边的总采样数必须一样（音轨不影响时长）"
            );
            assert_eq!(a.start_sample, b.start_sample);
        }
    }

    #[test]
    fn 取样点号是单调的() {
        // 相邻帧之间的采样点数只可能是 floor/ceil 两个值，且永不为负 ——
        // 这一条挡的是"某个帧率下采样点倒退"那种错。
        let tb2997 = tb(30_000, 1001);
        let mut last = sample_at_frame(0, &tb2997, 48_000).unwrap();
        for frame in 1..1000 {
            let now = sample_at_frame(frame, &tb2997, 48_000).unwrap();
            assert!(now > last, "第 {frame} 帧的采样点没有前进");
            last = now;
        }
    }
}
