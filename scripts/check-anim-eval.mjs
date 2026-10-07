#!/usr/bin/env node
// 网页动画的第二个宿主（DOM/CSS）与 core 的**逐帧数值对照**。
//
//   node scripts/check-anim-eval.mjs [project.doc.json] [from] [to]
//   node scripts/check-anim-eval.mjs --self-test
//
// # 它为什么存在
//
// `web/anim-eval.mjs` 是**第二份求值实现**（网页那边要脱离 wasm 也能跑）。
// 本仓的规矩是"同一份逻辑只有一份"，允许第二份的唯一前提就是：**它漂了会被抓住**。
// 这条判据就是那只手 —— 拿 core（@cargo run --example eval_channels@）算出来的
// 逐帧逐通道值，与 JS 那份逐个比。
//
// # 容差为什么不是零
//
// core 是 **f32** 算术，JS 这边是 **f64**：同一套公式、同样的迭代次数，
// 结果仍在最后几位上不同。所以容差是**量出来的**（见 plan/waapi-stage3-evidence.md），
// 不是拍出来的。要更紧就把 JS 那份的每一步用 `Math.fround` 夹一遍 —— 那是另一件事。
//
// # 捕获子进程输出为什么用文件而不是管道
//
// 本机 agent 会话里连 stdout 管道都起不来（spawnSync EPERM）。文件重定向不是垫片：
// 起不来时 status 是 null，判据照红（fail-closed）。

