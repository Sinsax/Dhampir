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
//   node scripts/check-local-backend.mjs --port 8799 --cli target/debug/dhampir.exe

import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 每一条判据的名字。**改这里就必须改采集端**，judge 会把差异指出来。 */
export const EXPECTED = [
  'health',
  'capabilities',
  'project-is-doc',
  'project-assets',
  'assets-info',
  'assets-gop',
  'media-range',
  'media-missing',
  'validate-clean',
  'validate-broken',
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
    record('project-is-doc', project.project_schema === 1 && project.timeline && project.timeline.schema === 2,
      'project_schema=' + project.project_schema);
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

    const download = await fetch(base + String(status && status.download_url));
    const bytes = download.ok ? Buffer.from(await download.arrayBuffer()) : Buffer.alloc(0);
    record('download-ok', download.ok && bytes.length > 0, download.status + ' ' + bytes.length + 'B');

    const outDir = join(REPO_ROOT, 'target', 'p6');
    mkdirSync(outDir, { recursive: true });
    const outPath = join(outDir, 'local-download.mp4');
    if (bytes.length > 0) writeFileSync(outPath, bytes);
    const probe = existsSync(outPath)
      ? spawnSync('ffprobe', ['-v', 'error', '-select_streams', 'v:0', '-count_frames',
          '-show_entries', 'stream=nb_read_frames,width,height,avg_frame_rate,duration', '-of', 'json', outPath],
          { encoding: 'utf8' })
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
  const cli = value('--cli', join(REPO_ROOT, 'target', 'debug', 'dhampir.exe'));
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
