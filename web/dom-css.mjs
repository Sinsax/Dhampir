// 把工程里的**特效**翻成 DOM 侧的 CSS。
//
// # 为什么要有这一层（而不是在 dom-host 里就地拼串）
//
// 它是一条**口径**，要能被判据钉住；拼串藏在 DOM 操作里就没法在无浏览器的地方验。
// 判据：scripts/check-dom-css.mjs。
//
// # 只搬**能证明等价**的那几种
//
// 口径见 plan/web-animation-criteria.md 的 D5（模糊）与 D6（色彩）：
//
// | 本仓 | CSS | 等价性 | 这里怎么做 |
// |---|---|---|---|
// | `contrast{amount}` | `contrast(k)` | 逐值等价（同一个公式） | 搬 |
// | `gaussian_blur{radius}` | `blur(σ)`，σ = radius/2 | 逐值等价（D5） | 搬 |
// | `saturation{amount}` | `saturate(k)` | **几乎**等价（luma 权重取整不同，D6） | 搬，但记一条**近似** |
// | `brightness_multiply{factor}` | `brightness(k)` | 等价（都是 `c·k`，保黑） | 原样搬 |
// | `brightness{amount}` | — | **不等价**：本仓那条是加、CSS 是乘（D6） | **不搬**（要乘性用上一条） |
// | `hue{degrees}` | `hue-rotate(Ddeg)` | **矩阵不同源**（D6） | **不搬** |
//
// 不搬的那两种**必须被报出来** —— 静默画一个「看起来差不多」的东西，比画不出来更坏：
// 前者会让人以为预览是对的，而后者会让人去查。

/** 精确程度的两档：等价 / 有已知偏差（偏差写在 why 里）。 */
export const EXACT = 'exact';
export const APPROXIMATE = 'approximate';

/** 数字进 CSS 文本：去掉浮点尾巴（0.1+0.2 那种不该出现在样式里）。 */
function format(value) {
  return String(Math.round(Number(value) * 1000) / 1000);
}

/**
 * 一条特效 → 一条 CSS 滤镜函数（或拒绝）。
 *
 * 返回 `{ css, applied, refused }`：applied 里每条带 exactness；refused 里每条带 why。
 * **不做静默降级**：认不出的特效进 refused，不会变成一个「差不多的」滤镜。
 */
export function layerFilters(effects) {
  const applied = [];
  const refused = [];
  for (const effect of effects === undefined ? [] : effects) {
    const kind = String(effect.kind);
    const param = (name, fallback) => {
      const raw = effect.params === undefined ? undefined : effect.params[name];
      return raw === undefined ? fallback : Number(raw);
    };
    if (kind === 'contrast') {
      applied.push({ kind, css: 'contrast(' + format(param('amount', 1)) + ')', exactness: EXACT });
    } else if (kind === 'gaussian_blur') {
      // D5：CSS 的 blur(R) 它就是标准差 σ，而本仓 σ = radius/2 ⇒ CSS 用 radius/2。
      const sigma = param('radius', 0) / 2;
      applied.push({ kind, css: 'blur(' + format(sigma) + 'px)', exactness: EXACT });
    } else if (kind === 'saturation_css') {
      // 第 45 轮新增的那条：灰度权重用 CSS/SVG 规范那组取整值 ⇒ 与 saturate() 等价。
      applied.push({ kind, css: 'saturate(' + format(param('amount', 1)) + ')', exactness: EXACT });
    } else if (kind === 'saturation') {
      applied.push({
        kind,
        css: 'saturate(' + format(param('amount', 1)) + ')',
        exactness: APPROXIMATE,
        why: '本仓 luma 用 Rec.709（0.2126/0.7152/0.0722），CSS 规范那组取整到 0.213/0.715/0.072 —— 实测逐通道差不超过 1 档（分不出来，但不是同一个函数）。要精确一致就用规范那条 saturation_css',
      });
    } else if (kind === 'brightness_multiply') {
      // 第 42 轮新增的那条**乘性**亮度：它就是 CSS `brightness()` 本身（`c·k`，保黑）。
      applied.push({ kind, css: 'brightness(' + format(param('factor', 1)) + ')', exactness: EXACT });
    } else if (kind === 'brightness') {
      refused.push({
        kind,
        why: '这一条是**加性**偏移（抬黑），而 CSS 的 brightness 是**乘性**（保黑）—— 两端画出来不是一回事（D6）。要预览它得用乘性那条（`brightness_multiply`）',
      });
    } else if (kind === 'hue_rotate_css') {
      // 第 44 轮新增的那条：矩阵取自 CSS/SVG 规范本身 ⇒ 与 CSS `hue-rotate()` 等价。
      applied.push({ kind, css: 'hue-rotate(' + format(param('degrees', 0)) + 'deg)', exactness: EXACT });
    } else if (kind === 'hue_rotate_css') {
      // 第 44 轮新增的那条：矩阵取自 CSS/SVG 规范本身 ⇒ 与 CSS hue-rotate() 等价。
      applied.push({ kind, css: 'hue-rotate(' + format(param('degrees', 0)) + 'deg)', exactness: EXACT });
      } else if (kind === 'hue') {
      refused.push({
        kind,
        why: '这一条用的是 YIQ / Rec.601 的旋转矩阵，而 CSS 规范用另一组 —— 饱和色上明显不同（D6）。要预览它得用规范那条（`hue_rotate_css`）',
      });
    } else {
      refused.push({ kind, why: 'DOM 宿主还没实现这条特效' });
    }
  }
  return { css: applied.map((item) => item.css).join(' '), applied, refused };
}

