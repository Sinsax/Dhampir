#!/usr/bin/env node
// 双端一致性：**一条命令**。
//
// # 为什么需要它
//
// "两端渲出来一样"是这个项目最重要的不变量，而它此前**没有守卫** ——
// 要手动跑三个脚本（浏览器导出、worker 渲染、比对），顺序与参数靠人记。
// 守卫应当是快的不假，但**最重要的不变量不能只靠"记得跑"**。
// 所以它是一条命令，不进默认守卫集（那个要求秒级），而是改渲染后必跑。
//
// # 它只做编排
//
// 三块零件都是现成的：
//   1. scripts/web-check.mjs      -> 浏览器逐帧导出 PNG
//   2. examples/render_project    -> worker 渲染同一份工程
//   3. examples/compare_project   -> 比对两目录（SSIM/PSNR/MAE）
// **判定交给 compare_project**，这里不重算指标 —— 重算就又多一份实现。
//
// 用法：
//   node scripts/check-dual-end.mjs
//   node scripts/check-dual-end.mjs --frames 0,30,89
//   node scripts/check-dual-end.mjs --self-test

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 两端各吃什么输入。**这是比对能不能成立的前提。**
 *
 * M4 得到过 SSIM=1.000000，那必然是在**两边输入一致**的路径上得到的 ——
 * 但那条路径**没有被记住**。于是后来的人（我）拿默认参数一跑，
 * 比的是「浏览器经 <video> 解码的真视频」与「worker 的合成源」，
 * **两个不同的东西**，而那个 SSIM 就成了一句没有意义的话。
 *
 * 所以本编排器**默认拒绝比对**，除非调用方显式声明输入一致（--inputs-identical）。
 * 等真有一条让两端吃同一份像素的路径时，再用那个开关。 */
export const INPUT_SOURCES = {
  browser: '<video> 解码 proxy.mp4（真实视频帧）',
  worker: 'core 的 synthetic_source_rgba8（合成源，不解码）',
};
const BROWSER_DIR = join(REPO_ROOT, 'target', 'export', 'frames');
const NATIVE_DIR = join(REPO_ROOT, 'target', 'native-frames');

/** 浏览器与 worker 导出的帧文件名。**两边都得是这个形状**，否则比对无从对齐。 */
export function framePath(dir, frame) {
  return join(dir, 'frame-' + String(frame).padStart(4, '0') + '.png');
}

/** 读 PNG 的宽高（IHDR 固定在偏移 16..24）。
 *
 * **尺寸不同就不是同一个东西** —— 那种「不一致」与「画面不一样」是两件事，必须分开报。
 * 早先 compare_project 只给了一句「尺寸不一致或数据不足」，看不到两边各是多少。 */
export function pngSize(file) {
  if (!existsSync(file)) return null;
  const head = readFileSync(file).subarray(0, 24);
  if (head.length < 24) return null;
  return { width: head.readUInt32BE(16), height: head.readUInt32BE(20) };
}

/** 跑一条命令并把输出原样带回来。**不吞输出**：失败时它就是现场。 */
export function run(command, args) {
  const result = spawnSync(command, args, { cwd: REPO_ROOT, encoding: 'utf8', env: process.env });
  return { status: result.status, stdout: result.stdout || '', stderr: result.stderr || '' };
}

/** 判定：只要求**请求的那几帧**两边都在、且尺寸相同。
 *
 * 早先的版本要求两边总帧数相等 —— 那是错的：浏览器导出整段、worker 只渲指定帧，
 * 本来就不该相等。**把「工具参数不同」误判成「两端不一致」**，正是这类编排最容易犯的错。 */
