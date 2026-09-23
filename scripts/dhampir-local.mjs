#!/usr/bin/env node
// 本机后端（参考实现）。
//
// # 它是什么
//
// **本机模式 = 分离模式的后端跑在 localhost。** 所以这个服务的形状就是分离模式后端的形状：
// 前端只认这些路由，不认后端在哪。把前端从"后端在远端"切到"后端在本地"应当只改注入点。
//
// # 分工：HTTP 在 Node，渲染在 Rust
//
// 这个文件只负责四件事：路由、任务生命周期、把请求落成临时文件、把 Rust 的输出翻成 HTTP。
// **它不重新实现任何时间线逻辑，也不自己解码或渲染** —— 那些只有一个实现，在 Rust 里
// （crates/dhampir-worker/src/bin/dhampir.rs）。
//
// 这条分工是被逼出来的，但结论比"凑合"好：
//   * 一旦这里长出渲染或校验逻辑，就出现了"本机与远端行为不同"的可能，
//     而那正是这个项目一直在防的事；
//   * 反过来，只要 CLI 够用，任何下游工程都能照这个形状接一个自己的后端。
//
// # 资产位置从哪来
//
// 从**工程文件自己的资产表**（assets[].uri，相对 --asset-root 解析）。
// fixtures/local-assets.json 降为**兜底**，只补工程文件没登记的 id。
// 以前的注释写着"这张表是占位，等工程文件壳接上后由它生成"—— 现在就是那时候。
//
// # 用法
//
//   node scripts/dhampir-local.mjs                       起服务（默认随机端口，打印 URL）
//   node scripts/dhampir-local.mjs --port 8787
//   node scripts/dhampir-local.mjs --cli target/debug/dhampir.exe
//   node scripts/dhampir-local.mjs --asset-root target/s3
//   node scripts/dhampir-local.mjs --self-test           只跑自检