/**
 * **引擎侧实现了几条混合模式** —— 这份清单必须与
 * `crates/dhampir-timeline/src/layer.rs` 的 `BlendMode::is_implemented` **一致**（判据会读源码比对）。
 *
 * 为什么 DOM 侧要知道这件事：CSS 有 8 条以上混合模式，而引擎只实现了 4 条。
 * DOM 宿主画得出来、引擎画不出来 —— 这正是阶段 4「只在 HTML 侧长」的形态。
 * 但**差额必须被报出来**：否则用户会以为「预览能看就等于能出片」。
 */
export const ENGINE_IMPLEMENTED_BLENDS = ['normal', 'add', 'multiply', 'screen', 'darken', 'lighten', 'overlay', 'soft_light', 'difference'];

/** 本仓的混合模式串 → CSS 的 `mix-blend-mode` 取值。`add` 不在里面（CSS 没有它，见下）。 */
const BLEND_CSS = {
  normal: 'normal',
  multiply: 'multiply',
  screen: 'screen',
  darken: 'darken',
  lighten: 'lighten',
  overlay: 'overlay',
  soft_light: 'soft-light',
  difference: 'difference',
};

/**
 * 一条混合模式 → CSS。返回 `{ css, exactness, refused, engine }`：
 *
 * - `css` 为空串表示「不设这个样式」（normal 或拒绝）；
 * - `engine: 'missing'` 表示**引擎还没实现**它 —— 预览画得出来，出片会拒绝那一帧。
 *
 * `add` 是唯一需要近似的：CSS 没有 `add`，最接近的是 `plus-lighter`（预乘空间相加），
 * 而它与本仓的 Add 未必逐值相同（预乘与夹取口径还没对账）—— 所以标**近似**并写清理由。
 */
export function layerBlendMode(blend) {
  const name = blend === undefined ? 'normal' : String(blend);
  if (name === 'normal') return { css: '', exactness: EXACT, engine: 'implemented' };
  if (name === 'add') {
    return {
      css: 'plus-lighter',
      exactness: APPROXIMATE,
      engine: 'implemented',
      why: 'CSS 没有 add：这里用 plus-lighter（预乘空间相加），与本仓的 Add **未必逐值相同**（预乘与夹取口径还没对账）',
    };
  }
  const css = BLEND_CSS[name];
  if (css === undefined) return { css: '', exactness: EXACT, refused: { kind: name, why: '不认识这个混合模式' } };
  const implemented = ENGINE_IMPLEMENTED_BLENDS.includes(name);
  return {
    css,
    exactness: EXACT,
    engine: implemented ? 'implemented' : 'missing',
    why: implemented ? undefined : '引擎还没实现这个模式（BlendMode::is_implemented 目前只有 4 条）—— 预览画得出来，**出片会拒绝这一帧**',
  };
}

/**
 * **引擎侧能不能画圆角** —— 判据读 `crates/dhampir-timeline/src/layer.rs` 比对：
 * 那里若存在 `unimplemented_corner_radius`（= 引擎还画不了），这个常量必须是 `false`。
 * 第 13 轮引擎把它画出来了（合成着色器里的无分支 SDF），于是两边一起翻成 true。
 *
 * 契约字段已经立了（`Layer.corner_radius`，缺省 0 且为 0 时不写进文件），
 * 所以 DOM 侧现在就可以先画出来（阶段 4：预览先看见）；但**引擎那边画不出来**，
 * 出片会拒绝这一帧 —— 这件事必须报出来，否则用户会以为「预览能看就能出片」。
 */
