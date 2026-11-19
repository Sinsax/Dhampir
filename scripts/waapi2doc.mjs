#!/usr/bin/env node
// WAAPI 动画快照 → dhampir 工程文件。
//
//   node scripts/waapi2doc.mjs <快照.json> [输出.doc.json] [报告.json]
//   node scripts/waapi2doc.mjs --strict ...     # 有跳过项就退 2
//
// # 这是什么、不是什么
//
// * 它是**研发期的转译器**：把网页里写的动画变成底座能吃的数据，用来验证阶段 2 的链路；
// * 它**不是底座的一部分**：本仓不存放下游方言（README）—— 成熟后整份搬去下游仓；
// * 它**不做静默近似**：表达不了的一律**明说 + 跳过该条**，并逐条写进报告。
//
// # 口径（plan/web-animation-criteria.md）
//
// * **D1 秒 → 整数帧**：只用**一次量化**（offset 先落到层区间再量化），
//   每个被量化的点连同误差都进报告；用 BigInt 做有理数取整，不靠浮点攒误差。
// * **D2 缓动**：字符串**原样透传**。注意 off-by-one —— WAAPI 的 easing 挂在**起点**键
//   （作用于该键到下一键之间），底座挂在**终点**键（用后一个键的 easing），
//   所以底座的第 i 个键取源的第 i-1 个 easing。第 0 个键的 easing 在底座里用不到。
// * **D3 timing**：delay / endDelay / fill 用**层区间**吸收；iterations / direction
//   这一版只支持"整数次 + direction=normal"，其余明说不支持（不近似、不展开镜像）。

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';

const EXIT_OK = 0;
const EXIT_NOTHING = 2;

/** 有理数取整：round-half-away-from-zero。**不经过浮点**，免得误差攒起来。 */
function roundRationalHalfAway(numerator, denominator) {
  const n = BigInt(numerator);
  const d = BigInt(denominator);
  if (d === 0n) throw new Error('分母为零');
  const negative = n < 0n !== d < 0n;
  const absN = n < 0n ? -n : n;
  const absD = d < 0n ? -d : d;
  const quotient = absN / absD;
  const remainder = absN % absD;
  const rounded = remainder * 2n >= absD ? quotient + 1n : quotient;
  return Number(negative ? -rounded : rounded);
}

/**
 * 毫秒 → 帧。先把毫秒放大成微秒（支持三位小数），再按有理数换算 —— 全程整数。
 * 误差（帧）也一起回报，报告里要能看见"这一格被量化了"。
 */
function msToFrame(ms, timebase) {
  const micros = Math.round(Number(ms) * 1000);
  const exact = (micros * timebase.num) / (1_000_000 * timebase.den);
  const frames = roundRationalHalfAway(BigInt(micros) * BigInt(timebase.num), 1_000_000n * BigInt(timebase.den));
  return { frames, exact, error: frames - exact };
}

/** offset（0..1）→ 层内帧。**只量化一次**：直接把 offset 落到这个跨度上。 */
function offsetToFrame(offset, spanFrames) {
  const scaled = Number(offset) * spanFrames;
  const frames = roundRationalHalfAway(Math.round(scaled * 1e9), 1_000_000_000);
  return { frames, exact: scaled, error: frames - scaled };
}

/** 只认这五个通道 —— 与契约的 TRANSFORM_TARGETS 一致，多一个都不猜。 */
const TRANSFORM_PATTERN = /(translate|translateX|translateY|scale|scaleX|scaleY|rotate)\(([^)]*)\)/g;

/** 解析一个 transform 串。返回写进工程的通道值 + **认不出来的函数名**（要报出去）。 */
function parseTransform(text, report) {
  const channels = {};
  const unknown = [];
  let match;
  TRANSFORM_PATTERN.lastIndex = 0;
  while ((match = TRANSFORM_PATTERN.exec(text)) !== null) {
    const name = match[1];
    const args = match[2]
      .split(',')
      .map((part) => Number(part.trim().replace(/px$|deg$/, '')))
      .filter((value) => Number.isFinite(value));
    if (args.length === 0) {
      unknown.push(name + '(' + match[2] + ')');
      continue;
    }
    if (name === 'translate') {
      if (args[0] !== undefined) channels.x = args[0];
      if (args[1] !== undefined) channels.y = args[1];
    } else if (name === 'translateX') {
      channels.x = args[0];
    } else if (name === 'translateY') {
      channels.y = args[0];
    } else if (name === 'scale') {
      channels.scale = args[0];
    } else if (name === 'rotate') {
      channels.rotation = args[0];
    } else {
      // scaleX / scaleY 在本仓的五个通道里**表达不了**（只有等比 scale）—— 明说，不装作没看见。
      unknown.push(name);
    }
  }
  if (unknown.length > 0) report.warnings.push({ what: 'transform', text, unknown });
  return channels;
}

