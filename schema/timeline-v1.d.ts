// 本文件由 scripts/timeline-contract.mjs 生成——**不要手改**。
// 上游是 Rust 类型：crates/dhampir-timeline/src/schema.rs。
//
// 契约冻结在 schema v1；破坏性改动一律 +1（见 plan/video-editor-plan.md 的 M4 退出标准）。

export interface Clip {
  /** 占多少帧。必须为正。 */
  duration: number;
  effects?: Effect[];
  id: string;
  keyframes?: Keyframe[];
  opacity?: number;
  /** 素材标识。**不含路径语义**——由宿主解释（浏览器是 URL，服务端是本地文件）。 */
  source: string;
  /** 在素材里的起始帧号。 */
  source_in: number;
  /** 在时间线上的起始帧号。 */
  track_at: number;
  transform?: Transform;
  /** 入场转场。挂在**后一个**片段上，占它开头的若干帧。 */
  transition_in?: TransitionSpec | null;
}

export type Easing = "linear" | "ease_in" | "ease_out" | "ease_in_out";

export interface Effect {
  /** 类型串，对应 core 的特效注册表。 */
  kind: string;
  /** 参数用 BTreeMap：同一份工程序列化出来必须**逐字节相同**，HashMap 做不到这点。 */
  params?: Record<string, number>;
}

export interface Keyframe {
  easing?: Easing;
  frame: number;
  target?: string;
  value: number;
}

export interface TimebaseDto {
  den: number;
  num: number;
}

export interface Track {
  clips?: Clip[];
  id: string;
  kind: TrackKind;
}

export type TrackKind = "video" | "audio" | "subtitle" | "danmaku";

export interface Transform {
  /** 角度制。整数帧号之外的东西可以是浮点——**只有时间必须是整数**。 */
  rotation_deg: number;
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