import { createReadStream, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { dirname, extname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 工程文件放哪。路由 /projects/<id> 就是 fixtures/<id>.json。 */
const PROJECTS_DIR = join(REPO_ROOT, 'fixtures');
/** 兜底登记表。工程文件的 assets 优先于它。 */
const FALLBACK_REGISTRY = join(PROJECTS_DIR, 'local-assets.json');
/** 出片产物与临时文件都放 target/（已忽略），仓库里不留垃圾。 */
const EXPORT_DIR = join(REPO_ROOT, 'target', 'p6', 'local-exports');
const TMP_DIR = join(REPO_ROOT, 'target', 'p6', 'local-tmp');

const MIME = {
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
  '.wav': 'audio/wav',
  '.m4a': 'audio/mp4',
  '.json': 'application/json; charset=utf-8',
};

/** 错误一律走这个形状 —— 与 Rust 侧的 Issue 同一套，前端只认一种。 */
function issue(code, path, message) {
  return { code: code, path: path, message: message };
}

const CORS = {
  'access-control-allow-origin': '*',
  // **跨源读得到的响应头要显式列出来。** 不列的话 JS 读 content-range 会拿到 null，
  // 而"读不到"和"没有这个头"在代码里长得一样。
  'access-control-expose-headers': 'content-range, accept-ranges, content-length',
};

/**
 * 预检的回答。
 *
 * **跨源的 POST + content-type: application/json 会先发一个 OPTIONS。**
 * 不回答它的表现是浏览器里的 "Failed to fetch" —— 看起来像后端没起来，
 * 而 curl 打同一个地址是通的（curl 不做预检）。本机模式第一次跑就是这么卡的。
 */
function preflight(res) {
  res.writeHead(204, {
    ...CORS,
    'access-control-allow-methods': 'GET, POST, DELETE, OPTIONS',
    'access-control-allow-headers': 'content-type, range',
    'access-control-max-age': '600',
  });
  res.end();
}

// ---------------------------------------------------------------------------
// 纯逻辑
// ---------------------------------------------------------------------------

/** 出片任务的状态机。**与 host_api::ExportState 同构**，但这里只是记账。 */
export const TERMINAL = ['succeeded', 'failed', 'cancelled'];

export function canTransition(from, to) {
  if (TERMINAL.includes(from)) return false;
  if (from === 'queued') return ['running', 'succeeded', 'failed', 'cancelled'].includes(to);
  if (from === 'running') return ['succeeded', 'failed', 'cancelled'].includes(to);
  return false;
}

/**
 * 从「工程文件文本」与「兜底登记表文本」拼出 id -> 文件。
 *
 * 优先级是有意的：**工程文件的 assets 先来，兜底表只补缺**。
 * 反过来的话，兜底表会悄悄盖掉工程文件里写的真实位置，而用户看不到。
 */
export function parseAssetIndex(projectTexts, fallbackText, assetRoot) {
  const resolved = new Map();
  const add = (id, uri, source) => {
    if (typeof id !== 'string' || id.length === 0) return;
    if (typeof uri !== 'string' || uri.length === 0) return;
    // 先来的赢。
    if (resolved.has(id)) return;
    resolved.set(id, {
      id: id,
      file: isAbsolute(uri) ? uri : join(assetRoot, uri),
      source: source,
    });
  };

  for (const text of projectTexts || []) {
    let doc = null;
    try { doc = JSON.parse(text); } catch (error) { continue; }
    // **只有工程文件形态才有资产表。** 裸契约里 source 只是个字符串，
    // 没有任何位置信息 —— 那不是"漏了"，是那一版的契约本来就这样。
    if (doc === null || doc.project_schema === undefined) continue;
    if (!Array.isArray(doc.assets)) continue;
    for (const asset of doc.assets) add(asset.id, asset.uri, 'project');
  }

  if (typeof fallbackText === 'string' && fallbackText.length > 0) {
    let table = null;
    try { table = JSON.parse(fallbackText).assets; } catch (error) { table = null; }
    if (table !== null && typeof table === 'object') {
      for (const id of Object.keys(table)) {
        const entry = table[id];
        if (entry !== null && typeof entry === 'object') add(id, entry.file, 'fallback');
      }
    }
  }
  return resolved;
}

/**
 * 解析 CLI 在 stdout 上打的 NDJSON。
 *
 * 不是 JSON 的行**不当事件**（CLI 可能打了一行诊断），但也**不报错** ——
 * 进度是观测手段，观测出问题不该让出片失败。
 */
export function parseNdjsonLines(text) {
  const events = [];
  for (const line of String(text).split('\n')) {
    const trimmed = line.trim();
    if (trimmed.length === 0) continue;
    try {
      const parsed = JSON.parse(trimmed);
      if (parsed !== null && typeof parsed === 'object') events.push(parsed);
    } catch (error) {
      // 不是 JSON，跳过。
    }
  }
  return events;
}

/**
 * 把事件流压成任务上要显示的字段。**纯函数**，自检与真跑走同一段判定。
 *
 * progress **未知时是 null 而不是 0** —— 契约里明说「进度未知」与「进度是零」是两件事。
 */
export function reduceRenderEvents(events, current) {
  let progress = current === undefined ? null : current;
  let summary = null;
  for (const event of events) {
    if (event.event === 'progress' && typeof event.total === 'number' && event.total > 0) {
      progress = Math.min(1, Math.max(0, event.done / event.total));
    } else if (event.event === 'done') {
      summary = event;
      progress = 1;
    } else if (event.event === 'start' && progress === null) {
      // 还没出第一帧：0 是**已知**的（一帧都没出），可以显示。
      progress = 0;
    }
  }
  return { progress: progress, summary: summary };
}

/** 找一个能跑的 CLI。顺序：显式给 > 环境变量 > 构建目录。 */
export function findCli(explicit, env) {
  const candidates = [];
  if (explicit) candidates.push(explicit);
  if (env && env.DHAMPIR_CLI) candidates.push(env.DHAMPIR_CLI);
  candidates.push(join(REPO_ROOT, 'target', 'debug', 'dhampir.exe'));
  candidates.push(join(REPO_ROOT, 'target', 'debug', 'dhampir'));
  candidates.push(join(REPO_ROOT, 'target', 'release', 'dhampir.exe'));
  candidates.push(join(REPO_ROOT, 'target', 'release', 'dhampir'));
  for (const candidate of candidates) {
    if (typeof candidate === 'string' && candidate.length > 0 && existsSync(candidate)) return candidate;
  }
  return null;
}

/** 能力声明。**从"本实现实际能做到什么"出发**，不抄一份好看的清单。 */
export function capabilities() {
  return {
    timeline_versions: [2],
    // 混合模式只列实现的那四种 —— 与 BlendMode::is_implemented() 同一事实。
    blend_modes: ['normal', 'add', 'multiply', 'screen'],
    effects: ['gaussian_blur'],
    max_blur_radius: 16,
    // 接入 CLI 之后这两条才是真的：解码走 ffmpeg 顺序管道，编码走 libx264。
    // 在此之前这里是 false —— 那时确实做不到，不是谦虚。
    has_decoder: true,
    has_encoder: true,
  };
}

// ---------------------------------------------------------------------------
// 起子进程
// ---------------------------------------------------------------------------

/** 跑一次 CLI 并收全输出。给 probe / info / gop 这类短命令用。 */
function runCli(cli, args, timeoutMs) {
  return new Promise((resolvePromise) => {
    const child = spawn(cli, args, { cwd: REPO_ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    const timer = setTimeout(() => {
      try { child.kill(); } catch (error) { /* 已经没了 */ }
    }, timeoutMs || 120000);
    child.on('close', (code) => {
      clearTimeout(timer);
      resolvePromise({ code: code === null ? -1 : code, stdout: stdout, stderr: stderr });
    });
    child.on('error', (error) => {
      clearTimeout(timer);
      resolvePromise({ code: -1, stdout: stdout, stderr: String(error && error.message ? error.message : error) });
    });
  });
}

// ---------------------------------------------------------------------------
// 资产索引（从磁盘读一次）
// ---------------------------------------------------------------------------

export function loadAssetIndex(assetRoot) {
  const texts = [];
  if (existsSync(PROJECTS_DIR)) {
    for (const name of readdirSync(PROJECTS_DIR)) {
      if (!name.endsWith('.json')) continue;
      try { texts.push(readFileSync(join(PROJECTS_DIR, name), 'utf8')); } catch (error) { /* 读不了就跳过 */ }
    }
  }
  let fallback = '';
  if (existsSync(FALLBACK_REGISTRY)) {
    try { fallback = readFileSync(FALLBACK_REGISTRY, 'utf8'); } catch (error) { fallback = ''; }
  }
  return parseAssetIndex(texts, fallback, assetRoot);
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

function sendJson(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    ...CORS,
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(text),
  });
  res.end(text);
}

/** 流式发文件。用 content-length 而不是 chunked，前端好做进度。 */
function sendFile(req, res, file, contentType) {
  const size = statSync(file).size;
  // **必须支持 Range。** video 元素一定会发 Range 请求，而不支持它的媒体路由
  // 会让浏览器拿不到"读到哪一段"的确认 —— 表现是**卡住而不报错**，那种状态最难查。
  const range = req.headers.range;
  if (range) {
    const match = /^bytes=(\d*)-(\d*)$/.exec(String(range).trim());
    if (match) {
      let start = match[1] === '' ? null : Number(match[1]);
      let end = match[2] === '' ? null : Number(match[2]);
      if (start === null && end !== null) {
        // bytes=-N：最后 N 字节。
        start = Math.max(0, size - end);
        end = size - 1;
      } else {
        if (start === null) start = 0;
        if (end === null || end >= size) end = size - 1;
      }
      if (start >= 0 && start <= end && start < size) {
        const length = end - start + 1;
        res.writeHead(206, {
          ...CORS,
          'content-type': contentType,
          'content-length': length,
          'content-range': 'bytes ' + start + '-' + end + '/' + size,
          'accept-ranges': 'bytes',
        });
        createReadStream(file, { start: start, end: end }).pipe(res);
        return;
      }
      res.writeHead(416, { ...CORS, 'content-range': 'bytes */' + size });
      res.end();
      return;
    }
  }
  res.writeHead(200, {
    ...CORS,
    'content-type': contentType,
    'content-length': size,
    'accept-ranges': 'bytes',
  });
  createReadStream(file).pipe(res);
}

function contentTypeFor(file, fallback) {
  const type = MIME[extname(file).toLowerCase()];
  return type === undefined ? fallback : type;
}

function readBody(req, callback) {
  const chunks = [];
  req.on('data', (chunk) => chunks.push(chunk));
  req.on('end', () => callback(Buffer.concat(chunks)));
}

/** 任务记账。**只记账**：渲染是子进程的事。 */
export function createBackend() {
  const jobs = new Map();
  let counter = 0;
  return {
    jobs: jobs,
    submit() {
      counter += 1;
      const id = 'job-' + counter;
      jobs.set(id, {
        job_id: id,
        state: 'queued',
        progress: null,
        download_url: null,
        error: null,
        child: null,
        output: null,
        temporary: null,
      });
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

/** 任务状态的**对外形状**。可选字段不出现，而不是塞 null —— 与 Rust 侧同一条规矩。 */
export function jobStatus(job) {
  const body = { job_id: job.job_id, state: job.state };
  if (job.progress !== null) body.progress = job.progress;
  if (job.download_url !== null) body.download_url = job.download_url;
  if (job.error !== null) body.error = job.error;
  return body;
}

/** 工程体：允许裸的工程对象，也允许包在 { project: ... } 里。 */
function projectFromBody(parsed) {
  if (parsed === null || typeof parsed !== 'object') return null;
  if (parsed.project !== undefined && parsed.project !== null && typeof parsed.project === 'object') {
    return parsed.project;
  }
  return parsed;
}

// ---------------------------------------------------------------------------
// 路由
// ---------------------------------------------------------------------------

function handle(req, res, context, url) {
  const path = url.pathname;
  const { backend, cli, assetRoot, assets } = context;

  // 预检**排在所有路由之前**：它是浏览器问"这个跨源请求能不能发"，
  // 与该请求要落到哪个路由无关。
  if (req.method === 'OPTIONS') return preflight(res);

  if (req.method === 'GET' && path === '/health') {
    return sendJson(res, 200, { ok: true });
  }

  if (req.method === 'GET' && path === '/capabilities') {
    if (cli === null) {
      return sendJson(res, 503, {
        error: issue(
          'cli_missing',
          '/capabilities',
          '没找到 dhampir 可执行文件。先跑：cargo build -p dhampir-worker --bin dhampir'
        ),
      });
    }
    return sendJson(res, 200, capabilities());
  }

  // 工程：从 fixtures/ 取。**id 要过白名单**，否则就是任意文件读取。
  const project = path.match(/^\/projects\/([A-Za-z0-9_.-]+)$/);
  if (req.method === 'GET' && project) {
    const file = join(PROJECTS_DIR, project[1] + '.json');
    if (!existsSync(file)) {
      return sendJson(res, 404, { error: issue('no_such_project', path, '没有这份工程：' + project[1]) });
    }
    return sendFile(req, res, file, 'application/json; charset=utf-8');
  }

  // 素材字节。位置**由索引解释**（工程文件的 assets 优先，兜底表补缺），
  // 而不是假定 id 就是文件名 —— 样本工程引用 a.mp4，而文件叫 proxy1080p.mp4。
  const asset = path.match(/^\/assets\/([A-Za-z0-9_.-]+)\/media$/);
  if (req.method === 'GET' && asset) {
    const resolved = resolveAsset(assets, asset[1], path);
    if (resolved.error !== undefined) return sendJson(res, resolved.status, { error: resolved.error });
    return sendFile(req, res, resolved.file, contentTypeFor(resolved.file, 'application/octet-stream'));
  }

  // info 与 gop 现在**真的接了**：调 Rust 侧的分离器，不在这里重写一份解析。
  const infoRoute = path.match(/^\/assets\/([A-Za-z0-9_.-]+)\/(info|gop)$/);
  if (req.method === 'GET' && infoRoute) {
    if (cli === null) {
      return sendJson(res, 503, {
        error: issue('cli_missing', path, '没找到 dhampir 可执行文件，素材信息要它来读'),
      });
    }
    const resolved = resolveAsset(assets, infoRoute[1], path);
    if (resolved.error !== undefined) return sendJson(res, resolved.status, { error: resolved.error });
    return void runCli(cli, [infoRoute[2], '--asset', resolved.file], 120000).then((result) => {
      if (result.code !== 0) {
        return sendJson(res, 500, {
          error: issue('asset_probe_failed', path, (result.stderr || '').trim() || ('dhampir ' + infoRoute[2] + ' 退出 ' + result.code)),
        });
      }
      try {
        return sendJson(res, 200, JSON.parse(result.stdout));
      } catch (error) {
        return sendJson(res, 500, { error: issue('asset_probe_bad_json', path, '分离器没给出 JSON：' + error.message) });
      }
    });
  }

  if (req.method === 'POST' && path === '/validate') {
    if (cli === null) {
      return sendJson(res, 503, { error: issue('cli_missing', path, '没找到 dhampir 可执行文件') });
    }
    return readBody(req, (body) => {
      let parsed = null;
      try { parsed = JSON.parse(body.toString('utf8')); } catch (error) {
        return sendJson(res, 400, { error: issue('bad_request', path, '请求体不是 JSON：' + error.message) });
      }
      const project = projectFromBody(parsed);
      if (project === null) {
        return sendJson(res, 400, { error: issue('no_project', 'project', '请求里没有工程') });
      }
      mkdirSync(TMP_DIR, { recursive: true });
      const temporary = join(TMP_DIR, 'validate-' + Date.now() + '-' + process.pid + '.json');
      writeFileSync(temporary, JSON.stringify(project), 'utf8');
      void runCli(cli, ['probe', '--project', temporary], 60000).then((result) => {
        try { rmSync(temporary, { force: true }); } catch (error) { /* 清不掉就算了 */ }
        // 退出码 2 = 校验有 error，那是**正常结论**，不是服务端故障。
        if (result.code !== 0 && result.code !== 2) {
          return sendJson(res, 500, {
            error: issue('validate_failed', path, (result.stderr || '').trim() || ('dhampir probe 退出 ' + result.code)),
          });
        }
        try {
          return sendJson(res, 200, JSON.parse(result.stdout));
        } catch (error) {
          return sendJson(res, 500, { error: issue('validate_bad_json', path, '校验器没给出 JSON：' + error.message) });
        }
      });
    });
  }

  if (req.method === 'POST' && path === '/export') {
    if (cli === null) {
      return sendJson(res, 503, {
        error: issue(
          'cli_missing',
          path,
          '没找到 dhampir 可执行文件。先跑：cargo build -p dhampir-worker --bin dhampir'
        ),
      });
    }
    return readBody(req, (body) => {
      let parsed = null;
      try { parsed = JSON.parse(body.toString('utf8')); } catch (error) {
        return sendJson(res, 400, { error: issue('bad_request', path, '请求体不是 JSON：' + error.message) });
      }
      const project = projectFromBody(parsed);
      if (project === null) {
        return sendJson(res, 400, { error: issue('no_project', 'project', '请求里没有工程') });
      }

      mkdirSync(EXPORT_DIR, { recursive: true });
      mkdirSync(TMP_DIR, { recursive: true });
      const id = backend.submit();
      const job = backend.jobs.get(id);
      const temporary = join(TMP_DIR, id + '.json');
      const output = join(EXPORT_DIR, id + '.mp4');
      writeFileSync(temporary, JSON.stringify(project), 'utf8');
      job.temporary = temporary;
      job.output = output;
      // **download_url 只在真的有产物时才出现** —— 契约里可选字段不出现，
      // 而一个指向失败任务的下载地址比"没有"更容易骗到调用方。

      const args = ['render', '--project', temporary, '--out', output, '--asset-root', assetRoot];
      if (Number.isFinite(parsed.from)) args.push('--from', String(parsed.from));
      if (Number.isFinite(parsed.to)) args.push('--to', String(parsed.to));
      if (Number.isFinite(parsed.width)) args.push('--width', String(parsed.width));
      if (Number.isFinite(parsed.height)) args.push('--height', String(parsed.height));
      // 兜底登记表：**只对工程文件没登记的 id 生效**（CLI 侧同样是工程文件优先）。
      if (existsSync(FALLBACK_REGISTRY)) args.push('--asset-map', FALLBACK_REGISTRY);

      const child = spawn(cli, args, { cwd: REPO_ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
      job.child = child;
      backend.transition(id, 'running');

      let stdoutBuffer = '';
      let stderrTail = '';
      child.stdout.on('data', (chunk) => {
        stdoutBuffer += chunk;
        // 只处理完整行，留下的半行等下一次。
        const cut = stdoutBuffer.lastIndexOf('\n');
        if (cut < 0) return;
        const complete = stdoutBuffer.slice(0, cut + 1);
        stdoutBuffer = stdoutBuffer.slice(cut + 1);
        const reduced = reduceRenderEvents(parseNdjsonLines(complete), job.progress);
        job.progress = reduced.progress;
      });
      child.stderr.on('data', (chunk) => {
        stderrTail = (stderrTail + chunk).slice(-4000);
      });
      child.on('error', (error) => {
        backend.transition(id, 'failed');
        job.error = issue('render_spawn_failed', path, String(error && error.message ? error.message : error));
        job.child = null;
      });
      child.on('close', (code) => {
        job.child = null;
        try { rmSync(temporary, { force: true }); } catch (error) { /* 清不掉就算了 */ }
        job.temporary = null;
        if (job.state === 'cancelled') return;
        if (code === 0) {
          job.download_url = '/export/' + id + '/download';
          backend.transition(id, 'succeeded');
          job.progress = 1;
          return;
        }
        backend.transition(id, 'failed');
        job.error = issue(
          'render_failed',
          path,
          (stderrTail || '').trim() || ('dhampir render 退出 ' + code)
        );
      });

      return sendJson(res, 202, { job_id: id });
    });
  }

  // 下载要排在 /export/:id **前面**：它是更长的路径，先匹配短的就把 download 吃掉了。
  const download = path.match(/^\/export\/([^/]+)\/download$/);
  if (req.method === 'GET' && download) {
    const job = backend.jobs.get(download[1]);
    if (!job) return sendJson(res, 404, { error: issue('no_such_job', path, '没有这个任务：' + download[1]) });
    if (job.state !== 'succeeded' || job.output === null || !existsSync(job.output)) {
      return sendJson(res, 409, {
        error: issue('not_ready', path, '任务还是 ' + job.state + '，产物还拿不到'),
      });
    }
    return sendFile(req, res, job.output, 'video/mp4');
  }

  const one = path.match(/^\/export\/([^/]+)$/);
  if (one) {
    const job = backend.jobs.get(one[1]);
    if (!job) return sendJson(res, 404, { error: issue('no_such_job', path, '没有这个任务：' + one[1]) });
    if (req.method === 'GET') return sendJson(res, 200, jobStatus(job));
    if (req.method === 'DELETE') {
      if (!backend.cancel(job.job_id)) {
        return sendJson(res, 409, {
          error: issue('not_cancellable', path, '任务已经是 ' + job.state + '，取消不了'),
        });
      }
      // **真把子进程杀掉**，否则"取消了"只是记账上的说法，机器还在跑。
      if (job.child !== null) {
        try { job.child.kill(); } catch (error) { /* 已经结束了 */ }
      }
      if (job.temporary !== null) {
        try { rmSync(job.temporary, { force: true }); } catch (error) { /* 清不掉就算了 */ }
        job.temporary = null;
      }
      return sendJson(res, 200, jobStatus(job));
    }
  }

  sendJson(res, 404, { error: issue('no_such_route', path, '没有这个路由：' + req.method + ' ' + path) });
}

/** 解析 asset id。三种失败**分开报**，因为前端该做的事不一样。 */
export function resolveAsset(assets, id, path) {
  const entry = assets.get(id);
  if (entry === undefined) {
    return { status: 404, error: issue('no_such_asset', path, '资产索引里没有：' + id) };
  }
  if (!existsSync(entry.file)) {
    return {
      status: 404,
      error: issue('asset_file_missing', path, '索引说在 ' + entry.file + '，但文件不在'),
    };
  }
  return { file: entry.file };
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

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
  expect('接了 CLI 之后如实说能解码能编码', caps.has_decoder === true && caps.has_encoder === true);

  // ---- 资产索引 ----
  const doc = JSON.stringify({
    project_schema: 1,
    assets: [{ id: 'a.mp4', kind: 'video', uri: 'proxy1080p.mp4' }],
  });
  const fallback = JSON.stringify({ assets: { 'a.mp4': { file: 'wrong.mp4' }, 'b.mp4': { file: 'proxy1080p.mp4' } } });
  const index = parseAssetIndex([doc, 'not json'], fallback, 'target/s3');
  expect('工程文件的 assets 优先于兜底表', String(index.get('a.mp4').file).endsWith('proxy1080p.mp4'));
  expect('兜底表补工程文件没登记的 id', index.get('b.mp4').source === 'fallback');
  expect('裸契约不进索引', parseAssetIndex([JSON.stringify({ schema: 1, tracks: [] })], '', 'x').size === 0);
  expect('坏 JSON 不让索引整体崩', parseAssetIndex(['{oops'], '{"assets":{"z.mp4":{"file":"z.mp4"}}}', 'x').size === 1);
  expect('绝对 uri 原样', parseAssetIndex(
    [JSON.stringify({ project_schema: 1, assets: [{ id: 'c', uri: 'C:/abs/c.mp4' }] })], '', 'x'
  ).get('c').file.replace(/\\/g, '/') === 'C:/abs/c.mp4');

  // ---- NDJSON 与进度 ----
  const ndjson = '{"event":"start","total":90}\nnot json\n{"event":"progress","done":45,"total":90}\n\n{"event":"done","frames":90}\n';
  const events = parseNdjsonLines(ndjson);
  expect('NDJSON 跳过非 JSON 行', events.length === 3);
  const reduced = reduceRenderEvents(events, null);
  expect('进度取到最后一帧的比例', reduced.progress === 1);
  expect('done 被记成摘要', reduced.summary !== null && reduced.summary.frames === 90);
  const mid = reduceRenderEvents(parseNdjsonLines('{"event":"progress","done":1,"total":4}\n'), null);
  expect('半途的进度是 0.25', mid.progress === 0.25);
  expect('没有事件时进度是未知(null)而不是 0', reduceRenderEvents([], null).progress === null);
  expect('start 之后进度是已知的 0', reduceRenderEvents([{ event: 'start' }], null).progress === 0);

  // ---- 状态形状 ----
  const probeJob = { job_id: 'j', state: 'queued', progress: null, download_url: null, error: null };
  const bare = jobStatus(probeJob);
  expect('可选字段不出现而不是塞 null', !('progress' in bare) && !('download_url' in bare) && !('error' in bare));
  probeJob.progress = 0.5;
  probeJob.download_url = '/export/j/download';
  const full = jobStatus(probeJob);
  expect('有值时才出现', full.progress === 0.5 && full.download_url === '/export/j/download');

  // ---- asset 解析 ----
  const emptyIndex = new Map();
  expect('id 不在索引里 -> 404', resolveAsset(emptyIndex, 'x', '/p').status === 404);
  expect('id 在但文件不在 -> 404 且理由不同', resolveAsset(
    new Map([['x', { id: 'x', file: 'target/definitely-not-here.mp4' }]]), 'x', '/p'
  ).error.code === 'asset_file_missing');

  // ---- 真索引（跑在真仓库上） ----
  const real = loadAssetIndex('target/s3');
  expect('真索引能从工程文件里读出 a.mp4', real.has('a.mp4'));
  expect('真索引里没有不存在的 id', !real.has('nope.mp4'));

  console.log('OK 本机后端自检通过（' + passed + ' 条断言）');
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) { runSelfTest(); return; }

  const value = (name, fallback) => {
    const index = argv.indexOf(name);
    return index >= 0 ? argv[index + 1] : fallback;
  };
  const port = Number(value('--port', '0'));
  const assetRoot = value('--asset-root', join('target', 's3'));
  const explicitCli = value('--cli', null);
  const cli = findCli(explicitCli, process.env);
  const assets = loadAssetIndex(assetRoot);
  const backend = createBackend();
  const context = { backend: backend, cli: cli, assetRoot: assetRoot, assets: assets };

  const server = createServer((req, res) => {
    try {
      handle(req, res, context, new URL(req.url, 'http://127.0.0.1'));
    } catch (error) {
      sendJson(res, 500, { error: issue('internal', req.url, String(error && error.message ? error.message : error)) });
    }
  });
  await new Promise((resolveListen) => server.listen(port, '127.0.0.1', resolveListen));
  const actual = server.address().port;
  console.log('本机后端：http://127.0.0.1:' + actual);
  console.log('资产根：' + assetRoot + '（索引 ' + assets.size + ' 条）');
  if (cli === null) {
    console.log('⚠ 没找到 dhampir 可执行文件：/validate 与 /export 会返回 503。');
    console.log('  先跑：cargo build -p dhampir-worker --bin dhampir');
  } else {
    console.log('渲染器：' + cli);
  }
}

main().catch((error) => {
  console.error('本机后端起不来：' + String(error && error.message ? error.message : error));
  process.exitCode = 1;
});