/** 把一条动画折成一个图层。返回 null 表示"明说不支持，跳过"。 */
/**
 * 缓动的**镜像**：`E'(u) = 1 − E(1−u)`。
 *
 * 为什么需要它：`direction: reverse / alternate` 的那一遍是**把时间倒过来走**，
 * 于是每一段的缓动也要跟着镜像 —— 不镜像就是「倒着放但缓动还正着」，
 * 而那种错**不会报错**，只是观感不对。
 *
 * 关键事实：镜像后的缓动**不一定还在本仓词汇表里**。
 *
 * | 原 | 镜像 | 能表达吗 |
 * |---|---|---|
 * | `linear` | `linear` | ✅ |
 * | `ease_in` ⇄ `ease_out`、`ease_in_out` | 同类（二次曲线一族自洽） | ✅ |
 * | `ease` / `ease-in` / … / `cubic-bezier(x1,y1,x2,y2)` | `cubic-bezier(1−x2, 1−y2, 1−x1, 1−y1)` | ✅ |
 * | `steps(n, jump-end)` ⇄ `steps(n, jump-start)`、`step-start` ⇄ `step-end` | 同类 | ✅ |
 * | `back_out` | 需要 `back_in` —— **本仓没有** | ❌ 明说不支持 |
 * | `steps(n, jump-none)` / `jump-both` | 镜像后不是那四个跳跃位置里的任何一个 | ❌ 明说不支持 |
 *
 * 所以它返回 `{ok: true, text}` 或 `{ok: false, why}`：**能镜像就镜像，不能就明说**。
 */
function mirrorEasing(text) {
  const raw = String(text).trim();
  const lower = raw.toLowerCase();
  if (lower === 'linear') return { ok: true, text: 'linear' };
  if (lower === 'ease_in') return { ok: true, text: 'ease_out' };
  if (lower === 'ease_out') return { ok: true, text: 'ease_in' };
  if (lower === 'ease_in_out') return { ok: true, text: 'ease_in_out' };
  if (lower === 'back_out') return { ok: false, why: 'back_out 的镜像是 back_in，而本仓没有这条' };
  if (lower === 'step-start') return { ok: true, text: 'step-end' };
  if (lower === 'step-end') return { ok: true, text: 'step-start' };
  const keywords = {
    ease: [0.25, 0.1, 0.25, 1],
    'ease-in': [0.42, 0, 1, 1],
    'ease-out': [0, 0, 0.58, 1],
    'ease-in-out': [0.42, 0, 0.58, 1],
  };
  if (keywords[lower] !== undefined) return { ok: true, text: formatBezier(mirrorPoints(keywords[lower])) };
  const bezier = lower.match(/^cubic-bezier\(([^)]*)\)$/);
  if (bezier !== null) {
    const parts = bezier[1].split(',').map((part) => Number(part.trim()));
    if (parts.length !== 4 || parts.some((value) => !Number.isFinite(value))) {
      return { ok: false, why: '读不懂这个 cubic-bezier 的参数' };
    }
    return { ok: true, text: formatBezier(mirrorPoints(parts)) };
  }
  const steps = lower.match(/^steps\(([^)]*)\)$/);
  if (steps !== null) {
    const parts = steps[1].split(',').map((part) => part.trim());
    const count = Number(parts[0]);
    const position = parts.length > 1 ? parts[1] : 'jump-end';
    if (position === 'jump-end' || position === 'end') return { ok: true, text: 'steps(' + count + ', jump-start)' };
    if (position === 'jump-start' || position === 'start') return { ok: true, text: 'steps(' + count + ', jump-end)' };
    return { ok: false, why: 'steps(' + count + ', ' + position + ') 的镜像不是那四个跳跃位置里的任何一个' };
  }
  // `linear()` 断点表的镜像（第 38 轮补上；此前是"明说不支持"）。
  //
  // 规则和别的缓动一样：`E′(u) = 1 − E(1−u)`。对折线来说就是**值取反、位置取反**，
  // 再按位置升序排（= 原序反转）—— 结果仍是一条折线，所以这里是**精确改写**，不是近似。
  //
  // 位置缺省的那些按规范补全（首 0%、末 100%、中间在相邻写明位置之间均匀分布），
  // 然后**一律写成显式百分比**：这样下游不必再推一遍分布规则 ——
  // "同一条折线"在两边就有了同一个来源。
  const linearFn = lower.match(/^linear\((.*)\)$/);
  if (linearFn !== null) {
    const round6 = (value) => String(Math.round(value * 1e6) / 1e6);
    const raw = [];
    for (const piece of linearFn[1].split(',')) {
      const bits = piece.trim().split(/\s+/).filter((bit) => bit !== '');
      if (bits.length === 0 || bits.length > 2) {
        return { ok: false, why: '读不出 linear() 的这一段：' + piece };
      }
      const value = Number(bits[0]);
      if (!Number.isFinite(value)) {
        return { ok: false, why: 'linear() 的值读不出来：' + bits[0] };
      }
      let position = null;
      if (bits.length === 2) {
        const text = String(bits[1]);
        if (!text.endsWith('%')) {
          return { ok: false, why: 'linear() 的位置要写百分比：' + text };
        }
        position = Number(text.slice(0, -1)) / 100;
        if (!Number.isFinite(position) || position < 0 || position > 1) {
          return { ok: false, why: 'linear() 的位置要在 0..100% 之间：' + text };
        }
      }
      raw.push({ value, position });
    }
    if (raw.length < 2) {
      return { ok: false, why: 'linear() 至少要有两个断点' };
    }
    if (raw[0].position === null) raw[0].position = 0;
    if (raw[raw.length - 1].position === null) raw[raw.length - 1].position = 1;
    let at = 0;
    while (at < raw.length) {
      if (raw[at].position !== null) {
        at += 1;
        continue;
      }
      let next = at;
      while (raw[next].position === null) next += 1;
      const start = raw[at - 1].position;
      const end = raw[next].position;
      const slots = next - at + 1;
      for (let slot = at; slot < next; slot += 1) {
        raw[slot].position = start + (end - start) * ((slot - at + 1) / slots);
      }
      at = next + 1;
    }
    const mirrored = raw
      .map((stop) => ({ value: 1 - stop.value, position: 1 - stop.position }))
      .reverse();
    const text = mirrored
      .map((stop) => round6(stop.value) + ' ' + round6(stop.position * 100) + '%')
      .join(', ');
    return { ok: true, text: 'linear(' + text + ')' };
  }
  return { ok: false, why: '认不出这个缓动' };
}