export const ENGINE_SUPPORTS_CORNER_RADIUS = true;

/**
 * 一条圆角半径 → CSS。返回 `{ css, engine, why }`。
 *
 * `css` 为空串表示「不设」（半径 <= 0）；`engine: 'missing'` 表示引擎还画不出来。
 */
export function layerCornerRadius(radius) {
  const value = radius === undefined ? 0 : Number(radius);
  if (!(value > 0)) return { css: '', engine: ENGINE_SUPPORTS_CORNER_RADIUS ? 'implemented' : 'missing' };
  return {
    css: format(value) + 'px',
    engine: ENGINE_SUPPORTS_CORNER_RADIUS ? 'implemented' : 'missing',
    why: ENGINE_SUPPORTS_CORNER_RADIUS ? undefined : '引擎还没实现圆角（契约字段已立、SDF 是下一步，见 criteria 的 D10）—— 预览画得出来，**出片会拒绝这一帧**',
  };
}

/**
 * 一条裁剪形状 → CSS `clip-path`。返回 `{ css, refused }`（`css` 为空串表示不裁）。
 *
 * **搬五种形状**：圆 / 椭圆 / 内缩矩形（引擎那边共用一套无分支 SDF）+
 * 多边形与路径（引擎把它们**栅格化成掩码纹理**）。路径两边都是**文档像素**，所以原样透传。
 *
 * `center` 是**归一化比例**，缺省时不写 `at` —— CSS 的默认位置就是元素中心，
 * 与引擎的 `None` = 图层中心一致；写了就转成百分比（`at 25% 50%`），两边同一口径。
 */
/**
 * 一层上的**投影** → CSS `filter: drop-shadow(…)`。
 *
 * 两处口径：
 *
 * 1. CSS 的 `drop-shadow(dx dy b)` 里 `b` 是**模糊半径**，换算成 σ 是 `b / 2`；
 *    而我们的 `blur_sigma` **就是 σ** ⇒ `b = 2σ`（与 `blur()` 那条换算同一个方向）。
 * 2. CSS 的 `drop-shadow` 没有单独的浓淡参数 ⇒ 用颜色的 alpha 表达。
 */
export function layerShadowFilter(shadow) {
  if (shadow === undefined || shadow === null) return '';
  const px = (value) => format(Number(value)) + 'px';
  const blur = px(Number(shadow.blur_sigma === undefined ? 0 : shadow.blur_sigma) * 2);
  const opacity = Number(shadow.opacity === undefined ? 1 : shadow.opacity);
  const color = 'rgb(0 0 0 / ' + format(opacity * 100) + '%)';
  return (
    'drop-shadow(' + px(shadow.offset_x === undefined ? 0 : shadow.offset_x) + ' ' +
    px(shadow.offset_y === undefined ? 0 : shadow.offset_y) + ' ' + blur + ' ' + color + ')'
  );
}

export function layerClip(clip) {
  if (clip === undefined || clip === null) return { css: '' };
  const kind = String(clip.kind);
  const px = (value) => format(Number(value)) + 'px';
  // 中心是**归一化比例** → CSS 的百分比（同一口径：`at 50% 50%` 就是正中）。
  const percent = (value) => format(Number(value) * 100) + '%';
  const at = (center) =>
    center === undefined || center === null ? '' : ' at ' + percent(center[0]) + ' ' + percent(center[1]);
  if (kind === 'circle') return { css: 'circle(' + px(clip.radius) + at(clip.center) + ')' };
  if (kind === 'ellipse') {
    return { css: 'ellipse(' + px(clip.radius_x) + ' ' + px(clip.radius_y) + at(clip.center) + ')' };
  }
  if (kind === 'inset') {
    const radius = Number(clip.radius === undefined ? 0 : clip.radius);
    const round = radius > 0 ? ' round ' + px(radius) : '';
    return {
      css: 'inset(' + px(clip.top) + ' ' + px(clip.right) + ' ' + px(clip.bottom) + ' ' + px(clip.left) + round + ')',
    };
  }
  if (kind === 'polygon') {
    const points = clip.points;
    if (!Array.isArray(points) || points.length < 3) {
      return {
        css: '',
        refused: { kind, why: '多边形至少要有 3 个顶点才画得出来（引擎那边同样会拒绝）' },
      };
    }
    // 引擎的顶点是**图层框内的归一化坐标**，CSS 用百分比 —— 同一个口径，乘 100 即可。
    const list = points.map((point) => percent(point[0]) + ' ' + percent(point[1])).join(', ');
    return { css: 'polygon(' + list + ')' };
  }
  if (kind === 'path') {
    const data = String(clip.data === undefined ? '' : clip.data).trim();
    if (data === '' || !(data.startsWith('M') || data.startsWith('m'))) {
      return { css: '', refused: { kind, why: '路径数据要不是空的、并且以 M 开头（引擎那边解析不了也会拒绝）' } };
    }
    // 两边都是**文档像素**：原样透传，不换算。
    return { css: 'path("' + data + '")' };
  }
  return {
    css: '',
    refused: { kind, why: 'DOM 侧只支持 circle / ellipse / inset / polygon / path' },
  };
}

