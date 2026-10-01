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
import { dirname, extname, isAbsolute, join, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

/** 工程文件放哪。路由 /projects/<id> 就是 fixtures/<id>.json。 */
const PROJECTS_DIR = join(REPO_ROOT, 'fixtures');
/** 兜底登记表。工程文件的 assets 优先于它。 */
const FALLBACK_REGISTRY = join(PROJECTS_DIR, 'local-assets.json');
/** 出片产物与临时文件都放 target/（已忽略），仓库里不留垃圾。 */
const EXPORT_DIR = join(REPO_ROOT, 'target', 'p6', 'local-exports');
const TMP_DIR = join(REPO_ROOT, 'target', 'p6', 'local-tmp');
/** 单帧 PNG 的落地目录。也是 target/ 下的：它是中间产物，不是交付物。 */
const FRAME_DIR = join(REPO_ROOT, 'target', 'p6', 'local-frames');

const MIME = {
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
  '.wav': 'audio/wav',
  '.m4a': 'audio/mp4',
  '.json': 'application/json; charset=utf-8',
  // 字幕是**文本**：页面 fetch 它再交给 Rust 解析。不给类型的话响应头会说
  // application/octet-stream —— fetch.text() 照样能用，但"头里说的是什么"
  // 是查问题时的一条线索，说错了就少一条。
  '.srt': 'text/plain; charset=utf-8',
  '.ass': 'text/plain; charset=utf-8',
  '.ssa': 'text/plain; charset=utf-8',
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
 * 判定素材 `uri` 是不是「绝对位置」。**按书写形态判，不按当前平台判。**
 *
 * `path.isAbsolute()` 是平台语义：Windows 上 `C:/x` 是绝对的，Linux 上它只是
 * 一个普通相对路径（`C:` 是个普通目录名）。工程文件是跨宿主走的，所以同一份 uri
 * 在两个宿主上必须得到**同一个答案** —— 否则 Windows 上写的 `C:/abs/b.mp4` 在
 * Linux 出片时会被挂到 assetRoot 下面，变成 `target/s3/C:/abs/b.mp4`。
 *
 * 规则与 Rust 侧 `is_absolute_uri`（crates/dhampir-worker/src/bin/dhampir.rs）逐条对应，
 * **不许只改一边**：
 * 1. 当前平台的绝对路径（`path.isAbsolute`，覆盖 POSIX 的 `/…`）；
 * 2. 盘符绝对：字母 + ':' + 紧跟 '/' 或 '\\'（`C:foo` 是盘符相对，Windows 也不认它绝对）；
 * 3. UNC：以两个反斜杠开头（Linux 上它只是一个普通组件，只能看文本）。
 */
export function isAbsoluteUri(uri) {
  if (typeof uri !== 'string' || uri.length === 0) return false;
  if (isAbsolute(uri)) return true;
  if (/^[A-Za-z]:[\\/]/.test(uri)) return true;
  return uri.startsWith('\\\\');
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
      file: isAbsoluteUri(uri) ? uri : join(assetRoot, uri),
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

/**
 * 从 Rust 源码里读一个 `pub const NAME: u32 = N;`。
 *
 * **读源码而不是抄一份常量表**：这份能力声明已经漂过一次（v2 → v3 时没跟上，
 * 于是浏览器在真正出片之前就被自己的预检拦住，而错误信息只说"对端只认 v3"，
 * 与真正的原因"这里写死了"离得很远）。再抄一份的话，下一次升版本还会这样。
 *
 * 读不到返回 null —— 调用方**如实失败**，不猜一个数。
 */
function readRustConst(relativePath, name) {
  try {
    const source = readFileSync(join(REPO_ROOT, relativePath), 'utf8');
    const pattern = new RegExp('pub const ' + name + ':\\s*u32\\s*=\\s*(\\d+)');
    const found = source.match(pattern);
    return found === null ? null : Number(found[1]);
  } catch (error) {
    return null;
  }
}

/** `dhampir-core` 登记表里的特效名。**它就是"实现了几种特效"的唯一来源。** */
function readEffectKinds() {
  try {
    const source = readFileSync(join(REPO_ROOT, 'crates', 'dhampir-core', 'src', 'effects.rs'), 'utf8');
    const block = source.match(/pub const REGISTRY[^=]*=\s*&\[([^\]]*)\]/);
    if (block === null) return null;
    // REGISTRY 里写的是常量**名**（GAUSSIAN_BLUR 之类），逐个回查它们的 kind 字符串。
    const names = block[1].split(',').map((item) => item.trim()).filter((item) => item.length > 0);
    const kinds = [];
    for (const name of names) {
      const spec = source.match(new RegExp('pub const ' + name + ':\\s*EffectSpec\\s*=\\s*EffectSpec\\s*\\{[^}]*?kind:\\s*"([^"]+)"'));
      if (spec === null) return null;
      kinds.push(spec[1]);
    }
    return kinds.length > 0 ? kinds : null;
  } catch (error) {
    return null;
  }
}

/** 能力声明。**从"本实现实际能做到什么"出发**，不抄一份好看的清单。 */
export function capabilities() {
  // **契约版本必须跟着 Rust 走。** 两处写死过：一次是 2（v3 时代），
  // 一次是 3（现在是 v4）—— 两次的表现都是"预检拦下了一次本来能成功的导出"，
  // 而预检本身是对的（它只转达对端声明），错的是这份声明。
  const contractVersion = readRustConst(join('crates', 'dhampir-timeline', 'src', 'layer.rs'), 'LAYER_SCHEMA_VERSION');
  const blurMax = readRustConst(join('crates', 'dhampir-core', 'src', 'render', 'blur.rs'), 'MAX_RADIUS');
  const effects = readEffectKinds();
  return {
    // 读不到就给空数组：**空数组会让预检拦下每一次导出**，那是响的；
    // 猜一个版本号则是静默地放行一份可能读不懂的工程 —— 后者更坏。
    timeline_versions: contractVersion === null ? [] : [contractVersion],
    // 混合模式只列实现的那四种 —— 与 BlendMode::is_implemented() 同一事实。
    blend_modes: ['normal', 'add', 'multiply', 'screen'],
    effects: effects === null ? [] : effects,
    max_blur_radius: blurMax === null ? 0 : blurMax,
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

/**
 * 判定回传的记账。
 *
 * **页面主动回传，而不是驱动钻进页面里取。** CDP 的 Runtime.evaluate（awaitPromise
 * 与 returnByValue 同用时）在本机 Chrome 上会给回空对象 —— 于是一个返回空对象的诊断
 * 工具，看起来和什么都没发生一模一样。让页面把判定 POST 到这里，驱动只读这里：
 * 拿不到就是**没拿到**，不会伪装成通过。
 */
export const MAX_VERDICTS = 64;

export function createVerdictStore() {
  return { seq: 0, items: [] };
}

/** 记一条判定。返回它带上的序号（驱动靠序号判断这是新的还是上一轮的）。 */
export function recordVerdict(store, name, value, at) {
  if (typeof name !== 'string' || name === '') return null;
  store.seq += 1;
  store.items.push({ seq: store.seq, name: name, value: value, at: at });
  while (store.items.length > MAX_VERDICTS) store.items.shift();
  return store.seq;
}

/** 取判定：name 为 null 表示全部；只取序号大于 after 的那些。 */
export function readVerdicts(store, name, after) {
  const floor = Number.isInteger(after) ? after : 0;
  return store.items.filter((item) => item.seq > floor && (name === null || item.name === name));
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
  const { backend, cli, assetRoot, assets, verdict } = context;

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

  // 素材库清点：**问 Rust**，不在这里数引用。
  // "引用了没有"只有一份实现（asset_reference_counts），前端只是把它画出来。
  const library = path.match(/^\/projects\/([A-Za-z0-9_.-]+)\/library$/);
  if (req.method === 'GET' && library) {
    if (cli === null) {
      return sendJson(res, 503, { error: issue('cli_missing', path, '没找到 dhampir 可执行文件') });
    }
    const file = join(PROJECTS_DIR, library[1] + '.json');
    if (!existsSync(file)) {
      return sendJson(res, 404, { error: issue('no_such_project', path, '没有这份工程：' + library[1]) });
    }
    return void runCli(cli, ['library', '--project', file], 60000).then((result) => {
      if (result.code !== 0) {
        return sendJson(res, 500, {
          error: issue('library_failed', path, (result.stderr || '').trim() || ('dhampir library 退出 ' + result.code)),
        });
      }
      try {
        return sendJson(res, 200, JSON.parse(result.stdout));
      } catch (error) {
        return sendJson(res, 500, { error: issue('library_bad_json', path, '清点器没给出 JSON：' + error.message) });
      }
    });
  }

  // 登记一个素材：**只接受资产根目录下面的文件**。
  //
  // 这是一个本机参考实现，不是上传服务：真正的上传（对象存储、账号、配额）属下游。
  // 但"路径必须落在资产根下面"这条要守住 —— 否则任何一个页面都能让本机后端
  // 把任意路径登记进工程，而那句"位置由宿主解释"就变成了"位置由网页解释"。
  if (req.method === 'POST' && path === '/assets') {
    if (cli === null) {
      return sendJson(res, 503, { error: issue('cli_missing', path, '没找到 dhampir 可执行文件') });
    }
    return readBody(req, (body) => {
      let request = null;
      try { request = JSON.parse(body.toString('utf8')); } catch (error) {
        return sendJson(res, 400, { error: issue('bad_request', path, '请求体不是 JSON：' + error.message) });
      }
      const projectId = typeof request.project === 'string' ? request.project : null;
      const wanted = typeof request.path === 'string' ? request.path : null;
      if (projectId === null || wanted === null) {
        return sendJson(res, 400, { error: issue('bad_request', path, '需要 {project, path}') });
      }
      const file = join(PROJECTS_DIR, projectId + '.json');
      if (!existsSync(file)) {
        return sendJson(res, 404, { error: issue('no_such_project', path, '没有这份工程：' + projectId) });
      }
      const resolvedRoot = resolve(assetRoot);
      const resolvedWanted = resolve(REPO_ROOT, wanted);
      if (resolvedWanted !== resolvedRoot && !resolvedWanted.startsWith(resolvedRoot + sep)) {
        return sendJson(res, 400, {
          error: issue('path_outside_asset_root', path,
            '只登记资产根（' + assetRoot + '）下面的文件，收到：' + wanted),
        });
      }
      if (!existsSync(resolvedWanted)) {
        return sendJson(res, 404, { error: issue('asset_file_missing', path, '文件不在：' + wanted) });
      }
      const cliArgs = ['import', '--project', file, '--file', resolvedWanted,
        '--asset-root', assetRoot, '--write'];
      if (typeof request.id === 'string' && request.id.length > 0) cliArgs.push('--id', request.id);
      if (request.replace === true) cliArgs.push('--replace');
      return void runCli(cli, cliArgs, 120000).then((result) => {
        try {
          return sendJson(res, result.code === 0 ? 200 : 400, JSON.parse(result.stdout));
        } catch (error) {
          return sendJson(res, 500, {
            error: issue('import_failed', path,
              (result.stderr || '').trim() || ('dhampir import 退出 ' + result.code)),
          });
        }
      });
    });
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

  // 出一帧**真实出片帧**（PNG）。
  //
  // 这是"预览与出片给出可比的帧"那条承诺的兑现口：页面手上有一份工程
  // （可能刚编辑过、还没落盘），把它连同帧号发过来，这里走的是**与 render
  // 完全相同的那条 Rust 路径**（同一个 cmd_frame、同一份时间线实现），
  // 于是"画布上这一帧"与"这张 PNG"是同一份工程的两次渲染，可以逐像素比。
  //
  // 为什么不是 GET /projects/:id/frame/:n：那样只能渲染**磁盘上**那份工程，
  // 而"我刚拉完这一刀，出片会是什么样"恰恰是编辑中的人最想问的。
  // 请求形状与 /export 一致（工程在请求体里），调用方不必学两套。
  //
  // **尺寸不由这里决定**：用工程自己的 render_hints / sequence_size
  // （CLI 的 resolve_size）。理由是这样拿到的就是**出片尺寸** ——
  // 想比"预览与出片一不一致"的人，要的正是出片那一份，而不是预览画布那一份。
  if (req.method === 'POST' && path === '/frame') {
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
      // 帧号必须是**非负整数**。收小数的话它会被静默取整，
      // 而"我要第 12.7 帧"变成第 12 帧而没有任何人说出来 —— 帧号是契约单位。
      if (!Number.isInteger(parsed.frame) || parsed.frame < 0) {
        return sendJson(res, 400, {
          error: issue('bad_frame', 'frame', 'frame 要是非负整数：' + String(parsed.frame)),
        });
      }
      const frame = parsed.frame;

      mkdirSync(FRAME_DIR, { recursive: true });
      mkdirSync(TMP_DIR, { recursive: true });
      const stamp = Date.now() + '-' + process.pid + '-' + frame;
      const temporary = join(TMP_DIR, 'frame-' + stamp + '.json');
      const outDir = join(FRAME_DIR, stamp);
      mkdirSync(outDir, { recursive: true });
      writeFileSync(temporary, JSON.stringify(project), 'utf8');

      const args = ['frame', '--project', temporary, '--frame', String(frame),
        '--out', outDir, '--asset-root', assetRoot];
      // 兜底登记表：**只对工程文件没登记的 id 生效**（CLI 侧同样是工程文件优先）。
      if (existsSync(FALLBACK_REGISTRY)) args.push('--asset-map', FALLBACK_REGISTRY);

      const cleanup = () => {
        try { rmSync(temporary, { force: true }); } catch (error) { /* 清不掉就算了 */ }
        try { rmSync(outDir, { recursive: true, force: true }); } catch (error) { /* 同上 */ }
      };

      return void runCli(cli, args, 120000).then((result) => {
        if (result.code !== 0) {
          cleanup();
          return sendJson(res, 500, {
            error: issue('frame_failed', path,
              (result.stderr || '').trim() || ('dhampir frame 退出 ' + result.code)),
          });
        }
        // **产物路径从 stdout 读**，不自己拼文件名。名字是 CLI 的事
        // （今天叫 frame-0012.png），在这边再拼一份就是第二份实现，改一天就漂。
        let produced = null;
        try {
          const meta = JSON.parse(result.stdout);
          if (typeof meta.path === 'string') produced = resolve(REPO_ROOT, meta.path);
        } catch (error) { produced = null; }
        if (produced === null || !existsSync(produced)) {
          // **不假装**：报成功但给不出产物，就是失败，退回"目录里唯一一张图"去猜更坏。
          cleanup();
          return sendJson(res, 500, {
            error: issue('frame_no_output', path, 'dhampir frame 报成功，但 stdout 里没有可读的产物路径'),
          });
        }
        const bytes = readFileSync(produced);
        cleanup();
        res.writeHead(200, {
          'content-type': 'image/png',
          'content-length': String(bytes.length),
          // 帧号回在头里：调用方据此确认拿到的**就是**自己点的那一帧，
          // 而不是"看起来像"。
          'x-dhampir-frame': String(frame),
          'cache-control': 'no-store',
        });
        res.end(bytes);
        return undefined;
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

  // ---- 判定回传 ----
  if (path === '/verdict') {
    if (req.method === 'GET') {
      const name = url.searchParams.get('name');
      const after = Number(url.searchParams.get('after'));
      const items = readVerdicts(verdict, name, Number.isInteger(after) ? after : 0);
      return sendJson(res, 200, { seq: verdict.seq, items: items });
    }
    if (req.method === 'POST') {
      return readBody(req, (body) => {
        let request = null;
        try { request = JSON.parse(body.toString('utf8')); } catch (error) {
          return sendJson(res, 400, { error: issue('bad_request', path, '请求体不是 JSON：' + error.message) });
        }
        const name = typeof request.name === 'string' ? request.name : null;
        if (name === null || name === '') {
          return sendJson(res, 400, { error: issue('bad_request', path, '需要 {name, value}') });
        }
        const seq = recordVerdict(verdict, name, request.value === undefined ? null : request.value, Date.now());
        return sendJson(res, 200, { seq: seq });
      });
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
  // ---- 「绝对」按**书写形态**判，不按平台判（Linux 上 path.isAbsolute('C:/…') 是 false）----
  const shown = (assets, id) => parseAssetIndex(
    [JSON.stringify({ project_schema: 1, assets })], '', 'x'
  ).get(id).file.replaceAll('\\', '/');
  expect('盘符绝对原样（正斜杠）', shown([{ id: 'c', uri: 'C:/abs/c.mp4' }], 'c') === 'C:/abs/c.mp4');
  expect('盘符绝对原样（反斜杠）', shown([{ id: 'd', uri: 'D:\\abs\\d.mp4' }], 'd') === 'D:/abs/d.mp4');
  expect('盘符大小写不敏感', shown([{ id: 'e', uri: 'c:/abs/e.mp4' }], 'e') === 'c:/abs/e.mp4');
  expect('UNC 原样', shown([{ id: 'g', uri: '\\\\server\\share\\g.mp4' }], 'g') === '//server/share/g.mp4');
  expect('盘符相对仍挂根（C:foo 不是绝对）', shown([{ id: 'f', uri: 'C:rel.mp4' }], 'f') === 'x/C:rel.mp4');

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

  // ---- 判定回传 ----
  const store = createVerdictStore();
  expect('新账本是空的', store.seq === 0 && readVerdicts(store, null, 0).length === 0);
  expect('记账返回递增序号', recordVerdict(store, 'trim-parity', { ok: true }, 1) === 1
    && recordVerdict(store, 'other', 0, 2) === 2);
  expect('按名字取只拿这一条', readVerdicts(store, 'trim-parity', 0).length === 1);
  expect('after 之后的才算新的', readVerdicts(store, null, 1).length === 1
    && readVerdicts(store, null, 2).length === 0);
  expect('名字为空不记账', recordVerdict(store, '', 1, 3) === null && store.seq === 2);
  expect('账本有上限（老条目会被挤掉，但序号只增不减）', (() => {
    for (let i = 0; i < MAX_VERDICTS + 5; i += 1) recordVerdict(store, 'flood', i, 4);
    return store.items.length === MAX_VERDICTS && store.seq === MAX_VERDICTS + 7;
  })());

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
  const context = {
    backend: backend,
    cli: cli,
    assetRoot: assetRoot,
    assets: assets,
    verdict: createVerdictStore(),
  };

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