function mirrorPoints(points) {
  return [1 - points[2], 1 - points[3], 1 - points[0], 1 - points[1]];
}

function formatBezier(points) {
  const text = points.map((value) => String(Math.round(value * 1e6) / 1e6)).join(', ');
  return 'cubic-bezier(' + text + ')';
}
function animationToLayer(animation, index, timebase, report) {
  const target = animation.target === undefined ? 'anim' + index : String(animation.target);
  const timing = animation.timing === undefined ? {} : animation.timing;
  const iterations = timing.iterations === undefined ? 1 : Number(timing.iterations);
  const direction = timing.direction === undefined ? 'normal' : String(timing.direction);
  const iterationStart = timing.iterationStart === undefined ? 0 : Number(timing.iterationStart);

  if (!Number.isInteger(iterations) || iterations > 64) {
    report.skipped.push({
      target,
      reason: 'iterations=' + String(timing.iterations) + '（这一版只支持 1..64 的整数次；不展开、不近似）',
    });
    return null;
  }
  if (!Number.isInteger(iterationStart)) {
    report.skipped.push({ target, reason: 'iterationStart=' + String(iterationStart) + ' 不是整数（非整数相位要额外一次量化）' });
    return null;
  }
  // 方向不再一律跳过：反向的那一遍用**镜像展开**表达（见 mirrorEasing）。
  // 认不出的方向明说；能镜像的照做，不能镜像的那一段在展开时明说。
  const DIRECTIONS = ['normal', 'reverse', 'alternate', 'alternate-reverse'];
  if (!DIRECTIONS.includes(direction)) {
    report.skipped.push({ target, reason: '不认识的 direction=' + direction });
    return null;
  }
  if (timing.fill === 'none') {
    report.warnings.push({ target, reason: 'fill=none 表达不了"不进画面"，按 fill=both 处理（层区间内恒为关键帧值）' });
  }

  // 静态样式 → 本仓的声明式数据（滤镜 / 混合模式 / 圆角）。口径见 D5 / D6 / D9 / D10。
  // 认不出或两端不等价的一律**明说并跳过这条动画** —— 不近似。
  const bits = styleToLayerBits(animation.style, report, target);
  if (bits === null) return null;

  const duration = msToFrame(timing.duration === undefined ? 0 : timing.duration, timebase);
  const delay = msToFrame(timing.delay === undefined ? 0 : timing.delay, timebase);
  const endDelay = msToFrame(timing.endDelay === undefined ? 0 : timing.endDelay, timebase);
  for (const item of [duration, delay, endDelay]) {
    report.quantization.push({ what: 'timing', frames: item.frames, exact: item.exact, error: item.error });
  }

  const span = Math.max(1, duration.frames);
  const keyframes = Array.isArray(animation.keyframes) ? animation.keyframes : [];

  // 逐键算出"这一键上有哪些通道"。**先按通道各自成列**：缺的键不补 0，
  // 而是在该通道的子序列里前后相接（这就是"某个属性只在部分键上出现"的语义）。
  const perChannel = { opacity: [], x: [], y: [], scale: [], rotation: [] };
  // 关键帧里驱动的**静态样式**：这一版不支持 —— 明说，不许悄悄只取第一帧的值。
  // （引擎那边有 `effect.<i>.<param>` 通道能做到滤镜参数动起来，转译侧还没接。）
  let keyedStyle = null;
  const keyedFilters = [];
  keyframes.forEach((key, keyIndex) => {
    const frame = offsetToFrame(key.offset === undefined ? keyIndex / Math.max(1, keyframes.length - 1) : key.offset, span);
    report.quantization.push({
      what: 'animation[' + index + '].key[' + keyIndex + '].offset',
      frames: frame.frames,
      exact: frame.exact,
      error: frame.error,
    });
    const easing = key.easing === undefined ? timing.easing : key.easing;
    if (key.opacity !== undefined) perChannel.opacity.push({ frame: frame.frames, value: Number(key.opacity), easing });
    if (key.transform !== undefined) {
      const parsed = parseTransform(String(key.transform), report);
      for (const name of Object.keys(parsed)) perChannel[name].push({ frame: frame.frames, value: parsed[name], easing });
    }
    // 滤镜可以**只让参数动**（引擎有 `effect.<i>.<param>` 通道）—— 收到关键帧里统一处理。
    if (key.filter !== undefined) keyedFilters.push({ frame: frame.frames, text: String(key.filter), easing });
    if (key.borderRadius !== undefined) keyedStyle = 'borderRadius';
  });

  if (keyedFilters.length > 0) {
    const keyed = keyedFiltersToEffects(keyedFilters, report, target);
    if (keyed === null) return null;
    if (bits.effects.length > 0) {
      report.warnings.push({
        target,
        reason: 'style 上的 filter 与关键帧里的 filter 同时存在 —— 按**关键帧**走（静态那份只当作初值）',
      });
    }
    bits.effects = keyed.effects;
    for (const name of Object.keys(keyed.channels)) perChannel[name] = keyed.channels[name];
  }

  if (keyedStyle !== null) {
    report.skipped.push({
      target,
      reason: keyedStyle + ' 被关键帧驱动 —— 这一版只支持**静态**圆角（引擎没有"圆角随关键帧变"那条通道）',
    });
    return null;
  }

  // 只有一条键的通道先说一声（它是常量，不是动画）。
  for (const name of Object.keys(perChannel)) {
    if (perChannel[name].length === 1) {
      report.warnings.push({ target, reason: name + ' 只有一个键 —— 在层里等于一个常量，已按常量写入' });
    }
  }

  // ---- 同向重播的交界跳变 ----
  //
  // `normal` / `reverse` 重播时，交界帧上「前一遍的末值」与「后一遍的首值」撞在同一帧。
  //
  // **第 48 轮更正：这堵墙其实不存在。** 原先的说法是"一个键只能有一个 easing，所以表达不了"——
  // 那只说明**交界帧本身**放不下两个值，而正确的表达根本不需要挤在同一帧：
  //
  //   · CSS 的迭代是**半开区间** `[kB, (k+1)B)` ⇒ 第 B 帧取的是**后一遍**的首值；
  //   · 而本仓只在**整数帧**求值（铁律）⇒ 在 `B−1` 补一个键、值取**前一遍在那一帧的值**，
  //     逐整数帧的取值就与 CSS **完全一致**（B−1 是前一遍的 ✓、B 是后一遍的 ✓）。
  //   · 那个补出来的键带着 `B−1 → B` 这一段的一帧缓动，段内取值**永远不会被求值** ⇒ 取什么都无害。
  //
  // **第 49 轮的二阶更正**：光"在交界前一帧采样一次"**不够**。
  // 键的 easing 描述的是"从这个键到下一个键的整段"，把段尾从 `B` 挪到 `B−1` 会让**段长从 12 变 11**，
  // 于是段内每一帧都偏（`E(f/12)` vs `E(f/11)`），只有交界那两帧对。
  // 要逐帧精确，得把这一遍**按整数帧烘开**（每个整数帧一个键）—— 代价是文档里键数按遍长增长，
  // 那是**产品决定**。所以继续拒绝（响亮、安全），把代价写清楚（见 criteria 的 D7）。
  // `alternate` / `alternate-reverse` 的交界处首尾值天然相同，不受这条限制。
  if (iterations > 1 && (direction === 'normal' || direction === 'reverse')) {
    for (const name of Object.keys(perChannel)) {
      const column = perChannel[name];
      if (column.length < 2) continue;
      if (column[0].value !== column[column.length - 1].value) {
        report.skipped.push({
          target,
          reason:
            'iterations=' + iterations + ' + direction=' + direction + '：每遍交界处会跳回起始值，而 ' + name +
            ' 的首尾值不同（' + column[0].value + ' vs ' + column[column.length - 1].value +
            '）—— 每通道每帧只能有一个键，表达不了。alternate 或首尾同值可以',
        });
        return null;
      }
    }
  }

  // ---- 方向 = 镜像：反向的那一遍把**值序倒过来**、把每段的缓动**镜像** ----
  //
  // 镜像的来历：正向段是 value(u) = v_i + (v_{i+1}−v_i)·E(u)；倒着走时 u → 1−u，
  // 于是把它写成一个从 v_{i+1} 到 v_i 的段时，缓动必须是 E'(u) = 1 − E(1−u)。
  // 不能镜像的那一段**明说并跳过整条动画** —— 不近似。
  const emit = [];
  const isReversedPass = (passIndex) =>
    direction === 'reverse' ||
    (direction === 'alternate' && passIndex % 2 === 1) ||
    (direction === 'alternate-reverse' && passIndex % 2 === 0);

  for (let pass = 0; pass < iterations; pass += 1) {
    const reversed = isReversedPass(iterationStart + pass);
    const base = pass * span;
    for (const name of Object.keys(perChannel)) {
      const column = perChannel[name];
      if (column.length === 0) continue;
      if (!reversed) {
        column.forEach((point, pointIndex) => {
          // off-by-one：底座的第 i 个键取源的**第 i-1 个** easing（见文件头 D2 那条）。
          const easing = pointIndex === 0 ? 'linear' : column[pointIndex - 1].easing;
          emit.push({ frame: base + point.frame, target: name, value: point.value, easing });
        });
        continue;
      }
      const count = column.length;
      for (let pointIndex = 0; pointIndex < count; pointIndex += 1) {
        // 倒着走：输出的第 j 个点对应源的第 m 个点。
        const m = count - 1 - pointIndex;
        let easing = 'linear';
        if (pointIndex > 0) {
          const mirrored = mirrorEasing(column[m].easing);
          if (!mirrored.ok) {
            report.skipped.push({
              target,
              reason: 'direction=' + direction + '（第 ' + pass + ' 遍要反向）要镜像 ' + column[m].easing + '：' + mirrored.why,
            });
            return null;
          }
          easing = mirrored.text;
        }
        emit.push({ frame: base + (span - column[m].frame), target: name, value: column[m].value, easing });
      }
    }
  }

  if (emit.length === 0) {
    report.skipped.push({ target, reason: '没有任何认得的通道（opacity / translate / scale / rotate）' });
    return null;
  }
  emit.sort((a, b) => (a.frame === b.frame ? a.target.localeCompare(b.target) : a.frame - b.frame));

  // 相邻两遍在交界帧上会重合（前一遍的末点与后一遍的首点），而它们**值相同**
  // （值不同的跳变上面已经明说不支持）。这种情况下留**先来的那个**：
  // 一个键的 easing 说的是「进入这一帧的那一段」，而后来那个的 easing 是给「离开这一帧」用的，
  // 本来就不该挂在这一帧上 —— 留错了会让进入那段丢掉缓动（看着只是「缓动没那么明显」）。
  const deduped = [];
  for (const key of emit) {
    const last = deduped[deduped.length - 1];
    if (last !== undefined && last.frame === key.frame && last.target === key.target) continue;
    deduped.push(key);
  }

  const start = Math.max(0, delay.frames) + iterationStart * span;
  const layer = {
    id: target,
    start,
    // 右开区间：要让**最后一个键**落在里面（键是层内相对帧），所以 +1；
    // 长度按 **iterations × span** 算（方向展开后每一遍都要装得下）；
    // endDelay 与 fill=forwards 的「保持末值」都由「区间更长的层」吸收。
    end: start + iterations * span + 1 + Math.max(0, endDelay.frames),
    opacity: 1,
    note: 'waapi2doc 由快照生成（target=' + target + '）',
    keyframes: deduped,
  };
  // 只在实际有值时写进去 —— 缺省不写，老工程重写时一个字节都不多。
  if (bits.effects.length > 0) layer.effects = bits.effects;
  if (bits.blend !== undefined && bits.blend !== 'normal') layer.blend = bits.blend;
  if (bits.cornerRadius !== undefined && bits.cornerRadius > 0) layer.corner_radius = bits.cornerRadius;
  if (bits.clip !== undefined) layer.clip = bits.clip;
  report.layers.push({ id: layer.id, start: layer.start, end: layer.end, keyframes: deduped.length });
  return layer;
}

