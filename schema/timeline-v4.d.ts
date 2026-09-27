// 本文件由 scripts/timeline-contract.mjs 生成——**不要手改**。
// 上游是 Rust 类型：crates/dhampir-timeline/src/schema.rs。
//
// 契约冻结在 schema v1；破坏性改动一律 +1（见 plan/video-editor-plan.md 的 M4 退出标准）。

export type BlendMode = "normal" | "add" | "multiply" | "screen" | "darken" | "lighten" | "overlay" | "soft_light" | "difference";

export interface DanmakuSpec {
  /** 指向弹幕素材（AssetKind::Subtitle，内容是 ASS）。 */
  asset_id: string;
  /** **文字颜色，RGBA。** */
  color?: number[];
  /** 一条弹幕从右滚到左要多久（毫秒）。 */
  duration_ms?: number;
  /** 淡入时长（毫秒）。V-Trim 的 `getActiveDms()` 是 `0.3`。 */
  fade_in_ms?: number;
  /** 淡出时长（毫秒）。V-Trim 是 `0.2`。 */
  fade_out_ms?: number;
  /** 字体族名（语义同 [`SubtitleStyle::font_family`]，弹幕也归这条）。 */
  font_family?: string | null;
  /** 字号 = 目标高度 * 这个比例。 */
  font_ratio?: number;
  /** 字重。V-Trim 的弹幕是 `600`；默认 400。 */
  font_weight?: number;
  /** **相邻泳道的间距**（归一化）。默认 0 = 取行盒高（`font_ratio * LINE_HEIGHT_EM`）。 */
  lane_spacing_ratio?: number;
  /** **0 号泳道的顶边**（归一化，相对目标高）。默认 0 = 贴着画面最上面。 */
  lane_top_ratio?: number;
  /** 泳道数。排不下就**丢该条并计数**，不叠在一起。 */
  lanes?: number;
  /** 行高 / 字号的比例。默认 0 = 用 [`crate::text_layout::LINE_HEIGHT_EM`]。 */
  line_height?: number;
  /** **基础不透明度。** V-Trim 用 `0.9`（不是 1.0）。 */
  opacity?: number;
  /** 是否加描边。V-Trim 弹幕是 `2px #000`。 */
  outline?: boolean;
  /** 描边颜色。 */
  stroke_color?: number[];
  /** 描边宽度 = 目标高度 * 这个比例（V-Trim 是 `2/1080`）。 */
  stroke_ratio?: number;
}

export type Easing = "linear" | "ease_in" | "ease_out" | "ease_in_out" | "back_out";

export interface Effect {
  /** 类型串，对应 core 的特效注册表。 */
  kind: string;
  /** 整体混合强度，与 `window` 的包络**相乘**。缺省 1.0。 */
  opacity?: number;
  /** 参数用 BTreeMap：同一份工程序列化出来必须**逐字节相同**，HashMap 做不到这点。 */
  params?: Record<string, number>;
  /** 时间窗。缺省 `Always` = 老工程的行为（整个图层生命周期都生效）。 */
  window?: Window;
}

export interface Keyframe {
  easing?: Easing;
  frame: number;
  target?: string;
  value: number;
}

export interface Layer {
  blend?: BlendMode;
  effects?: Effect[];
  enabled?: boolean;
  end: number;
  /** **音频增益**（线性倍数，1.0 = 原样）。 */
  gain?: number;
  /** 全局唯一。v1 里只保证轨内唯一，v2 提到全局 —— 否则跨轨引用无从谈起。 */
  id: string;
  keyframes?: Keyframe[];
  /** **素材放完了要不要从头再来。** */
  loop_source?: boolean;
  markers?: Marker[];
  note?: string;
  opacity?: number;
  /** 没有它就不是实拍片段 —— 这正是调整图层能存在的原因。 */
  source?: SourceRef | null;
  /** 帧区间**左闭右开**：`[start, end)`。 */
  start: number;
  /** 键值分类维度。用 BTreeMap 保证序列化逐字节稳定。 */
  tags?: Record<string, string>;
  transform?: TransformV2;
  transition_in?: TransitionSpec | null;
}

export interface Marker {
  color?: string | null;
  /** **相对该元素 start 的偏移**。用绝对帧号的话，元素一挪标记就错位了。 */
  frame: number;
  id: string;
  name?: string;
}

export interface SourceRef {
  /** 指向工程文件里资产登记表的 id。**契约里只有它，没有位置信息** —— */
  asset_id: string;
  /** 素材内起点（帧）。 */
  source_in: number;
}

export interface SubtitleStyle {
  /** 底边距 = 目标高度 * 这个比例。 */
  bottom_margin?: number;
  /** 文字颜色，RGBA。 */
  color?: number[];
  /** **淡入时长（毫秒）。** 0 = 硬出现。 */
  fade_in_ms?: number;
  /** **淡出时长（毫秒）。** 0 = 硬消失。 */
  fade_out_ms?: number;
  /** **字体族名**（`"LXGW WenKai"` 这种）。`None` = 用宿主的默认字体。 */
  font_family?: string | null;
  /** 字号 = 目标高度 * 这个比例。 */
  font_ratio?: number;
  /** **字重**（100..=900，CSS 的同一套刻度）。默认 400。 */
  font_weight?: number;
  /** **行高 / 字号**的比例。默认 0 = 用 [`crate::text_layout::LINE_HEIGHT_EM`]（1.2）。 */
  line_height?: number;
  /** 最多几行（超出的行丢掉 —— 字幕不该盖住半屏）。 */
  max_lines?: number;
  /** 是否加描边（压住亮背景）。 */
  outline?: boolean;
  /** 入场时从下方浮上来的距离（**文档像素**）。V-Trim 是 `20`。 */
  rise_in_px?: number;
  /** 退场时向上浮的距离（文档像素）。V-Trim 是 `8`。 */
  rise_out_px?: number;
  /** **换行安全宽**（占画布宽的比例）。默认 0 = 沿用老行为。 */
  safe_width_ratio?: number;
  /** 描边颜色。`outline` 为假时忽略。 */
  stroke_color?: number[];
  /** **描边宽度** = 目标高度 * 这个比例，单位是**外侧宽度**（见下）。 */
  stroke_ratio?: number;
}

export interface TimebaseDto {
  den: number;
  num: number;
}

export type TrackKind = "video" | "audio" | "subtitle" | "danmaku";

export interface TrackV2 {
  /** 弹幕轨的参数。非弹幕轨忽略它。 */
  danmaku?: DanmakuSpec | null;
  /** **整条轨的音频增益**（线性倍数，1.0 = 原样）。 */
  gain?: number;
  id: string;
  kind: TrackKind;
  layers?: Layer[];
  /** 字幕轨的样式。非字幕轨忽略它。 */
  subtitle?: SubtitleStyle | null;
}

export interface TransformV2 {
  /** **角度制**（契约规定，不由字段名暗示）。 */
  rotation: number;
  scale: number;
  x: number;
  y: number;
}

export interface TransitionSpec {
  /** 占多少帧。必须为正，且不超过本片段的时长。 */
  duration: number;
  /** 类型串，对应 core 的转场注册表。 */
  kind: string;
}

export type Window = Record<string, never>;
