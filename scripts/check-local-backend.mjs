#!/usr/bin/env node
// 本机后端的端到端检查。
//
// # 它和 web-check --local 的分工
//
// 这一份**不进浏览器**：它只验后端的 HTTP 层与出片链路（路由、Range、任务状态机、
// 真的出一个 mp4 并用 ffprobe 核对形状）。浏览器那一半归 web-check.mjs。
// 分开的理由很实际：后端挂了和页面挂了表现完全不同，而混在一个检查里
// 只能得到一句"失败了"。
//
// # 为什么每条判据都要留名字
//
// 检查项写在 EXPECTED 名单里，judge() 要求**名字一条不差**：
// 少了任何一条（比如某段代码提前 return 跳过了）都会红。
// 静默跳过一条判据和那条判据通过，在外面看起来一样 —— 这正是要防的。
//
// # 用法
//
//   node scripts/check-local-backend.mjs
//   node scripts/check-local-backend.mjs --self-test
//   node scripts/check-local-backend.mjs [--port 8799 --cli target/debug/dhampir]

import { spawn } from 'node:child_process';
import { runToolSync } from './spawn-tool.mjs';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tryRemove } from './safe-remove.mjs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 每一条判据的名字。**改这里就必须改采集端**，judge 会把差异指出来。 */
export const EXPECTED = [
  'backend-self-test',
  'health',
  'capabilities',
  'project-is-doc',
  'project-assets',
  'assets-info',
  'assets-gop',
  'media-range',
  'media-missing',
  'library',
  'assets-guard',
  'validate-clean',
  'validate-broken',
  'frame-png',
  'frame-rejects-fraction',
  'export-accepted',
  'export-succeeded',
  'export-progress-unknown-or-one',
  'download-ok',
  'download-frames',
  'download-shape',
  'download-seconds',
  'cancel',
  'cancel-twice',
  'missing-job',
];

/**
 * 判定。抽成纯函数是为了能喂**故意坏的**观察进来验证它真的会红。
 *
 * 三条纪律（与仓库其他守卫一致）：
 *   1. **拒绝在空集上通过**；
 *   2. 名单一条不差 —— 少一条、多一条都红；
 *   3. 不放过任何 ok=false。
 */
export function judge(observed) {
  const problems = [];
  if (!Array.isArray(observed) || observed.length === 0) {
    problems.push('一条观察都没有 —— 守卫拒绝在空集上通过（采集段提前退出了吗？）');
    return problems;
  }
  const seen = new Map();
  for (const row of observed) {
    if (seen.has(row.name)) problems.push('判据重复：' + row.name);
    seen.set(row.name, row);
  }
  for (const name of EXPECTED) {
    if (!seen.has(name)) problems.push('判据缺失：' + name + '（这段代码被跳过了？）');
  }
  for (const name of seen.keys()) {
    if (!EXPECTED.includes(name)) problems.push('名单里没有这条判据：' + name);
  }
  for (const row of observed) {
    if (!row.ok) {
      problems.push(row.name + (row.detail === undefined || row.detail === '' ? '' : '：' + row.detail));
    }
  }
  return problems;
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, condition) => {
    if (!condition) throw new Error('自检失败：' + name);
    passed += 1;
  };
  const good = EXPECTED.map((name) => ({ name: name, ok: true, detail: '' }));

  expect('完整的观察通过', judge(good).length === 0);
  expect('空集必须红', judge([]).length > 0);
  expect('undefined 必须红', judge(undefined).length > 0);

  const oneFalse = good.map((row) => (row.name === 'assets-info' ? { ...row, ok: false, detail: 'x' } : row));
  const falseProblems = judge(oneFalse);
  expect('一条 false 必须红', falseProblems.length === 1);
  expect('而且要点名是哪一条', falseProblems[0].startsWith('assets-info'));

  const missing = good.filter((row) => row.name !== 'download-ok');
  const missingProblems = judge(missing);
  expect('少一条必须红', missingProblems.length === 1);
  expect('而且要说是缺失', missingProblems[0].includes('判据缺失'));

  const extra = good.concat([{ name: 'smuggled', ok: true, detail: '' }]);
  expect('多一条必须红', judge(extra).some((problem) => problem.includes('smuggled')));

  const duplicated = good.concat([{ name: EXPECTED[0], ok: true, detail: '' }]);
  expect('重复必须红', judge(duplicated).some((problem) => problem.includes('判据重复')));

  console.log('✓ 本机后端检查自检通过（' + passed + ' 条断言）');
}