import { spawnSync } from 'node:child_process';
import { closeSync, existsSync, mkdirSync, openSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { evaluateFrame, parseEasing, easeValue, channelFrom } from '../web/anim-eval.mjs';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const LOG_DIR = join(REPO_ROOT, 'target', 'agent-logs');
const CHANNELS = ['opacity', 'x', 'y', 'scale', 'rotation'];

/**
 * 逐帧逐通道的容差。
 *
 * **依据是量出来的，不是拍的**（plan/waapi-stage3-evidence.md）：
 * 样本工程上实测最大偏差 **9.37e-6**（title.x@20）—— 那是 f32（core）与 f64（网页）
 * 的必然差，公式与迭代次数两边是一致的。这里取 **1e-4**，比实测大一个数量级。
 *
 * **这个数还够不够用，由下面那条区分度判据回答**：把所有缓动换成 linear 之后
 * 对照必须变红。真正的漂移（换错缓动、丢键、规则错）会带来 1e-3 以上的偏差，
 * 所以 1e-4 有区分度。区分度没了就说明容差太松，得往回收。
 */
export const TOLERANCE = 1e-4;

/** 对照本体。纯函数，便于喂**故意错的**输入验证它真的会红。 */
export function compareFrames(expected, actual, tolerance = TOLERANCE) {
  const problems = [];
  let worst = { delta: 0, what: '(还没比)' };
  if (expected.length !== actual.length) {
    problems.push('帧数不同：core ' + expected.length + ' 帧，网页 ' + actual.length + ' 帧');
  }
  const frames = Math.min(expected.length, actual.length);
  for (let i = 0; i < frames; i += 1) {
    const want = expected[i];
    const got = actual[i];
    if (want.frame !== got.frame) {
      problems.push('第 ' + i + ' 行的帧号不同：core ' + want.frame + '，网页 ' + got.frame);
      continue;
    }
    if (want.layers.length !== got.layers.length) {
      problems.push('帧 ' + want.frame + ' 的层数不同：core ' + want.layers.length + '，网页 ' + got.layers.length);
      continue;
    }
    for (let j = 0; j < want.layers.length; j += 1) {
      const a = want.layers[j];
      const b = got.layers[j];
      if (a.id !== b.id) {
        problems.push('帧 ' + want.frame + ' 第 ' + j + ' 层的 id 不同：core ' + a.id + '，网页 ' + b.id);
        continue;
      }
      for (const channel of CHANNELS) {
        const delta = Math.abs(a[channel] - b[channel]);
        if (delta > worst.delta) worst = { delta, what: a.id + '.' + channel + '@' + want.frame };
        if (!(delta <= tolerance)) {
          problems.push(
            '帧 ' + want.frame + ' 层 ' + a.id + ' 的 ' + channel + ' 超容差：core ' + a[channel] +
              '，网页 ' + b[channel] + '，差 ' + delta,
          );
        }
      }
    }
  }
  return { problems, worst };
}

/** 起一次 dumper，拿回 NDJSON（输出重定向到文件）。 */
function coreFrames(docPath, from, to) {
  mkdirSync(LOG_DIR, { recursive: true });
  const outPath = join(LOG_DIR, 'anim-eval-core.ndjson');
  const errPath = join(LOG_DIR, 'anim-eval-core.err');
  const outFd = openSync(outPath, 'w');
  const errFd = openSync(errPath, 'w');
  const result = spawnSync(
    'cargo',
    ['run', '-q', '-p', 'dhampir-worker', '--example', 'eval_channels', '--', docPath, String(from), String(to)],
    { cwd: REPO_ROOT, windowsHide: true, stdio: ['ignore', outFd, errFd] },
  );
  closeSync(outFd);
  closeSync(errFd);
  const stdout = existsSync(outPath) ? readFileSync(outPath, 'utf8') : '';
  const stderr = existsSync(errPath) ? readFileSync(errPath, 'utf8') : '';
  return { status: result.status, error: result.error === undefined ? null : result.error.message, stdout, stderr };
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    return selfTest();
  }
  const files = argv.filter((arg) => !arg.startsWith('--'));
  const docPath = resolve(REPO_ROOT, files[0] === undefined ? 'target/waapi-demo/out.doc.json' : files[0]);
  const from = files[1] === undefined ? 0 : Number(files[1]);
  const to = files[2] === undefined ? 30 : Number(files[2]);

  if (!existsSync(docPath)) {
    console.error('✗ 找不到工程文件：' + docPath);
    console.error('  先跑：node scripts/waapi2doc.mjs fixtures/waapi-snapshot.sample.json ' + docPath);
    process.exit(1);
  }

  const doc = JSON.parse(readFileSync(docPath, 'utf8'));
  const timeline = doc.timeline === undefined ? doc : doc.timeline;

  // 前置：**样本里不许有转场**。转场会给 opacity 叠一层权重，而网页那份还没实现它 ——
  // 混在一起就分不清"没实现"与"算错了"。这条前置是 fail-closed 的。
  const problems = [];
  for (const track of timeline.tracks === undefined ? [] : timeline.tracks) {
    for (const layer of track.layers === undefined ? [] : track.layers) {
      if (layer.transition_in !== undefined && layer.transition_in !== null) {
        problems.push('层 ' + layer.id + ' 带 transition_in：网页那份还没实现转场权重，这条对照会失去意义');
      }
    }
  }
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 前置不成立');
    process.exit(1);
  }

  const core = coreFrames(docPath, from, to);
  if (core.status !== 0) {
    console.error('✗ dumper 没跑起来：status=' + String(core.status) + ' error=' + String(core.error));
    console.error(core.stderr.slice(-800));
    process.exit(1);
  }
  const expected = core.stdout
    .split(/\r?\n/)
    .filter((line) => line.startsWith('{'))
    .map((line) => JSON.parse(line));
  if (expected.length === 0) {
    console.error('✗ dumper 一行都没出 —— 空文件集不算通过');
    process.exit(1);
  }

  const actual = [];
  for (let frame = from; frame <= to; frame += 1) {
    actual.push({ frame, layers: evaluateFrame(timeline, frame) });
  }

  const { problems: found, worst } = compareFrames(expected, actual);
  console.log('对照 ' + expected.length + ' 帧 × 最多 ' + CHANNELS.length + ' 个通道，最大偏差 ' + worst.delta +
    '（' + worst.what + '），容差 ' + TOLERANCE);
  if (found.length > 0) {
    for (const problem of found.slice(0, 12)) console.error('  - ' + problem);
    console.error('✗ 网页那份求值与 core 对不上（第二实现漂了）');
    process.exit(1);
  }
  console.log('✓ 网页求值与 core 逐帧逐通道一致（含缓动、关键帧插值、步进与过冲）');

  // ---- 区分度：容差再松，也必须抓得住**真正的漂移** ----
  //
  // 把样本里所有缓动换成 linear（最典型的一种漂），再比一遍。
  // 它要是还绿，说明容差已经松到没有意义 —— 那比"太紧"更坏。
  const mutatedTimeline = JSON.parse(JSON.stringify(timeline));
  for (const track of mutatedTimeline.tracks === undefined ? [] : mutatedTimeline.tracks) {
    for (const layer of track.layers === undefined ? [] : track.layers) {
      for (const key of layer.keyframes === undefined ? [] : layer.keyframes) key.easing = 'linear';
    }
  }
  const drifted = [];
  for (let frame = from; frame <= to; frame += 1) {
    drifted.push({ frame, layers: evaluateFrame(mutatedTimeline, frame) });
  }
  const discrimination = compareFrames(expected, drifted);
  if (discrimination.problems.length === 0) {
    console.error('✗ 区分度不足：把所有缓动换成 linear 之后对照居然还是绿的 —— 容差太松，等于没有判据');
    process.exit(1);
  }
  console.log('✓ 区分度：把全部缓动换成 linear 会被抓住（' + discrimination.problems.length + ' 条，最大偏差 ' +
    discrimination.worst.delta + '，是实测偏差的 ' + (discrimination.worst.delta / worst.delta).toFixed(0) + ' 倍）');
}