function main() {
  const argv = process.argv.slice(2);
  const strict = argv.includes('--strict');
  const files = argv.filter((arg) => !arg.startsWith('--'));
  if (files.length === 0) {
    console.error('用法：node scripts/waapi2doc.mjs <快照.json> [输出.doc.json] [报告.json] [--strict]');
    process.exit(2);
  }
  const inputPath = resolve(files[0]);
  // 缺省写到 target/ 下：那是本仓约定的"跑出来的东西"目录（不进 git）。
  const outputPath = resolve(files[1] === undefined ? 'target/waapi-demo/out.doc.json' : files[1]);
  const reportPath = resolve(files[2] === undefined ? outputPath.replace(/\.doc\.json$/, '') + '.report.json' : files[2]);

  const snapshot = JSON.parse(readFileSync(inputPath, 'utf8'));
  const timebase = snapshot.fps === undefined ? { num: 30, den: 1 } : { num: Number(snapshot.fps.num), den: Number(snapshot.fps.den) };
  const size = snapshot.size === undefined ? { width: 1920, height: 1080 } : snapshot.size;

  const report = { input: inputPath, output: outputPath, fps: timebase, quantization: [], warnings: [], skipped: [], layers: [] };
  const animations = Array.isArray(snapshot.animations) ? snapshot.animations : [];
  const layers = [];
  animations.forEach((animation, index) => {
    const layer = animationToLayer(animation, index, timebase, report);
    if (layer !== null) layers.push(layer);
  });

  const worst = report.quantization.reduce((acc, item) => Math.max(acc, Math.abs(item.error)), 0);
  report.worst_error_frames = worst;

  const doc = {
    project_schema: 1,
    generator: { app: 'waapi2doc', version: '0.0.1' },
    meta: { title: 'WAAPI 转译（' + inputPath.split(/[\\/]/).pop() + '）', created_at: null, modified_at: null },
    assets: [],
    timeline: {
      // 4 = layer::LAYER_SCHEMA_VERSION（当前）。
      schema: 4,
      timebase,
      markers: [],
      // **一条动画一条轨**，不是全塞进一条轨：
      // 同轨的层不许重叠（probe 的 layer_overlap），而这里的动画本来就是**同时在演**的
      // 独立元素；轨序 = 叠放顺序（谁压谁）。
      tracks: layers.map((layer) => ({ id: 'waapi-' + layer.id, kind: 'video', layers: [layer] })),
    },
    view: { playhead: 0, selection: null, zoom: 1 },
    // 画布底色：快照给了就带上（#rgb / #rrggbb / #rrggbbaa）。它是**零图层**的落地方式 ——
    // 底色不依赖任何图层，所以它能不能出图，直接判定 render_hints 有没有被渲染器吃进去。
    render_hints: Object.assign(
      { width: Number(size.width), height: Number(size.height), format: 'mp4' },
      snapshot.background === undefined ? {} : { background: String(snapshot.background) },
    ),
    extensions: {},
  };

  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, JSON.stringify(doc, null, 2) + '\n', 'utf8');
  writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n', 'utf8');

  console.log('waapi2doc：' + animations.length + ' 条动画 → ' + layers.length + ' 个图层');
  console.log('  量化点 ' + report.quantization.length + ' 个，最大误差 ' + worst.toFixed(4) + ' 帧（报告：' + reportPath + '）');
  for (const item of report.skipped) console.log('  **跳过** ' + item.target + '：' + item.reason);
  for (const item of report.warnings) console.log('  警告 ' + JSON.stringify(item));
  if (report.skipped.length > 0 && strict) {
    console.error('有 ' + report.skipped.length + ' 条被跳过（--strict）');
    process.exit(EXIT_NOTHING);
  }
  if (layers.length === 0) {
    console.error('一条都没转成 —— 不许把空工程写出去当成功');
    process.exit(EXIT_NOTHING);
  }
  process.exit(EXIT_OK);
}


