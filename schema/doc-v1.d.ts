// 本文件由 scripts/timeline-contract.mjs 生成——**不要手改**。
// 上游是 Rust 类型：crates/dhampir-timeline/src/schema.rs。
//
// 契约冻结在 schema v1；破坏性改动一律 +1（见 plan/video-editor-plan.md 的 M4 退出标准）。

export interface Asset {
  /** 内容指纹。**只记录，不校验** —— 怎么算是宿主的事。 */
  content_hash?: string | null;
  /** 素材总帧数。**有它才谈得上校验素材内越界。** */
  frame_count?: number | null;
  height?: number | null;
  id: string;
  kind: AssetKind;
  /** 显示名。UI 用它，不再拿 URI 当标题。 */
  name?: string;
  note?: string;
  tags?: Record<string, string>;
  /** 素材自身帧率。与工程时基不同就要换算。 */
  timebase?: TimebaseDto | null;
  /** 位置。**语义由宿主解释**（浏览器是 URL，服务端是路径或对象存储 key）。 */
  uri?: string;
  width?: number | null;
}

export type AssetKind = "video" | "audio" | "image" | "image_sequence" | "subtitle";

export type BlendMode = "normal" | "add" | "multiply" | "screen" | "darken" | "lighten" | "overlay" | "soft_light" | "difference";

export interface DanmakuSpec {
  /** 指向弹幕素材（AssetKind::Subtitle，内容是 ASS）。 */
  asset_id: string;
  /** 一条弹幕从右滚到左要多久（毫秒）。 */
  duration_ms?: number;
  /** 字号 = 目标高度 * 这个比例。 */
  font_ratio?: number;
  /** 泳道数。排不下就**丢该条并计数**，不叠在一起。 */
  lanes?: number;
}

export type Easing = "linear" | "ease_in" | "ease_out" | "ease_in_out";

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

export interface Generator {
  app?: string;
  version?: string;
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
  /** 全局唯一。v1 里只保证轨内唯一，v2 提到全局 —— 否则跨轨引用无从谈起。 */
  id: string;
  keyframes?: Keyframe[];
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

export interface Meta {
  /** ISO-8601 字符串。**时间不进渲染，就不该用强类型把宿主绑死。** */
  created_at?: string | null;
  modified_at?: string | null;
  title?: string;
}

export interface RenderHints {
  format?: string;
  height?: number;
  width?: number;
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
  /** 字号 = 目标高度 * 这个比例。 */
  font_ratio?: number;
  /** 最多几行（超出的行丢掉 —— 字幕不该盖住半屏）。 */
  max_lines?: number;
  /** 是否加描边（压住亮背景）。 */
  outline?: boolean;
}

export interface TimebaseDto {
  den: number;
  num: number;
}

export interface TimelineV2 {
  markers?: Marker[];
  schema: number;
  timebase: TimebaseDto;
  tracks?: TrackV2[];
}

export type TrackKind = "video" | "audio" | "subtitle" | "danmaku";

export interface TrackV2 {
  /** 弹幕轨的参数。非弹幕轨忽略它。 */
  danmaku?: DanmakuSpec | null;
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

export interface View {
  playhead?: number;
  selection?: string | null;
  zoom?: number;
}

export type Window = Record<string, never>;