export function judge(entries, compareStatus) {
  const problems = [];
  if (entries.length === 0) problems.push('一帧都没比 —— 那是「没跑」而不是「一致」');
  for (const entry of entries) {
    if (!entry.left) { problems.push(entry.frame + ' 帧：浏览器侧没有'); continue; }
    if (!entry.right) { problems.push(entry.frame + ' 帧：worker 侧没有'); continue; }
    if (entry.left.width !== entry.right.width || entry.left.height !== entry.right.height) {
      problems.push(
        entry.frame + ' 帧尺寸不同：浏览器 ' + entry.left.width + 'x' + entry.left.height +
        '，worker ' + entry.right.width + 'x' + entry.right.height +
        ' —— 尺寸不同就**不可比**，这不是「画面不一样」'
      );
    }
  }
  if (compareStatus !== 0) problems.push('compare_project 退出码 ' + compareStatus + '，它判了不一致');
  return problems;
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, problems, shouldBeEmpty) => {
    if ((problems.length === 0) !== shouldBeEmpty) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };
  const both = [{ frame: 0, left: { width: 640, height: 360 }, right: { width: 640, height: 360 } }];
  expect('尺寸相同且比对通过 -> 通过', judge(both, 0), true);
  expect('一帧都没比 -> 必须红（不能把「没跑」当「一致」）', judge([], 0), false);
  expect('一边缺帧 -> 必须红', judge([{ frame: 0, left: null, right: both[0].right }], 0), false);
  expect(
    '尺寸不同 -> 必须红，且要报出两边各是多少',
    judge([{ frame: 0, left: { width: 640, height: 360 }, right: { width: 320, height: 180 } }], 0),
    false
  );
  expect('比对判不一致 -> 必须红', judge(both, 1), false);
  console.log('✓ 双端比对编排的自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }

  const argv = process.argv.slice(2);
  const framesIndex = argv.indexOf('--frames');
  const frames = framesIndex >= 0 ? argv[framesIndex + 1].split(',') : ['0', '30', '60', '89'];

  console.log('抽取 ' + frames.length + ' 帧做双端比对：' + frames.join(', '));

  // **先确认这次比对的前提成立**：两端得吃同一份输入。
  // 默认它们不是 —— 所以默认拒绝，并说清楚各自吃什么。
  if (!argv.includes('--inputs-identical')) {
    console.log('');
    console.log('拒绝比对：两端消费的输入不同。');
    console.log('  浏览器侧：' + INPUT_SOURCES.browser);
    console.log('  worker 侧：' + INPUT_SOURCES.worker);
    console.log('');
    console.log('这个前提下给出的 SSIM **没有意义** —— 它量的是「输入不同」，');
    console.log('不是「渲染不一致」。要让它有意义，先做出一条两端吃同一份像素的路径，');
    console.log('再用 --inputs-identical 显式声明。');
    process.exitCode = 1;
    return;
  }


  // 浏览器侧：**空目录不能算通过**，所以先清掉旧帧。
  rmSync(BROWSER_DIR, { recursive: true, force: true });
  rmSync(NATIVE_DIR, { recursive: true, force: true });
  mkdirSync(NATIVE_DIR, { recursive: true });

  console.log('\n[1/3] 浏览器逐帧导出 …');
  const browser = // **--frames-only：验收工具不该改动交付物** —— 不加这个参数，
  // 跑一次比对就会把 milestones/edited-milestone.mp4 覆盖掉。
  run(process.execPath, ['scripts/web-check.mjs', '--frames-only', '--canvas', '320x180']);
  if (browser.status !== 0 || !browser.stdout.includes('帧已就绪')) {
    console.error('浏览器侧导出没成功（退出码 ' + browser.status + '）');
    console.error((browser.stdout + browser.stderr).slice(-2000));
    process.exitCode = 1;
    return;
  }

  console.log('[2/3] worker 渲染同一份工程 …');
  const render = run('cargo', [
    'run', '--quiet', '--example', 'render_project', '--',
    'fixtures/sample-project.json', NATIVE_DIR, ...frames,
  ]);
  if (render.status !== 0) {
    console.error('worker 渲染失败（退出码 ' + render.status + '）');
    console.error((render.stdout + render.stderr).slice(-2000));
    process.exitCode = 1;
    return;
  }

  // **先自己比尺寸**，再交给 compare_project —— 它只说「不一致」，
  // 而「尺寸不同」与「画面不同」需要分开报，否则看不出去查哪里。
  const entries = frames.map((frame) => ({
    frame: frame,
    left: pngSize(join(BROWSER_DIR, 'frame-' + String(frame).padStart(4, '0') + '.png')),
    right: pngSize(join(NATIVE_DIR, 'frame-' + String(frame).padStart(4, '0') + '.png')),
  }));

  console.log('[3/3] 比对 …');
  const compare = run('cargo', [
    'run', '--quiet', '--example', 'compare_project', '--',
    BROWSER_DIR, NATIVE_DIR, ...frames,
  ]);
  console.log(compare.stdout.trim());
  if (compare.stderr.trim()) console.error(compare.stderr.trim().slice(-800));

  const problems = judge(entries, compare.status);
  // 通过线的出处见文件顶部注释；换算只认「完全一致」这一档。
  // 通过线的出处见文件顶部注释；换算只认「完全一致」这一档。
  //
  // **上一轮这里只有用法、没有定义** —— 于是脚本一跑到这里就 ReferenceError，
  // 而「崩溃」在外面看起来与「没输出」一样。修的时候顺带补上定义。
  const REACHED = /最差 SSIM = inf/;
  if (!REACHED.test(compare.stdout)) {
    problems.push("SSIM 没有达到 1.000000（通过线出处：M4 记录）");
    problems.push("若两边输入本就不同源（浏览器解码真视频、worker 用合成源），这个数没有意义 —— 先对齐输入，再谈一致性");
  }
  if (problems.length > 0) {
    for (const problem of problems) console.log('  - ' + problem);
    console.log('双端一致性未通过');
    process.exitCode = 1;
    return;
  }
  console.log('\n✓ 双端一致性通过（编排：浏览器导出 -> worker 渲染 -> compare_project 判定）');
}

main();