/**
 * **关键帧驱动的滤镜** → 特效 + 参数通道（`effect.<i>.<param>`）。
 *
 * 引擎那边本来就有这条通道（`crates/dhampir-core/src/compose.rs` 会用 `effect.<下标>.<参数名>`
 * 覆盖特效参数，`project.rs` 还会校验下标与参数名），所以"模糊随关键帧变"本来就能表达 ——
 * 之前转译器遇到它就整条跳过，那是**转译器的锅**。
 *
 * 两条限制（都明说，不近似）：
 *   · 关键帧之间的**函数列表必须一致**（名称与顺序）：这一版只能让参数变，不能让函数增减；
 *   · 参数值必须能解析成数（本仓的特效参数都是数）。
 *
 * 返回 `{ effects, channels }`，或 `null`（已把理由写进报告）。
 */
function keyedFiltersToEffects(list, report, target) {
  const parsed = [];
  for (const item of list) {
    const functions = [];
    for (const piece of splitFilterList(item.text)) {
      const match = piece.match(/^([a-z-]+)\((.*)\)$/);
      if (match === null) {
        report.skipped.push({ target, reason: '看不懂这段 filter：' + piece });
        return null;
      }
      const converted = filterItemToEffect(match[1], match[2]);
      if (converted.refused !== undefined) {
        report.skipped.push({ target, reason: converted.refused });
        return null;
      }
      functions.push({ name: match[1], effect: converted.effect, approximate: converted.approximate, why: converted.why });
    }
    parsed.push({ frame: item.frame, easing: item.easing, functions });
  }
  const shape = (item) => item.functions.map((entry) => entry.name).join(' ');
  const first = shape(parsed[0]);
  for (const item of parsed) {
    if (shape(item) !== first) {
      report.skipped.push({
        target,
        reason: '关键帧之间滤镜的**函数列表**变了（' + first + ' → ' + shape(item) + '）—— 这一版只能让参数变，不能让函数增减',
      });
      return null;
    }
  }
  const effects = parsed[0].functions.map((entry) => entry.effect);
  const channels = {};
  parsed[0].functions.forEach((entry, index) => {
    if (entry.approximate === true) report.warnings.push({ target, reason: entry.why });
    for (const param of Object.keys(entry.effect.params)) {
      const values = parsed.map((item) => item.functions[index].effect.params[param]);
      if (!values.some((value) => value !== values[0])) continue;
      // 缓动按既有约定挂在"这一段的起点键"上，展开时的 off-by-one 由公共那段处理。
      channels['effect.' + index + '.' + param] = parsed.map((item, at) => ({
        frame: item.frame,
        value: values[at],
        easing: item.easing,
      }));
    }
  });
  return { effects, channels };
}
/**
 * 一条 `clip-path:` 值 → 本仓的裁剪形状（`{ shape }` 或 `{ refused }`）。
 *
 * # 只认三种
 *
 * `circle()` / `ellipse()` / `inset()` —— 引擎那边也是这三种（每像素一个覆盖度那套 SDF）。
 * `polygon()` / `path()` / `url()` **明说拒绝**（见 criteria 的 D11）。
 *
 * # 两条口径
 *
 * 1. **半径一律要文档像素**：`circle(50%)` 的百分比是相对元素尺寸算的，而本仓的半径是
 *    文档像素 —— 转译器不知道图层尺寸，所以**百分比半径拒绝**，不猜。
 * 2. **`at` 位置一律要百分比**（`at 25% 75%`）：本仓的 `center` 就是归一化比例，两边同一口径。
 *    像素位置要图层的像素尺寸才能换算，同样拒绝。省略 `at`、写 `center`、写 `50%` 都等于居中。
 */
