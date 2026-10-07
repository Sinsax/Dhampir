// 网页动画的**求值**（第二个宿主用）。
//
// # 这是一份**第二实现**，而且本仓一贯反对第二实现
//
// 本仓的规矩是"同一份逻辑只有一份"（见 crates/dhampir-timeline/src/curve.rs 的模块头：
// 两份会漂，而漂了没有任何东西会红）。这里之所以还敢有第二份，是因为它的**唯一前提**
// 已经就位：`scripts/check-anim-eval.mjs` 拿 core 的逐帧逐通道读数与它比对，
// 容差写在判据里。而两份实现不可能逐比特相同（core 是 f32 算术，JS 这边是 f64），
// 所以容差不是零，而是"量出来的那个数"，见 plan/waapi-stage3-evidence.md。
//
// # 它要干的只有两件事
//
// 1. **缓动**：把 CSS 缓动串算成 [0,1] 上的值 —— 与 crates/dhampir-timeline/src/easing.rs 同一套语义；
// 2. **通道插值**：与 crates/dhampir-timeline/src/curve.rs 的四条规则逐条相同。
//
// 它**不做**：转场权重、素材帧换算、特效参数求值、文字布局。那些要么与画面无关（DOM 侧自带），
// 要么还没到这一步 —— 需要的时候按同一套判据一件一件加。

/** 缓动公式的常数：与 easing.rs 里那份**逐字相同**（下划线 vs 连字符是两条曲线）。 */
const CSS_EASE = [0.25, 0.1, 0.25, 1.0];
const CSS_EASE_IN = [0.42, 0.0, 1.0, 1.0];
const CSS_EASE_OUT = [0.0, 0.0, 0.58, 1.0];
const CSS_EASE_IN_OUT = [0.42, 0.0, 0.58, 1.0];

/** 解析一个缓动串。认不出来**抛错**（调用方决定怎么办），不静默降级。 */
export function parseEasing(text) {
  const trimmed = String(text).trim();
  if (trimmed === '') throw new Error('缓动是空串');
  const lower = trimmed.toLowerCase();
  switch (lower) {
    case 'linear': return { kind: 'linear' };
    // 本仓既有（下划线）：二次曲线
    case 'ease_in': return { kind: 'quadIn' };
    case 'ease_out': return { kind: 'quadOut' };
    case 'ease_in_out': return { kind: 'quadInOut' };
    case 'back_out': return { kind: 'backOut' };
    // CSS 关键字（连字符）：等于一条 cubic-bezier
    case 'ease': return bezier(CSS_EASE);
    case 'ease-in': return bezier(CSS_EASE_IN);
    case 'ease-out': return bezier(CSS_EASE_OUT);
    case 'ease-in-out': return bezier(CSS_EASE_IN_OUT);
    case 'step-start': return { kind: 'steps', count: 1, position: 'jumpStart' };
    case 'step-end': return { kind: 'steps', count: 1, position: 'jumpEnd' };
    default: break;
  }
  const bezierArgs = functionArgs(lower, 'cubic-bezier');
  if (bezierArgs !== null) return bezier(parseBezierArgs(trimmed, bezierArgs));
  const stepsArgs = functionArgs(lower, 'steps');
  if (stepsArgs !== null) return parseStepsArgs(trimmed, stepsArgs);
  if (functionArgs(lower, 'linear') !== null) throw new Error('这一版不支持 linear() 断点表：' + trimmed);
  throw new Error('不认识的缓动：' + trimmed);
}

function bezier(values) {
  return { kind: 'cubicBezier', x1: values[0], y1: values[1], x2: values[2], y2: values[3] };
}

function functionArgs(lower, name) {
  if (!lower.startsWith(name)) return null;
  const rest = lower.slice(name.length).trimStart();
  if (!rest.startsWith('(') || !rest.endsWith(')')) return null;
  return rest.slice(1, -1);
}

function numbersOf(original, args, expected) {
  const parts = args.split(',').map((part) => part.trim());
  if (parts.some((part) => part === '')) throw new Error('参数不合法：' + original);
  if (expected !== null && parts.length !== expected) throw new Error('参数个数不对：' + original);
  const values = parts.map((part) => Number(part));
  if (values.some((value) => !Number.isFinite(value))) throw new Error('参数不是有限数：' + original);
  return values;
}

function parseBezierArgs(original, args) {
  const values = numbersOf(original, args, 4);
  // 规范：两个 x 必须在 [0,1]；y 不限（过冲靠它表达）。
  if (values[0] < 0 || values[0] > 1 || values[2] < 0 || values[2] > 1) throw new Error('x 越界：' + original);
  return values;
}

function parseStepsArgs(original, args) {
  const parts = args.split(',').map((part) => part.trim());
  if (parts.length === 0 || parts.length > 2) throw new Error('参数个数不对：' + original);
  const count = Number(parts[0]);
  if (!Number.isInteger(count) || count < 1) throw new Error('步数必须是正整数：' + original);
  const raw = parts.length === 2 ? parts[1] : 'jump-end';
  const table = {
    'jump-end': 'jumpEnd', end: 'jumpEnd',
    'jump-start': 'jumpStart', start: 'jumpStart',
    'jump-none': 'jumpNone',
    'jump-both': 'jumpBoth',
  };
  const position = table[raw];
  if (position === undefined) throw new Error('跳跃位置不合法：' + original);
  if (position === 'jumpNone' && count < 2) throw new Error('jump-none 至少要两段：' + original);
  return { kind: 'steps', count, position };
}

