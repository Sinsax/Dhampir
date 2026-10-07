#!/usr/bin/env node
// waapi2doc 的判据守卫：转译器有没有把该保的保住、该报的报出来。
//
//   node scripts/check-waapi2doc.mjs              # 判据（绿/红）
//   node scripts/check-waapi2doc.mjs --self-test  # 反向验证：每条判据都真的会红
//
// # 为什么捕获子进程输出不用管道
//
// 本机 agent 会话里**连 stdout 管道都起不来**：spawnSync 直接 EPERM
// （仓库里 scripts/spawn-tool.mjs 记的是"输出管道是好的"，那是另一台会话的读数）。
// 所以这里把子进程的 stdout/stderr **重定向到文件**再读回来。
// 这不是垫片：起不来时 status 是 null，判据照样红（fail-closed）。

import { spawnSync } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const OUT_DIR = join(REPO_ROOT, 'target', 'waapi-demo', 'guard');
const LOG_DIR = join(REPO_ROOT, 'target', 'agent-logs');
const FIXTURE = 'fixtures/waapi-snapshot.sample.json';
const TRANSFORM_TARGETS = ['opacity', 'x', 'y', 'scale', 'rotation'];

/** 起一个 node 脚本，输出重定向到文件（不用管道）。 */
function runNode(args, label) {
  mkdirSync(LOG_DIR, { recursive: true });
  const outPath = join(LOG_DIR, 'guard-' + label + '.out');
  const errPath = join(LOG_DIR, 'guard-' + label + '.err');
  const outFd = openSync(outPath, 'w');
  const errFd = openSync(errPath, 'w');
  const result = spawnSync(process.execPath, args, {
    cwd: REPO_ROOT,
    windowsHide: true,
    stdio: ['ignore', outFd, errFd],
  });
  closeSync(outFd);
  closeSync(errFd);
  return {
    status: result.status,
    error: result.error === undefined ? null : String(result.error.message),
    stdout: existsSync(outPath) ? readFileSync(outPath, 'utf8') : '',
    stderr: existsSync(errPath) ? readFileSync(errPath, 'utf8') : '',
  };
}

/**
 * 判据本体。纯函数：给一份 doc 与一份报告，返回问题清单（空 = 绿）。
 *
 * 判据都是**结构性的**，不是"再实现一遍转译器"——
 * 唯一带具体数值的那几条（off-by-one 的缓动）钉的是**样本工程的既定答案**。
 */
