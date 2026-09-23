#!/usr/bin/env node
// 本机后端（参考实现）。
//
// # 它是什么
//
// **本机模式 = 分离模式的后端跑在 localhost。** 所以这个服务的形状就是分离模式后端的形状：
// 前端只认这些路由，不认后端在哪。把前端从"后端在远端"切到"后端在本地"应当**只改注入点**。
//
// # 它刻意保持"薄"
//
// 它**不重新实现**状态机、也不自己判工程合法性 —— 那些只有一个实现，在 Rust 里。
// 这个文件只负责：路由、任务生命周期、以及把错误包成**同一套 Issue 形状**。
// 一旦这里长出渲染或时间线逻辑，就出现了"本机与远端行为不同"的可能，
// 而那正是这个项目一直在防的事。
//
// # 当前状态：骨架
//
// 路由与任务生命周期是真的；**渲染与校验还没接上**（那要调 Rust 侧）。
// 没接上的端点返回 501 + 一个 Issue，而不是假装成功 ——
// 假装成功会让前端在错误的地方失败，更难查。
//
// 用法：
//   node scripts/dhampir-local.mjs                 起服务（默认随机端口，打印 URL）
//   node scripts/dhampir-local.mjs --port 8787
//   node scripts/dhampir-local.mjs --self-test     只跑自检

import { createServer } from 'node:http';

/** 错误一律走这个形状 —— 与 Rust 侧的 Issue 同一套，前端只认一种。 */
function issue(code, path, message) {
  return { code: code, path: path, message: message };
}

function sendJson(res, status, body) {
  const text = JSON.stringify(body);
  const bytes = Buffer.byteLength(text);
  res.writeHead(status, { 'content-type': 'application/json; charset=utf-8', 'content-length': bytes });
  res.end(text);
}

/** 出片任务的状态机。**与 host_api::ExportState 同构**，但这里只是记账。 */
export const TERMINAL = ['succeeded', 'failed', 'cancelled'];

export function canTransition(from, to) {
  if (TERMINAL.includes(from)) return false;
  if (from === 'queued') return ['running', 'succeeded', 'failed', 'cancelled'].includes(to);
  if (from === 'running') return ['succeeded', 'failed', 'cancelled'].includes(to);
  return false;
}

/** 能力声明。**从"本实现实际能做到什么"出发**，不抄一份好看的清单。 */
export function capabilities() {
  return {
    timeline_versions: [2],
    // 混合模式只列实现的那四种 —— 与 BlendMode::is_implemented() 同一事实。
    blend_modes: ['normal', 'add', 'multiply', 'screen'],
    effects: ['gaussian_blur'],
    max_blur_radius: 16,
    // 实话：本机后端现在还没有解码器与编码器。
    has_decoder: false,
    has_encoder: false,
  };
}

export function createBackend() {
  const jobs = new Map();
  let counter = 0;

  return {
    jobs: jobs,
    submit() {
      counter += 1;
      const id = 'job-' + counter;
      jobs.set(id, { job_id: id, state: 'queued', progress: null, download_url: null, error: null });
      return id;
    },
    transition(id, next) {
      const job = jobs.get(id);
      if (!job) return false;
      if (!canTransition(job.state, next)) return false;
      job.state = next;
      return true;
    },
    cancel(id) {
      return this.transition(id, 'cancelled');
    },
  };
}

function handle(req, res, backend, url) {
  const path = url.pathname;

  if (req.method === 'GET' && path === '/health') {
    return sendJson(res, 200, { ok: true });
  }
  if (req.method === 'GET' && path === '/capabilities') {
    return sendJson(res, 200, capabilities());
  }
  if (req.method === 'POST' && (path === '/validate' || path === '/export')) {
    // **没接上就明说**，而不是假装成功。
    return sendJson(res, 501, {
      error: issue(
        'backend_incomplete',
        path,
        '这个参考实现还没有接上 Rust 侧的' + (path === '/validate' ? '校验' : '渲染') + '；形状已定，接线待做'
      ),
    });
  }

  const match = path.match(/^\/export\/([^/]+)$/);
  if (match) {
    const job = backend.jobs.get(match[1]);
    if (!job) return sendJson(res, 404, { error: issue('no_such_job', path, '没有这个任务：' + match[1]) });
    if (req.method === 'GET') {
      // 空的可选字段**不出现**，而不是塞 null —— 与 Rust 侧同一条规矩。
      const body = { job_id: job.job_id, state: job.state };
      if (job.progress !== null) body.progress = job.progress;
      if (job.download_url !== null) body.download_url = job.download_url;
      if (job.error !== null) body.error = job.error;
      return sendJson(res, 200, body);
    }
    if (req.method === 'DELETE') {
      const ok = backend.cancel(job.job_id);
      return sendJson(res, ok ? 200 : 409, ok ? { job_id: job.job_id, state: job.state } : {
        error: issue('not_cancellable', path, '任务已经是 ' + job.state + '，取消不了'),
      });
    }
  }

  sendJson(res, 404, { error: issue('no_such_route', path, '没有这个路由：' + req.method + ' ' + path) });
}

function runSelfTest() {
  let passed = 0;
  const expect = (name, condition) => {
    if (!condition) throw new Error('自检失败：' + name);
    passed += 1;
  };

  expect('终态不能再转', !canTransition('succeeded', 'running') && !canTransition('cancelled', 'queued'));
  expect('queued 可以直接到 succeeded', canTransition('queued', 'succeeded'));
  expect('running 回不到 queued', !canTransition('running', 'queued'));

  const backend = createBackend();
  const id = backend.submit();
  expect('提交后是 queued', backend.jobs.get(id).state === 'queued');
  expect('取消成功', backend.cancel(id));
  expect('取消后是终态', backend.jobs.get(id).state === 'cancelled');
  expect('终态再取消失败', !backend.cancel(id));

  const caps = capabilities();
  expect('能力里只列实现的混合模式', caps.blend_modes.length === 4 && !caps.blend_modes.includes('overlay'));
  expect('能力里如实说明没有解码器', caps.has_decoder === false);

  console.log('OK 本机后端自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }

  const portIndex = process.argv.indexOf('--port');
  const port = portIndex >= 0 ? Number(process.argv[portIndex + 1]) : 0;
  const backend = createBackend();
  const server = createServer((req, res) => {
    handle(req, res, backend, new URL(req.url, 'http://127.0.0.1'));
  });
  server.listen(port, '127.0.0.1', () => {
    const actual = server.address().port;
    console.log('本机后端：http://127.0.0.1:' + actual);
    console.log('（骨架：路由与任务生命周期是真的；渲染与校验待接 Rust 侧）');
  });
}

main();