function clipPathToShape(text) {
  const raw = String(text).trim();
  const lower = raw.toLowerCase();
  if (lower === '' || lower === 'none') return {};
  const match = lower.match(/^([a-z-]+)\((.*)\)$/);
  if (match === null) return { refused: '看不懂这段 clip-path：' + raw };
  const name = match[1];
  if (name === 'polygon' || name === 'path' || name === 'url') {
    return {
      refused: 'clip-path: ' + name + '() —— 本仓只做 circle / ellipse / inset（每像素一个覆盖度那套基建表达不了多边形，见 D11）',
    };
  }
  // 把 `at <位置>` 切出来（顶层，没有嵌套括号）。
  const inner = match[2].trim();
  const atIndex = inner.toLowerCase().indexOf(' at ');
  const body = (atIndex < 0 ? inner : inner.slice(0, atIndex)).trim();
  const positionText = atIndex < 0 ? '' : inner.slice(atIndex + 4).trim();
  const position = (() => {
    if (positionText === '') return { center: undefined };
    const parts = positionText.split(/\s+/).filter((part) => part !== '');
    if (parts.length > 2) return { refused: 'at 位置只认一到两个值：' + positionText };
    const one = (value) => {
      const piece = String(value).toLowerCase();
      if (piece === 'center') return { value: 0.5 };
      const percent = piece.match(/^(-?[0-9.]+)%$/);
      if (percent !== null) return { value: Number(percent[1]) / 100 };
      return { refused: 'at 位置只认百分比（本仓的 center 是归一化比例）：' + value };
    };
    const first = one(parts[0]);
    const second = parts.length > 1 ? one(parts[1]) : { value: 0.5 };
    if (first.refused !== undefined) return { refused: first.refused };
    if (second.refused !== undefined) return { refused: second.refused };
    if (first.value === 0.5 && second.value === 0.5) return { center: undefined };
    return { center: [first.value, second.value] };
  })();
  if (position.refused !== undefined) return { refused: position.refused };
  const lengths = (list, what) => {
    const out = [];
    for (const piece of list) {
      const px = String(piece).trim().match(/^(-?[0-9.]+)px$/);
      if (px === null) {
        return { refused: what + '只认像素值（本仓的长度是文档像素）：' + piece };
      }
      out.push(Number(px[1]));
    }
    return { values: out };
  };
  const args = body.split(/\s+/).filter((part) => part !== '');
  if (name === 'circle') {
    const got = lengths(args, 'circle 的半径');
    if (got.refused !== undefined) return { refused: got.refused };
    if (got.values.length !== 1) return { refused: 'circle 要一个半径：' + body };
    return { shape: { kind: 'circle', radius: got.values[0], center: position.center } };
  }
  if (name === 'ellipse') {
    const got = lengths(args, 'ellipse 的半径');
    if (got.refused !== undefined) return { refused: got.refused };
    if (got.values.length !== 2) return { refused: 'ellipse 要两个半径：' + body };
    return {
      shape: { kind: 'ellipse', radius_x: got.values[0], radius_y: got.values[1], center: position.center },
    };
  }
  if (name === 'inset') {
    const roundIndex = args.findIndex((part) => part.toLowerCase() === 'round');
    const sideArgs = (roundIndex < 0 ? args : args.slice(0, roundIndex)).filter((part) => part !== '');
    const roundArgs = roundIndex < 0 ? [] : args.slice(roundIndex + 1);
    const sides = lengths(sideArgs, 'inset 的四条边');
    if (sides.refused !== undefined) return { refused: sides.refused };
    const round = lengths(roundArgs, 'inset 的圆角');
    if (round.refused !== undefined) return { refused: round.refused };
    if (round.values.length > 1) return { refused: 'inset 的 round 只认一个值：' + body };
    // CSS 简写：1 个值 = 四边相同；2 = 上下 / 左右；3 = 上 / 左右 / 下；4 = 上 右 下 左。
    const v = sides.values;
    let top;
    let right;
    let bottom;
    let left;
    if (v.length === 1) [top, right, bottom, left] = [v[0], v[0], v[0], v[0]];
    else if (v.length === 2) [top, bottom, right, left] = [v[0], v[0], v[1], v[1]];
    else if (v.length === 3) [top, right, bottom, left] = [v[0], v[1], v[2], v[1]];
    else if (v.length === 4) [top, right, bottom, left] = [v[0], v[1], v[2], v[3]];
    else return { refused: 'inset 要一到四个值：' + body };
    if (positionText !== '') return { refused: 'inset 不支持 at 位置（CSS 里 inset 本来也没有 at）' };
    return {
      shape: { kind: 'inset', top, right, bottom, left, radius: round.values.length === 1 ? round.values[0] : 0 },
    };
  }
  return { refused: '本仓不认识的裁剪形状：' + name };
}