export function judgeContract(doc, report) {
  const problems = [];
  const timeline = doc.timeline === undefined ? {} : doc.timeline;

  if (timeline.schema !== 4) {
    problems.push('timeline.schema 应当是 4（当前 LAYER_SCHEMA_VERSION），得到 ' + String(timeline.schema));
  }
  if (timeline.timebase === undefined || !(timeline.timebase.num > 0) || !(timeline.timebase.den > 0)) {
    problems.push('timeline.timebase 必须是正有理数');
  }
  if (!Array.isArray(timeline.tracks)) {
    problems.push('timeline.tracks 必须是数组');
    return problems;
  }

  // 比较键时只看这三样：doc 里的键还带 target，而 target 与这条判据无关。
  const shape = (keys) => keys.map((key) => ({ frame: key.frame, value: key.value, easing: key.easing }));

  const seenLayers = new Set();
  const byId = new Map();
  for (const track of timeline.tracks) {
    if (!Array.isArray(track.layers)) {
      problems.push('轨道 ' + String(track.id) + ' 没有 layers 数组');
      continue;
    }
    for (const layer of track.layers) {
      if (seenLayers.has(layer.id)) problems.push('层 id 重复：' + String(layer.id) + '（v2 起要求全局唯一）');
      seenLayers.add(layer.id);
      byId.set(layer.id, layer);
      const span = layer.end - layer.start;
      if (!(span > 0)) problems.push('层 ' + layer.id + ' 的区间不是正长度：[' + layer.start + ',' + layer.end + ')');
      // 契约里的通道有两类：
      //   · 五个固定通道（transform 的四个 + opacity）；
      //   · `effect.<下标>.<参数名>` —— 引擎的求值会用它覆盖特效参数，契约层也校验下标与参数名。
      // 这条曾经只认第一类，于是"参数随关键帧变的模糊"被守卫自己挡住了（第 18 轮修）。
      const effectTarget = (name) => {
        const match = String(name).match(/^effect\.(\d+)\.([A-Za-z_][A-Za-z0-9_]*)$/);
        if (match === null) return null;
        return { index: Number(match[1]), param: match[2] };
      };
      const effects = layer.effects === undefined ? [] : layer.effects;
      for (const key of layer.keyframes === undefined ? [] : layer.keyframes) {
        if (TRANSFORM_TARGETS.includes(key.target)) {
          // 固定通道：认得。
        } else {
          const target = effectTarget(key.target);
          if (target === null) {
            problems.push('层 ' + layer.id + ' 的关键帧 target 既不是固定通道、也不是 effect.<下标>.<参数名>：' + String(key.target));
          } else if (target.index >= effects.length) {
            problems.push(
              '层 ' + layer.id + ' 的关键帧指向 effect.' + target.index + '，可这一层只有 ' + effects.length + ' 条特效',
            );
          } else if (!Object.prototype.hasOwnProperty.call(effects[target.index].params, target.param)) {
            problems.push(
              '层 ' + layer.id + ' 的关键帧指向 ' + effects[target.index].kind + ' 没有的参数：' + target.param,
            );
          }
        }
        if (!(key.frame >= 0 && key.frame < span)) {
          problems.push('层 ' + layer.id + ' 的关键帧落在层外：frame=' + key.frame + '，层内合法区间 0..' + (span - 1));
        }
      }
    }
  }

  if (!Array.isArray(report.quantization) || report.quantization.length === 0) {
    problems.push('报告里没有量化记录 —— 秒 → 帧的每一步都必须留痕');
  } else {
    for (const item of report.quantization) {
      if (item.exact === undefined || item.frames === undefined || item.error === undefined) {
        problems.push('量化记录缺少 exact / frames / error 之一：' + JSON.stringify(item));
      }
    }
  }
  for (const item of report.skipped === undefined ? [] : report.skipped) {
    if (!item.target || !item.reason) problems.push('跳过项必须写明是哪个目标、为什么：' + JSON.stringify(item));
  }

  // ---- 样本工程的既定答案（钉 off-by-one）----
  const title = byId.get('title');
  if (title === undefined) {
    problems.push('样本里应当有 title 层');
  } else {
    const opacity = title.keyframes.filter((key) => key.target === 'opacity').sort((a, b) => a.frame - b.frame);
    const expected = [
      { frame: 0, value: 0, easing: 'linear' },
      { frame: 30, value: 1, easing: 'cubic-bezier(0.2, 0.8, 0.4, 1)' },
    ];
    if (JSON.stringify(shape(opacity)) !== JSON.stringify(expected)) {
      problems.push('title 的 opacity 键与既定答案不符（off-by-one 的缓动挂在**终点**键上）：' + JSON.stringify(shape(opacity)));
    }
  }
  const note = byId.get('note');
  if (note === undefined) {
    problems.push('样本里应当有 note 层');
  } else {
    const opacity = note.keyframes.filter((key) => key.target === 'opacity').sort((a, b) => a.frame - b.frame);
    const expected = [
      { frame: 0, value: 0.2, easing: 'linear' },
      { frame: 18, value: 1, easing: 'steps(4, jump-both)' },
    ];
    if (JSON.stringify(shape(opacity)) !== JSON.stringify(expected)) {
      problems.push('note 的 opacity 键与既定答案不符（缓动串必须原样透传）：' + JSON.stringify(shape(opacity)));
    }
  }
  // ① `badge`（iterations=3 + alternate）**现在能转了**：反向的那一遍靠**镜像**表达。
  //    这里钉的就是镜像本身 —— 12 帧那个键挂 `ease_in`（进入它的那一段），24 帧那个挂 `ease_out`。
  const badge = byId.get('badge');
  if (badge === undefined) {
    problems.push('样本里应当有 badge 层（alternate 现在能转了）');
  } else {
    const opacity = badge.keyframes.filter((key) => key.target === 'opacity').sort((a, b) => a.frame - b.frame);
    const expected = [
      { frame: 0, value: 0.3, easing: 'linear' },
      { frame: 12, value: 1, easing: 'ease_in' },
      { frame: 24, value: 0.3, easing: 'ease_out' },
      { frame: 36, value: 1, easing: 'ease_in' },
    ];
    if (JSON.stringify(shape(opacity)) !== JSON.stringify(expected)) {
      problems.push('badge 的 opacity 键与既定答案不符（alternate 的镜像：24 帧那个键应当是 ease_out）：' + JSON.stringify(shape(opacity)));
    }
    if (badge.end !== 37) problems.push('badge 的层长应当是 3 遍 × 12 帧 + 1 = 37，得到 ' + badge.end);
  }
  // ② 表达不了的那条**必须出现在报告里** —— 静默丢弃正是这个项目最要避免的。
  const skipped = (report.skipped === undefined ? [] : report.skipped).map((item) => item.target);
  if (!skipped.includes('sparkle')) {
    problems.push('样本里 sparkle（back_out 镜像后不在词汇表里）表达不了，报告里却没有它 —— 静默丢弃');
  }
  const sparkle = (report.skipped === undefined ? [] : report.skipped).find((item) => item.target === 'sparkle');
  if (sparkle !== undefined && !/back_out/.test(String(sparkle.reason))) {
    problems.push('sparkle 的跳过理由应当点出 back_out：' + JSON.stringify(sparkle));
  }

  // ③ 静态样式 → 声明式数据（D5 / D6 / D9 / D10 那几笔债的转译侧）。
  const card = byId.get('card');
  if (card === undefined) {
    problems.push('样本里应当有 card 层（滤镜 + 混合模式 + 圆角）');
  } else {
    const wanted = [
      // D5：blur(4px) 的参数是 σ，本仓 radius = 2σ = 8。
      { kind: 'gaussian_blur', param: 'radius', value: 8 },
      { kind: 'contrast', param: 'amount', value: 2 },
      // 第 45 轮起按**规范权重**那条（与 CSS saturate() 精确等价）。
      { kind: 'saturation_css', param: 'amount', value: 0.5 },
    ];
    const effects = card.effects === undefined ? [] : card.effects;
    if (effects.length !== wanted.length) {
      problems.push('card 应当有 ' + wanted.length + ' 条特效，得到 ' + effects.length + '：' + JSON.stringify(effects));
    }
    for (const want of wanted) {
      const hit = effects.find((effect) => effect.kind === want.kind);
      if (hit === undefined) {
        problems.push('card 缺特效 ' + want.kind);
      } else if (!(Math.abs(Number(hit.params[want.param]) - want.value) <= 1e-6)) {
        // 写成 `!(... <= tol)` 而不是 `... > tol`：**NaN 比较漏洞** —— 参数被删掉时 Number(undefined) 是 NaN，
        // 而 `NaN > tol` 是 false，那个洞会让"字段没了"悄悄穿过判据（自检抓过一次）。
        problems.push(want.kind + ' 的 ' + want.param + ' 应当是 ' + want.value + '，得到 ' + hit.params[want.param]);
      }
    }
    if (card.blend !== 'overlay') problems.push('card 的混合模式应当是 overlay，得到 ' + String(card.blend));
    if (!(Math.abs(Number(card.corner_radius) - 12) <= 1e-6)) problems.push('card 的圆角应当是 12，得到 ' + String(card.corner_radius));
    // 第 31 轮起 overlay 引擎也实现了 ⇒ **不该再有"引擎还没实现"的警告**（假消息比不报更坏）。
    const blendWarning = (report.warnings === undefined ? [] : report.warnings).find((item) => /混合模式/.test(String(item.reason)));
    if (blendWarning !== undefined) problems.push('overlay 已经实现了，不该再报缺口：' + JSON.stringify(blendWarning));
    // 第 45 轮起 `saturate()` 有**精确对应**（规范权重那条）⇒ 不该再有近似警告。
    const satWarning = (report.warnings === undefined ? [] : report.warnings).find((item) => /saturat/i.test(String(item.reason)));
    if (satWarning !== undefined) problems.push('saturate() 已经精确对应了，不该再报近似：' + JSON.stringify(satWarning));
  }
  // ⑤ 裁剪形状（D11）：三种形状 + 两条口径（半径要像素、位置要百分比）。
  const clipWant = { kind: 'circle', radius: 40, center: [0.25, 0.75] };
  if (JSON.stringify(card === undefined ? undefined : card.clip) !== JSON.stringify(clipWant)) {
    problems.push('card 的裁剪形状应当是 ' + JSON.stringify(clipWant) + '，得到 ' + JSON.stringify(card === undefined ? undefined : card.clip));
  }
  const band = byId.get('band');
  // `inset(10px 20px round 4px)`：CSS 简写 2 个值 = 上下 / 左右。
  const insetWant = { kind: 'inset', top: 10, right: 20, bottom: 10, left: 20, radius: 4 };
  if (JSON.stringify(band === undefined ? undefined : band.clip) !== JSON.stringify(insetWant)) {
    problems.push('band 的内缩应当是 ' + JSON.stringify(insetWant) + '，得到 ' + JSON.stringify(band === undefined ? undefined : band.clip));
  }
  const poly = (report.skipped === undefined ? [] : report.skipped).find((item) => item.target === 'poly');
  if (poly === undefined) {
    problems.push('poly（polygon）表达不了，报告里却没有它 —— 静默丢弃');
  } else if (!/polygon/.test(String(poly.reason))) {
    problems.push('poly 的跳过理由要点出 polygon：' + JSON.stringify(poly));
  }

  // ⑥ 关键帧驱动的滤镜 → `effect.<i>.<param>` 通道（引擎本来就有这条通道，是转译器之前没接）。
  const pulse = byId.get('pulse');
  if (pulse === undefined) {
    problems.push('样本里应当有 pulse 层（参数随关键帧变的模糊）');
  } else {
    const effects = pulse.effects === undefined ? [] : pulse.effects;
    const base = effects.length === 1 ? effects[0] : undefined;
    if (base === undefined || base.kind !== 'gaussian_blur' || !(Math.abs(Number(base.params.radius) - 4) <= 1e-6)) {
      problems.push('pulse 的基础特效应当是 gaussian_blur radius 4（2×2），得到 ' + JSON.stringify(effects));
    }
    const channel = shape(
      pulse.keyframes.filter((key) => key.target === 'effect.0.radius').sort((a, b) => a.frame - b.frame),
    );
    const want = [
      { frame: 0, value: 4, easing: 'linear' },
      { frame: 12, value: 24, easing: 'ease_in' },
    ];
    if (JSON.stringify(channel) !== JSON.stringify(want)) {
      problems.push('pulse 的参数通道应当是 ' + JSON.stringify(want) + '，得到 ' + JSON.stringify(channel));
    }
  }
  const wobble = (report.skipped === undefined ? [] : report.skipped).find((item) => item.target === 'wobble');
  if (wobble === undefined) {
    problems.push('wobble（关键帧之间函数列表变了）表达不了，报告里却没有它 —— 静默丢弃');
  } else if (!/函数列表/.test(String(wobble.reason))) {
    problems.push('wobble 的跳过理由要点出「函数列表」：' + JSON.stringify(wobble));
  }

  // ⑦ `linear()` 断点表的**镜像**（第 38 轮）：`alternate` 的反向那一遍要把折线
  //    值取反、位置取反、再升序。原式 (0,0)(0.75,0.25)(1,1) ⇒ 镜像 (0,0)(0.25,0.75)(1,1)。
  const wave = byId.get('wave');
  if (wave === undefined) {
    problems.push('样本里应当有 wave 层（alternate + linear() 断点表）');
  } else {
    const opacity = shape(wave.keyframes.filter((key) => key.target === 'opacity').sort((a, b) => a.frame - b.frame));
    const want = [
      { frame: 0, value: 0, easing: 'linear' },
      { frame: 12, value: 1, easing: 'linear(0, 0.25 75%, 1)' },
      { frame: 24, value: 0, easing: 'linear(0 0%, 0.75 25%, 1 100%)' },
    ];
    if (JSON.stringify(opacity) !== JSON.stringify(want)) {
      problems.push('wave 的键与既定答案不符（反向那一遍的 linear() 应当被镜像）：' + JSON.stringify(opacity));
    }
  }

  //    而是要落成 `brightness_multiply`。（判据跟着事实走，而不是跟着历史。）
  const glow = byId.get('glow');
  if (glow === undefined) {
    problems.push('样本里应当有 glow 层');
  } else {
    const bright = (glow.effects === undefined ? [] : glow.effects).find((item) => item.kind === 'brightness_multiply');
    if (bright === undefined) {
      problems.push('glow 的 CSS brightness 应当映射成乘性那条：' + JSON.stringify(glow.effects));
    } else if (Math.abs(Number(bright.params.factor) - 1.5) > 1e-6) {
      problems.push('glow 的 factor 应当是 1.5（CSS 里那个 k）：' + JSON.stringify(bright.params));
    }
  }
  const stillSkipped = (report.skipped === undefined ? [] : report.skipped).find((item) => item.target === 'glow');
  if (stillSkipped !== undefined) {
    problems.push('brightness() 已经精确对应了，glow 不该再被跳过：' + JSON.stringify(stillSkipped));
  }
  // ⑨ **整数 `iterationStart`**（第 50/51 轮）：相位是整数时精确可表达 ——
  //    层起点右移 `iterationStart × span`；**奇偶性跟着变**（alternate 下第 1 遍是反向的）；
  //    层长按 `iterations × span + 1`。这条能力以前在、但**没有判据**，现在钉住。
  {
    const drift = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'drift');
    if (drift === undefined) {
      problems.push('样本里应当有 drift 层（整数 iterationStart）');
    } else {
      const keys = drift.keyframes
        .filter((key) => key.target === 'opacity')
        .sort((a, b) => a.frame - b.frame)
        .map((key) => ({ frame: key.frame, value: key.value, easing: key.easing }));
      const want = [
        { frame: 0, value: 1, easing: 'linear' },
        { frame: 12, value: 0, easing: 'linear' },
        { frame: 24, value: 1, easing: 'linear' },
      ];
      if (JSON.stringify(keys) !== JSON.stringify(want)) {
        problems.push('drift 的键与既定答案不符（第 1 遍必须**反向**）：' + JSON.stringify(keys));
      }
      if (drift.start !== 12 || drift.end !== 37) {
        problems.push(
          'drift 的区间应当是 [12, 37)（起点右移 1×span、层长 2×span+1），得到 [' +
            drift.start + ', ' + drift.end + ']',
        );
      }
    }
  }


  return problems;
}