/**
 * 一条掩码 → CSS `mask-image`。返回 `{ css, exactness, why, refused }`。
 *
 * # 一条必须先说清的安全规矩
 *
 * **掩码图取不到时，浏览器会把整个元素遮没**（CSS 的规矩：拿不到的遮罩 = 全黑 = alpha 0）。
 * 而本仓的工程里 `asset.uri` 常常是个**相对文件名**（`m.png`），DOM 宿主没有它的基准路径 ——
 * 那种情况下如果照写 `url(m.png)`，预览里那一层会**整个消失**，比"画不出掩码"坏得多。
 *
 * 所以这里分两种：
 *
 *   · `uri` 是**能直接取的**（`http:` / `https:` / `blob:` / `data:`）→ 写上 `mask-image`，
 *     并把它标成 **未实测**（DOM 这一版不做素材解码，这条没在浏览器里比过）；
 *   · 否则 → **不设**，并**报出来**（"不设"是刻意的，不是漏了）。
 */
export function layerMask(mask, assets) {
  if (mask === undefined || mask === null) return { css: '' };
  // **程序化渐变**（第 47 轮）：不需要素材表，直接写成 CSS 的 `linear-gradient`。
  // 角度约定两端**正好一致**（0° 朝上、90° 朝右）—— 引擎侧就是这么定的（照 CSS）。
  // 覆盖度写成 **alpha**（引擎默认读掩码的 alpha 通道）。
  if (mask.gradient !== undefined && mask.gradient !== null) {
    const stops = (mask.gradient.stops === undefined ? [] : mask.gradient.stops)
      .map((stop) => 'rgba(0, 0, 0, ' + format(stop.coverage) + ') ' + format(stop.at * 100) + '%')
      .join(', ');
    return {
      css: 'linear-gradient(' + format(mask.gradient.angle_deg) + 'deg, ' + stops + ')',
      exactness: EXACT,
      notes: mask.invert === true
        ? [{ kind: 'mask', why: '反相在 CSS 的 mask-image 里表达不了（要靠 mask-composite 换图）—— 这一版**没有把反相画上去**（引擎侧画了）' }]
        : [],
    };
  }
  const table = assets === undefined ? {} : assets;
  const asset = table[String(mask.asset_id)];
  if (asset === undefined) {
    return {
      css: '',
      refused: { kind: 'mask', why: '掩码引用的素材 ' + String(mask.asset_id) + ' 不在登记的素材表里 —— 不设 mask-image（取不到会把整个元素遮没）', },
    };
  }
  const uri = String(asset.uri === undefined ? '' : asset.uri);
  const fetchable = /^(https?|blob|data):/i.test(uri);
  if (!fetchable) {
    return {
      css: '',
      refused: {
        kind: 'mask',
        why: '掩码素材的 uri 是 ' + JSON.stringify(uri) + '（不是能直接取的形式）—— DOM 宿主没有它的基准路径，**刻意不设** mask-image：取不到时浏览器会把整个元素遮没，那比画不出掩码更坏',
      },
    };
  }
  // 反相：CSS 里用 mask-composite 那一套表达不了，得换一张图 —— v1 明说。
  const notes = [];
  if (mask.invert === true) notes.push({ kind: 'mask', why: '反相在 CSS 的 mask-image 里表达不了（要靠 mask-composite 换图）—— 这一版**没有把反相画上去**' });
  return {
    css: 'url("' + uri + '")',
    exactness: 'unmeasured',
    why: 'DOM 这一版不做素材解码：这条只把 CSS 写上，**未实测**',
    notes,
  };
}