/** 反向验证：每条判据都得会红。 */
function selfTest() {
  const good = [
    { frame: 0, layers: [{ id: 'a', opacity: 0, x: -80, y: 12, scale: 0.9, rotation: 0 }] },
    { frame: 1, layers: [{ id: 'a', opacity: 0.5, x: 0, y: 0, scale: 1, rotation: 0 }] },
  ];
  const clone = () => JSON.parse(JSON.stringify(good));
  const mutations = [
    ['帧数不同', () => { const a = clone(); a.pop(); return a; }],
    ['帧号不同', () => { const a = clone(); a[1].frame = 7; return a; }],
    ['层数不同', () => { const a = clone(); a[0].layers.push({ id: 'b' }); return a; }],
    ['层 id 不同', () => { const a = clone(); a[0].layers[0].id = 'c'; return a; }],
    ['通道超容差', () => { const a = clone(); a[0].layers[0].opacity = 1e-3; return a; }],
    ['通道在容差之内（不许红）', () => { const a = clone(); a[0].layers[0].opacity = TOLERANCE / 2; return a; }],
  ];
  let caught = 0;
  for (const [name, mutate] of mutations) {
    const { problems } = compareFrames(good, mutate());
    const shouldRed = !name.includes('不许红');
    if (shouldRed && problems.length === 0) {
      console.error('  - 自检失败：判据对「' + name + '」视而不见');
      process.exit(1);
    }
    if (!shouldRed && problems.length > 0) {
      console.error('  - 自检失败：容差之内的偏差不该红（' + name + '）');
      process.exit(1);
    }
    caught += 1;
  }
  // 缓动那几条也得会拒绝：认不出的串必须抛，而不是当线性算。
  for (const bad of ['bounce', 'linear(0, 1)', 'cubic-bezier(2,0,0,1)', 'steps(0)']) {
    let threw = false;
    try { parseEasing(bad); } catch { threw = true; }
    if (!threw) {
      console.error('  - 自检失败：认不出的缓动没有抛错：' + bad);
      process.exit(1);
    }
    caught += 1;
  }
  // 端点归位 + 过冲保留 + 下划线/连字符是两条曲线
  if (Math.abs(easeValue(parseEasing('linear'), 0.5) - 0.5) > 1e-9) {
    console.error('  - 自检失败：linear 在 0.5 处不对');
    process.exit(1);
  }
  const back = parseEasing('back_out');
  const peak = Math.max(...Array.from({ length: 101 }, (_, i) => easeValue(back, i / 100)));
  if (!(peak > 1)) {
    console.error('  - 自检失败：back_out 应当冲过 1，峰值只有 ' + peak);
    process.exit(1);
  }
  const underscore = easeValue(parseEasing('ease_in'), 0.5);
  const hyphen = easeValue(parseEasing('ease-in'), 0.5);
  if (Math.abs(underscore - hyphen) < 1e-3) {
    console.error('  - 自检失败：ease_in 与 ease-in 被当成同一条曲线');
    process.exit(1);
  }
  // 通道求值：后一个键的缓动在起作用（换成线性就会不同）
  const keys = [
    { frame: 0, target: 'opacity', value: 0, easing: 'linear' },
    { frame: 10, target: 'opacity', value: 1, easing: 'ease_in' },
  ];
  const eased = channelFrom(0, keys, 'opacity', 5);
  if (Math.abs(eased - 0.25) > 1e-9) {
    console.error('  - 自检失败：中点应当用后一个键的 ease_in（t²）算出 0.25，得到 ' + eased);
    process.exit(1);
  }
  console.log('✓ 自检：' + caught + ' 个变异被抓住，缓动与通道的边界也钉住了');
}

main();