async function collect(port) {
  const base = 'http://127.0.0.1:' + port;
  const observed = [];
  const record = (name, ok, detail) => {
    observed.push({ name: name, ok: !!ok, detail: detail === undefined ? '' : String(detail) });
  };

  // 后端模块**自己的自检**先跑。它盯的是纯函数（资产索引的绝对路径判定、NDJSON 归约、
  // 任务状态机形状），端到端这条路走不到那些分支。
  // 起因是它在 Linux 上红过一次（「C:/abs/c.mp4」被当成相对路径挂到 asset_root 下面），
  // 而当时**没有任何守卫会跑它** —— 自检红了也没人知道。这条把它接进来。
  const moduleSelfTest = runToolSync(process.execPath, ['scripts/dhampir-local.mjs', '--self-test'], {
    cwd: REPO_ROOT,
  });
  const moduleLog = (moduleSelfTest.stdout || '') + (moduleSelfTest.stderr || '');
  record('backend-self-test', moduleSelfTest.status === 0,
    moduleSelfTest.status === 0 ? '' : moduleLog.trim().slice(-300));

  const child = spawn(process.execPath, ['scripts/dhampir-local.mjs', '--port', String(port)], {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let log = '';
  child.stdout.on('data', (chunk) => { log += chunk; });
  child.stderr.on('data', (chunk) => { log += chunk; });

  try {
    let ready = false;
    for (let attempt = 0; attempt < 80; attempt += 1) {
      try {
        const response = await fetch(base + '/health');
        if (response.ok) { ready = true; break; }
      } catch (error) { /* 还没起来 */ }
      await new Promise((done) => setTimeout(done, 100));
    }
    record('health', ready, ready ? '' : log.slice(-400));
    if (!ready) return observed;

    const caps = await (await fetch(base + '/capabilities')).json();
    record('capabilities', caps.has_encoder === true && caps.has_decoder === true, JSON.stringify(caps));

    const project = await (await fetch(base + '/projects/sample-project.doc')).json();
    // **钉住当前契约版本。** 版本升了这里就要跟着改 —— 那正是"升级牵动多少处"
    // 的可数证据，而不是让断言跟着代码自动漂（自动漂的断言什么都证明不了）。
    record('project-is-doc', project.project_schema === 1 && project.timeline && project.timeline.schema === 3,
      'project_schema=' + project.project_schema + ' timeline.schema=' + (project.timeline && project.timeline.schema));
    record('project-assets', Array.isArray(project.assets) && project.assets.length === 4,
      'assets=' + (project.assets ? project.assets.length : 'none'));

    const info = await (await fetch(base + '/assets/a.mp4/info')).json();
    record('assets-info', info.width === 1920 && info.height === 1080 && info.frame_count === 480,
      JSON.stringify(info));

    const gop = await (await fetch(base + '/assets/a.mp4/gop')).json();
    record('assets-gop', Array.isArray(gop.slices) && gop.slices.length > 1, 'slices=' + (gop.slices ? gop.slices.length : 'none'));

    const ranged = await fetch(base + '/assets/a.mp4/media', { headers: { range: 'bytes=0-1023' } });
    record('media-range', ranged.status === 206 && String(ranged.headers.get('content-range') || '').startsWith('bytes 0-1023/'),
      ranged.status + ' ' + ranged.headers.get('content-range'));

    const missingAsset = await fetch(base + '/assets/nope.mp4/media');
    record('media-missing', missingAsset.status === 404, missingAsset.status);

    // 素材库清点：**引用次数由 Rust 算**，这里只核对它确实报了出来。
    const library = await (await fetch(base + '/projects/sample-project.doc/library')).json();
    record('library',
      library.total === 4 && Array.isArray(library.unused) && library.unused.length === 0
        && library.assets.every((asset) => asset.references > 0),
      'total=' + library.total + ' unused=' + JSON.stringify(library.unused));

    // 登记素材的**守卫**：只许资产根下面的文件。
    // 不守这条的话，任何一个页面都能让本机后端把任意路径登记进工程 ——
    // "位置由宿主解释"就变成了"位置由网页解释"。
    const outside = await fetch(base + '/assets', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: 'sample-project.doc', path: 'fixtures/sample-project.json' }),
    });
    const outsideBody = await outside.json();
    record('assets-guard',
      outside.status === 400 && outsideBody.error !== undefined
        && outsideBody.error.code === 'path_outside_asset_root',
      outside.status + ' ' + JSON.stringify(outsideBody).slice(0, 140));

    const clean = await fetch(base + '/validate', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: project }),
    });
    const cleanBody = await clean.json();
    record('validate-clean', clean.ok && cleanBody.errors.length === 0, JSON.stringify(cleanBody).slice(0, 200));

    const broken = JSON.parse(JSON.stringify(project));
    broken.timeline.tracks[0].layers[0].end = -5;
    const brokenResponse = await fetch(base + '/validate', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: broken }),
    });
    const brokenBody = await brokenResponse.json();
    // **坏工程也是 200**：结论在 errors 里。HTTP 状态码表示"这次请求处理得怎么样"，
    // 不表示"这份工程好不好" —— 混起来前端就没法区分"服务挂了"与"工程有错"。
    record('validate-broken', brokenResponse.status === 200 && brokenBody.errors.length > 0,
      brokenResponse.status + ' errors=' + brokenBody.errors.length);


    // ---- 「出一帧真实出片帧」这条 API ----
    // 它兑现的是本工程的核心承诺（预览与出片给出的帧可比），所以两条都要钉：
    // 出得来（200 + 真 PNG + 帧号回在头里），以及**帧号是整数**这条契约。
    const frameResponse = await fetch(base + '/frame', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: project, frame: 12 }),
    });
    const frameBytes = Buffer.from(await frameResponse.arrayBuffer());
    const isPng = frameBytes.length > 8 && frameBytes[0] === 0x89
      && frameBytes.subarray(1, 4).toString('latin1') === 'PNG';
    // 尺寸从 PNG 的 IHDR 里读（第 16 字节起，大端）—— 不靠猜，也不靠再问一次接口。
    const pngWidth = isPng ? frameBytes.readUInt32BE(16) : 0;
    const pngHeight = isPng ? frameBytes.readUInt32BE(20) : 0;
    record('frame-png',
      frameResponse.status === 200
      && String(frameResponse.headers.get('content-type')).startsWith('image/png')
      && isPng
      && frameResponse.headers.get('x-dhampir-frame') === '12'
      && pngWidth === 640 && pngHeight === 360,
      frameResponse.status + ' ' + frameResponse.headers.get('content-type')
        + ' x-frame=' + frameResponse.headers.get('x-dhampir-frame')
        + ' png=' + isPng + ' ' + pngWidth + 'x' + pngHeight + ' ' + frameBytes.length + 'B');

    // 帧号是**契约单位**：收小数会让它被静默取整，那时「我要第 12.7 帧」变成第 12 帧，
    // 而没有任何人说出来。所以这里钉死它必须被拒。
    const badFrame = await fetch(base + '/frame', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: project, frame: 12.7 }),
    });
    const badFrameBody = await badFrame.json();
    record('frame-rejects-fraction',
      badFrame.status === 400 && badFrameBody.error && badFrameBody.error.code === 'bad_frame',
      badFrame.status + ' ' + JSON.stringify(badFrameBody).slice(0, 160));
    const submitted = await fetch(base + '/export', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: project, from: 0, to: 29, width: 640, height: 360 }),
    });
    const accepted = await submitted.json();
    record('export-accepted', submitted.status === 202 && typeof accepted.job_id === 'string',
      submitted.status + ' ' + JSON.stringify(accepted));

    let status = null;
    for (let attempt = 0; attempt < 900; attempt += 1) {
      status = await (await fetch(base + '/export/' + accepted.job_id)).json();
      if (status.state === 'succeeded' || status.state === 'failed') break;
      await new Promise((done) => setTimeout(done, 200));
    }
    record('export-succeeded', status !== null && status.state === 'succeeded', JSON.stringify(status));
    record('export-progress-unknown-or-one', status !== null && status.progress === 1, status && status.progress);

    // **导出没成功时不许在这里崩。** 这里原本直接拼 `base + status.download_url`，
    // 拿不到地址就拼出 `http://127.0.0.1:8799undefined` 然后抛 TypeError ——
    // 守卫崩掉会把"到底哪条判据没过"一起吞掉，**那是守卫自己的缺陷**：
    // 报告不出来的检查等于没有检查。
    const downloadUrl = status !== null && typeof status.download_url === 'string'
      ? status.download_url : null;
    const download = downloadUrl === null ? null : await fetch(base + downloadUrl);
    const bytes = download !== null && download.ok
      ? Buffer.from(await download.arrayBuffer()) : Buffer.alloc(0);
    record('download-ok', download !== null && download.ok && bytes.length > 0,
      download === null
        ? '导出没有给出下载地址：' + JSON.stringify(status).slice(0, 200)
        : download.status + ' ' + bytes.length + 'B');

    const outDir = join(REPO_ROOT, 'target', 'p6');
    mkdirSync(outDir, { recursive: true });
    const outPath = join(outDir, 'local-download.mp4');
    // **先删掉上一轮的产物。** 留着的话，这一轮明明没下载成功，ffprobe 也会去读旧文件，
    // 于是"产物形状对不对"这条判据替上一轮的自己背了书 —— 一个假绿。
    tryRemove(outPath);
    const wrote = bytes.length > 0;
    if (wrote) writeFileSync(outPath, bytes);
    const probe = wrote
      ? runToolSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-count_frames',
          '-show_entries', 'stream=nb_read_frames,width,height,avg_frame_rate,duration', '-of', 'json', outPath])
      : { stdout: '' };
    let stream = null;
    try { stream = JSON.parse(probe.stdout).streams[0]; } catch (error) { stream = null; }
    record('download-frames', stream !== null && Number(stream.nb_read_frames) === 30,
      stream === null ? '没有产物' : stream.nb_read_frames);
    record('download-shape', stream !== null && stream.width === 640 && stream.height === 360,
      stream === null ? '' : stream.width + 'x' + stream.height);
    record('download-seconds', stream !== null && stream.duration === '1.000000',
      stream === null ? '' : stream.duration + ' avg=' + stream.avg_frame_rate);

    const second = await (await fetch(base + '/export', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ project: project, from: 0, to: 89, width: 1920, height: 1080 }),
    })).json();
    const cancelled = await fetch(base + '/export/' + second.job_id, { method: 'DELETE' });
    const cancelledBody = await cancelled.json();
    record('cancel', cancelled.ok && cancelledBody.state === 'cancelled', JSON.stringify(cancelledBody));
    record('cancel-twice', (await fetch(base + '/export/' + second.job_id, { method: 'DELETE' })).status === 409, '');

    record('missing-job', (await fetch(base + '/export/nope')).status === 404, '');
  } finally {
    try { child.kill(); } catch (error) { /* 已经没了 */ }
  }
  return observed;
}