/** 跑一次转译器，拿回 doc 与报告。 */
function transcribe(label, extraArgs = []) {
  mkdirSync(OUT_DIR, { recursive: true });
  const docPath = join(OUT_DIR, label + '.doc.json');
  const reportPath = join(OUT_DIR, label + '.report.json');
  const run = runNode(['scripts/waapi2doc.mjs', FIXTURE, docPath, reportPath, ...extraArgs], label);
  const doc = existsSync(docPath) ? JSON.parse(readFileSync(docPath, 'utf8')) : null;
  const report = existsSync(reportPath) ? JSON.parse(readFileSync(reportPath, 'utf8')) : null;
  return { run, doc, report, docPath, reportPath };
}

/**
 * **跨文件比对**：「引擎实现了哪几条混合模式」这件事现在有**三处**提到 ——
 * `layer.rs` 的 `matches!`、`web/dom-css.mjs` 的常量、以及本转译器的那份。三处必须一致。
 *
 * 为什么值得一条判据：引擎哪天补上一条，别处那句「引擎还没实现」就变成**假消息** ——
 * 假消息比不报更坏（会让人为一条不缺的能力去改工程）。
 *
 * 三份都从**源码文本**里抠（不 import waapi2doc.mjs：那个文件末尾会真的跑一遍转译）。
 * 抠不出来时判红 —— 解析坏了不许当通过。
 */