/** CSS 的一个滤镜函数 → 本仓的特效参数。**口径全在 plan/web-animation-criteria.md 的 D5/D6 里。** */
function filterItemToEffect(name, rawArgs) {
  const args = rawArgs.split(',').map((part) => part.trim()).filter((part) => part !== '');
  const number = (text, fallback) => {
    if (text === undefined) return fallback;
    const value = Number(String(text).replace(/px$|deg$|%$/, ''));
    return Number.isFinite(value) ? value : fallback;
  };
  if (name === 'blur') {
    // D5：CSS 的 blur(R) 参数**就是 σ**，而本仓 σ = radius/2 ⇒ radius = 2R。
    const sigma = number(args[0], 0);
    return { effect: { kind: 'gaussian_blur', params: { radius: sigma * 2 } }, approximate: false };
  }
  if (name === 'contrast') {
    // D6：两边同一个公式 (c−0.5)·a+0.5，逐值等价。百分比 → 倍率。
    const raw = args[0] === undefined ? '1' : String(args[0]);
    const value = raw.endsWith('%') ? Number(raw.slice(0, -1)) / 100 : number(raw, 1);
    return { effect: { kind: 'contrast', params: { amount: value } }, approximate: false };
  }
  if (name === 'saturate') {
    const raw = args[0] === undefined ? '1' : String(args[0]);
    const value = raw.endsWith('%') ? Number(raw.slice(0, -1)) / 100 : number(raw, 1);
    // 第 45 轮起本仓有了按**规范权重**的那条 ⇒ 与 saturate() 精确等价（不再是近似）。
    return { effect: { kind: 'saturation_css', params: { amount: value } } };
  }
  if (name === 'brightness') {
    // 第 42 轮起本仓有了**乘性**那条（`brightness_multiply`）：CSS 的 `brightness(k)` 就是 `c·k`，
    // 与它精确等价 —— 所以这里不再是"拒绝"，而是直接映射过去。
    const raw = args[0] === undefined ? '1' : String(args[0]);
    const value = raw.endsWith('%') ? Number(raw.slice(0, -1)) / 100 : number(raw, 1);
    return { effect: { kind: 'brightness_multiply', params: { factor: value } } };
  }
  if (name === 'hue-rotate') {
    // 第 44 轮起本仓有了按**规范矩阵**的那条 ⇒ CSS 的 hue-rotate(θ) 直接映射过去。
    const raw = args[0] === undefined ? '0' : String(args[0]);
    const value = number(raw.endsWith('deg') ? raw.slice(0, -3) : raw, 0);
    return { effect: { kind: 'hue_rotate_css', params: { degrees: value } } };
  }
  return { refused: 'DOM / 引擎都还没实现这条滤镜：' + name };
}