/** [0,1] 上的映射。输入夹到 [0,1]；**输出不夹**（过冲是回弹的全部意义）。 */
export function easeValue(form, t) {
  const x = Math.min(1, Math.max(0, t));
  switch (form.kind) {
    case 'linear': return x;
    case 'quadIn': return x * x;
    case 'quadOut': return 1 - (1 - x) * (1 - x);
    case 'quadInOut': return x < 0.5 ? 2 * x * x : 1 - 2 * (1 - x) * (1 - x);
    case 'backOut': {
      const s = 1.70158;
      const u = x - 1;
      return u * u * ((s + 1) * u + s) + 1;
    }
    case 'cubicBezier': return cubicBezierAt(x, form.x1, form.y1, form.x2, form.y2);
    case 'steps': return stepsAt(x, form.count, form.position);
    default: throw new Error('未实现的缓动形式：' + form.kind);
  }
}

function cubicBezierAt(t, x1, y1, x2, y2) {
  const cx = 3 * x1;
  const bx = 3 * (x2 - x1) - cx;
  const ax = 1 - cx - bx;
  const cy = 3 * y1;
  const by = 3 * (y2 - y1) - cy;
  const ay = 1 - cy - by;
  const u = solveCurveX(ax, bx, cx, t);
  return ((ay * u + by) * u + cy) * u;
}

const sampleX = (ax, bx, cx, u) => ((ax * u + bx) * u + cx) * u;
const sampleDx = (ax, bx, cx, u) => (3 * ax * u + 2 * bx) * u + cx;

/** 由 x 反解 u。**迭代次数与 easing.rs 写死的一致**（不靠收敛精度决定结果）。 */
function solveCurveX(ax, bx, cx, x) {
  const EPSILON = 1e-6;
  let u = x;
  for (let i = 0; i < 8; i += 1) {
    const error = sampleX(ax, bx, cx, u) - x;
    if (Math.abs(error) < EPSILON) return u;
    const derivative = sampleDx(ax, bx, cx, u);
    if (Math.abs(derivative) < 1e-6) break;
    u -= error / derivative;
  }
  let low = 0;
  let high = 1;
  u = x;
  for (let i = 0; i < 24; i += 1) {
    const value = sampleX(ax, bx, cx, u);
    if (Math.abs(value - x) < EPSILON) return u;
    if (x > value) low = u; else high = u;
    u = (high + low) * 0.5;
  }
  return u;
}

/** `steps()` 的闭式 —— 四个变体的差别只在第几级 + 除以几（与 easing.rs 同形）。 */
export function stepsAt(t, count, position) {
  const n = count;
  const level = Math.floor(t * n);
  let value;
  switch (position) {
    case 'jumpStart': value = (level + 1) / n; break;
    case 'jumpEnd': value = level / n; break;
    case 'jumpNone': value = level / (n - 1); break;
    case 'jumpBoth': value = (level + 1) / (n + 1); break;
    default: throw new Error('未实现的跳跃位置：' + position);
  }
  return Math.min(1, Math.max(0, value));
}

/**
 * 通道求值：与 crates/dhampir-timeline/src/curve.rs 的四条规则**逐条相同**。
 *
 * 1. 先按 frame 排一次（契约不要求有序）；
 * 2. local <= 首键 → 首键的值；
 * 3. local >= 末键 → 末键的值；
 * 4. 否则在相邻对里插值，用的是**后一个键**的缓动；span <= 0 时取 t = 1 防除零。
 */
export function channelFrom(fallback, keyframes, target, localFrame) {
  const keys = (keyframes === undefined ? [] : keyframes)
    .filter((key) => key.target === target)
    .slice()
    .sort((a, b) => a.frame - b.frame);
  if (keys.length === 0) return fallback;
  const first = keys[0];
  if (localFrame <= first.frame) return first.value;
  const last = keys[keys.length - 1];
  if (localFrame >= last.frame) return last.value;
  for (let i = 0; i + 1 < keys.length; i += 1) {
    const a = keys[i];
    const b = keys[i + 1];
    if (localFrame >= a.frame && localFrame <= b.frame) {
      const span = b.frame - a.frame;
      const t = span <= 0 ? 1 : (localFrame - a.frame) / span;
      const eased = easeValue(parseEasing(b.easing === undefined ? 'linear' : b.easing), t);
      return a.value + (b.value - a.value) * eased;
    }
  }
  return fallback;
}

/**
 * 某一帧上要画的层（**从下往上**，与 core 的 Composite 同序）。
 *
 * 只做 core 在 `element_to_draw` 里做的那几件：opacity（含元素自身的关键帧曲线）
 * 与 x/y/scale/rotation 四条通道。**转场权重不在这里** —— 样本工程没有转场，
 * 而判据会**先断言样本没有转场**，免得把"没实现"混进"算错了"。
 */
export function evaluateFrame(timeline, frame) {
  const layers = [];
  for (const track of timeline.tracks === undefined ? [] : timeline.tracks) {
    if (track.kind !== 'video') continue;
    const layer = (track.layers === undefined ? [] : track.layers)
      .find((candidate) => candidate.start <= frame && frame < candidate.end);
    if (layer === undefined || layer.enabled === false) continue;
    const local = frame - layer.start;
    const keys = layer.keyframes;
    const transform = layer.transform === undefined ? {} : layer.transform;
    layers.push({
      id: layer.id,
      opacity: channelFrom(layer.opacity === undefined ? 1 : layer.opacity, keys, 'opacity', local),
      x: channelFrom(transform.x === undefined ? 0 : transform.x, keys, 'x', local),
      y: channelFrom(transform.y === undefined ? 0 : transform.y, keys, 'y', local),
      scale: channelFrom(transform.scale === undefined ? 1 : transform.scale, keys, 'scale', local),
      rotation: channelFrom(transform.rotation === undefined ? 0 : transform.rotation, keys, 'rotation', local),
    });
  }
  return layers;
}