async function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) { runSelfTest(); return; }
  const value = (name, fallback) => {
    const index = argv.indexOf(name);
    return index >= 0 ? argv[index + 1] : fallback;
  };
  // 默认按**当前平台**的可执行名找（Windows 是 dhampir.exe，Linux/macOS 是 dhampir）。
  const cli = value('--cli', join(REPO_ROOT, 'target', 'debug', process.platform === 'win32' ? 'dhampir.exe' : 'dhampir'));
  if (!existsSync(cli)) {
    console.error('找不到 dhampir 可执行文件：' + cli);
    console.error('先跑：cargo build -p dhampir-worker --bin dhampir');
    process.exitCode = 2;
    return;
  }
  const port = Number(value('--port', '8799'));
  const observed = await collect(port);
  const problems = judge(observed);
  const ok = observed.filter((row) => row.ok).length;
  console.log('本机后端：' + ok + ' / ' + EXPECTED.length + ' 条判据通过');
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 本机后端不满足端到端约定');
    process.exitCode = 1;
    return;
  }
  console.log('✓ 本机后端端到端成立（校验 / 素材 info+gop / Range / 出片 / 下载 / 取消）');
}

main().catch((error) => {
  console.error('检查跑挂了：' + String(error && error.stack ? error.stack : error));
  process.exitCode = 1;
});