/** 把一条 `filter:` 值拆成若干函数（只切顶层括号）。 */
function splitFilterList(text) {
  const out = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i];
    if (ch === '(') depth += 1;
    else if (ch === ')') depth -= 1;
    else if (depth === 0 && /\s/.test(ch)) {
      const piece = text.slice(start, i).trim();
      if (piece !== '') out.push(piece);
      start = i + 1;
    }
  }
  const last = text.slice(start).trim();
  if (last !== '') out.push(last);
  return out;
}

/** 本仓的混合模式串 → CSS（与 web/dom-css.mjs 那份**必须一致**；判据跨文件比对）。 */
const BLEND_TO_DHAMPIR = {
  normal: 'normal',
  multiply: 'multiply',
  screen: 'screen',
  darken: 'darken',
  lighten: 'lighten',
  overlay: 'overlay',
  'soft-light': 'soft_light',
  difference: 'difference',
};

/**
 * **引擎实现了哪几条混合模式** —— 与 crates/dhampir-timeline/src/layer.rs 的
 * `matches!(self, Self::…)` **必须一致**（判据读源码比对）。转译器把它们写进工程没问题
 * （契约接受），但要**报出来**：那几条出片会拒绝整帧。
 */
export const ENGINE_IMPLEMENTED_BLENDS = ['normal', 'add', 'multiply', 'screen', 'darken', 'lighten', 'overlay', 'soft_light', 'difference'];

/** 一条静态样式 → 本仓的 effect / blend / corner_radius。认不出的**明说**，不近似。 */
function styleToLayerBits(style, report, target) {
  const effects = [];
  let blend;
  let cornerRadius;
  const bag = style === undefined ? {} : style;
  const filterText = bag.filter === undefined ? '' : String(bag.filter).trim();
  if (filterText !== '' && filterText !== 'none') {
    for (const piece of splitFilterList(filterText)) {
      const match = piece.match(/^([a-z-]+)\((.*)\)$/);
      if (match === null) {
        report.skipped.push({ target, reason: '看不懂这段 filter：' + piece });
        return null;
      }
      const converted = filterItemToEffect(match[1], match[2]);
      if (converted.refused !== undefined) {
        report.skipped.push({ target, reason: converted.refused });
        return null;
      }
      effects.push(converted.effect);
      if (converted.approximate === true) report.warnings.push({ target, reason: converted.why });
    }
  }
  const blendText = bag['mix-blend-mode'] === undefined ? undefined : String(bag['mix-blend-mode']).trim();
  if (blendText !== undefined && blendText !== '') {
    if (blendText === 'plus-lighter') {
      blend = 'add';
      report.warnings.push({ target, reason: 'plus-lighter → add：两者未必逐值相同（预乘 / 夹取口径未对账，见 D9）' });
    } else if (BLEND_TO_DHAMPIR[blendText] !== undefined) {
      blend = BLEND_TO_DHAMPIR[blendText];
      if (!ENGINE_IMPLEMENTED_BLENDS.includes(blend)) {
        report.warnings.push({ target, reason: '混合模式 ' + blend + '：**引擎还没实现**（出片会拒绝这一帧）—— 预览画得出来（D9）' });
      }
    } else {
      report.skipped.push({ target, reason: '不认识的混合模式：' + blendText });
      return null;
    }
  }
  // 裁剪形状（D11）：只认 circle / ellipse / inset，其余明说。
  let clip;
  const clipText = bag['clip-path'] === undefined ? undefined : String(bag['clip-path']).trim();
  if (clipText !== undefined && clipText !== '') {
    const parsed = clipPathToShape(clipText);
    if (parsed.refused !== undefined) {
      report.skipped.push({ target, reason: parsed.refused });
      return null;
    }
    if (parsed.shape !== undefined) {
      clip = parsed.shape;
      // 形状里可能出现 undefined 的 center（居中）—— 去掉，别把 null 写进工程。
      for (const key of Object.keys(clip)) if (clip[key] === undefined) delete clip[key];
    }
  }

  const radiusText = bag['border-radius'] === undefined ? undefined : String(bag['border-radius']).trim();
  if (radiusText !== undefined && radiusText !== '') {
    const parts = radiusText.split(/\s+/).filter((part) => part !== '');
    if (parts.length !== 1) {
      // 四个角各写一个这种形状，本仓的字段是**单一半径**表达不了的 —— 明说。
      report.skipped.push({ target, reason: 'border-radius 只支持单一半径，得到：' + radiusText });
      return null;
    }
    const value = Number(parts[0].replace(/px$/, ''));
    if (!Number.isFinite(value) || value < 0) {
      report.skipped.push({ target, reason: 'border-radius 读不出数字：' + radiusText });
      return null;
    }
    cornerRadius = value;
  }
  return { effects, blend, cornerRadius, clip };
}

main();