export function checkEngineBlendCopies(readLayerRs, readTranspiler, readDomCss) {
  const problems = [];
  const normalize = (names) => [...new Set(names)].sort();
  const source = readLayerRs();
  const at = source.indexOf('fn is_implemented');
  if (at < 0) {
    problems.push('layer.rs 里找不到 fn is_implemented —— 解析坏了，不许当通过');
    return problems;
  }
  const match = source.slice(at).match(/matches!\s*\(\s*self\s*,\s*([^)]*)\)/);
  if (match === null) {
    problems.push('is_implemented 里没找到 matches! —— 解析坏了，不许当通过');
    return problems;
  }
  const rust = normalize([...match[1].matchAll(/Self::([A-Za-z]+)/g)].map((item) =>
    item[1].replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase()));
  const arrayOf = (text, pattern, label) => {
    const found = text.match(pattern);
    if (found === null) {
      problems.push('从 ' + label + ' 里抠不出那份混合模式清单 —— 解析坏了，不许当通过');
      return null;
    }
    return normalize([...found[1].matchAll(/'([^']+)'/g)].map((item) => item[1]));
  };
  const here = arrayOf(readTranspiler(), /ENGINE_IMPLEMENTED_BLENDS\s*=\s*\[([^\]]*)\]/, 'scripts/waapi2doc.mjs');
  if (here !== null && JSON.stringify(here) !== JSON.stringify(rust)) {
    problems.push('转译器那份与 layer.rs 对不上：源码 ' + JSON.stringify(rust) + '，转译器 ' + JSON.stringify(here));
  }
  const dom = arrayOf(readDomCss(), /ENGINE_IMPLEMENTED_BLENDS\s*=\s*\[([^\]]*)\]/, 'web/dom-css.mjs');
  if (dom !== null && JSON.stringify(dom) !== JSON.stringify(rust)) {
    problems.push('web/dom-css.mjs 那份与 layer.rs 对不上：DOM ' + JSON.stringify(dom) + '，源码 ' + JSON.stringify(rust));
  }
  return problems;
}

function main() {
  const selfTest = process.argv.includes('--self-test');
  const first = transcribe('run1');
  if (first.run.status !== 0) {
    console.error('✗ 转译器没有正常退出：status=' + String(first.run.status) + ' error=' + String(first.run.error));
    console.error(first.run.stderr.slice(-800));
    process.exit(1);
  }
  if (first.doc === null || first.report === null) {
    console.error('✗ 转译器没产出 doc 或报告');
    process.exit(1);
  }
  const problems = judgeContract(first.doc, first.report);

  // 跨文件：「引擎实现了哪几条混合模式」三处必须一致。
  const readText = (rel) => readFileSync(new URL(rel, import.meta.url), 'utf8');
  problems.push(
    ...checkEngineBlendCopies(
      () => readText('../crates/dhampir-timeline/src/layer.rs'),
      () => readText('../scripts/waapi2doc.mjs'),
      () => readText('../web/dom-css.mjs'),
    ),
  );

  // 确定性：同样输入跑第二遍，两个 doc 必须逐字节相同。
  const second = transcribe('run2');
  if (second.run.status !== 0 || readFileSync(first.docPath, 'utf8') !== readFileSync(second.docPath, 'utf8')) {
    problems.push('同一份输入跑两遍，产出的 doc 不是逐字节相同（转译必须是确定的）');
  }

  // --strict：样本里有表达不了的那条 ⇒ 必须退 2（而不是假装成功）。
  const strict = transcribe('strict', ['--strict']);
  if (strict.run.status !== 2) {
    problems.push('带 --strict 跑样本应当退 2（它有表达不了的动画），得到 ' + String(strict.run.status));
  }

  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ waapi2doc 的判据没过');
    process.exit(1);
  }
  console.log('✓ waapi2doc：结构、off-by-one 的缓动、量化留痕、明确跳过、确定性与 --strict 都对');

  if (selfTest) {
    const mutations = [
    // 第 51 轮：整数相位的**起点右移**是这条能力的核心，钉住它。
    ['整数相位的起点没右移', (doc) => {
      const drift = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'drift');
      drift.start = 0;
    }],
      ['schema 不是 4', (doc) => { doc.timeline.schema = 3; }],
      ['层区间不是正长度', (doc) => { doc.timeline.tracks[0].layers[0].end = doc.timeline.tracks[0].layers[0].start; }],
      ['关键帧 target 不在五个通道里', (doc) => { doc.timeline.tracks[0].layers[0].keyframes[0].target = 'opacity_x'; }],
      ['关键帧落在层外', (doc) => { const l = doc.timeline.tracks[0].layers[0]; l.keyframes[0].frame = l.end - l.start; }],
      ['off-by-one 被破坏（缓动挪到了起点键）', (doc) => {
        const keys = doc.timeline.tracks[0].layers[0].keyframes
          .filter((key) => key.target === 'opacity')
          .sort((a, b) => a.frame - b.frame);
        keys[0].easing = 'cubic-bezier(0.2, 0.8, 0.4, 1)';
        keys[1].easing = 'linear';
      }],
      ['层 id 重复', (doc) => { doc.timeline.tracks[1].layers[0].id = doc.timeline.tracks[0].layers[0].id; }],
      ['量化记录丢了 error', (doc, report) => { report.quantization[0].error = undefined; }],
      ['跳过项少了 sparkle', (doc, report) => { report.skipped = []; }],
      ['镜像后的缓动被抹平成线性', (doc) => {
        const badge = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'badge');
        for (const key of badge.keyframes) if (key.target === 'opacity') key.easing = 'linear';
      }],
      ['层长没按遍数算', (doc) => {
        const badge = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'badge');
        badge.end = 13;
      }],
      ['模糊忘了 σ = radius/2（少乘了 2）', (doc) => {
        const card = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'card');
        const blur = card.effects.find((effect) => effect.kind === 'gaussian_blur');
        blur.params.radius = 4;
      }],
      ['圆角没写进层里', (doc) => {
        const card = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'card');
        delete card.corner_radius;
      }],
      // 第 45 轮：两条"引擎缺口"警告都关掉之后，"清空全部警告"这个变异**再也红不了**
      // （判据从"必须有警告"翻成了"不许有警告"）—— 所以换成仍被盯的那一种：
      // 凭空塞一条**已经解决了的**缺口警告，判据必须红。
      ['已经解决的缺口又被报出来', (doc, report) => {
        report.warnings = (report.warnings === undefined ? [] : report.warnings).concat([
          { target: 'card', reason: 'filter.saturate 与规范权重差极小（近似）' },
        ]);
      }],
      ['线性断点表没镜像（反向那遍照抄了原式）', (doc) => {
        const wave = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'wave');
        const key = wave.keyframes.find((item) => item.target === 'opacity' && item.frame === 24);
        key.easing = 'linear(0, 0.25 75%, 1)';
      }],
      ['圆心比例没写进去', (doc) => {
        const card = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'card');
        delete card.clip.center;
      }],
      ['内缩简写展开错了（左右当成上下）', (doc) => {
        const band = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'band');
        band.clip.bottom = 20;
      }],
      ['参数通道的第二个值没乘 2', (doc) => {
        const pulse = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'pulse');
        const key = pulse.keyframes.find((item) => item.target === 'effect.0.radius' && item.frame === 12);
        key.value = 12;
      }],
      ['参数通道指向不存在的特效下标', (doc) => {
        const pulse = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'pulse');
        for (const key of pulse.keyframes) if (key.target === 'effect.0.radius') key.target = 'effect.3.radius';
      }],
      ['参数通道指向该特效没有的参数', (doc) => {
        const pulse = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'pulse');
        for (const key of pulse.keyframes) if (key.target === 'effect.0.radius') key.target = 'effect.0.nope';
      }],
      ['参数通道被整个丢掉', (doc) => {
        const pulse = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'pulse');
        pulse.keyframes = pulse.keyframes.filter((item) => item.target !== 'effect.0.radius');
      }],
      ['裁剪形状被整个丢掉', (doc) => {
        const band = doc.timeline.tracks.flatMap((track) => track.layers).find((layer) => layer.id === 'band');
        delete band.clip;
      }],
    ];
    let caught = 0;
    for (const [name, mutate] of mutations) {
      const doc = JSON.parse(JSON.stringify(first.doc));
      const report = JSON.parse(JSON.stringify(first.report));
      mutate(doc, report);
      const found = judgeContract(doc, report);
      if (found.length === 0) {
        console.error('  - 自检失败：判据对「' + name + '」视而不见（守卫不会红的判据不是判据）');
        process.exit(1);
      }
      caught += 1;
    }
    // 跨文件那一组：把三处里的某一处改掉，必须红。
    const drift = checkEngineBlendCopies(
      () => readText('../crates/dhampir-timeline/src/layer.rs'),
      () => "export const ENGINE_IMPLEMENTED_BLENDS = ['normal', 'multiply'];\n",
      () => readText('../web/dom-css.mjs'),
    );
    if (drift.length === 0) {
      console.error('  - 自检失败：三处混合模式清单不一致时判据视而不见');
      process.exit(1);
    }
    caught += 1;
    console.log('✓ 自检：' + caught + ' 个变异全部被抓住（守卫会红）');
  }
}

main();
