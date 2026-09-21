// M0 浏览器宿主自检的本地服务：一个静态文件服务 + 一个「记录落盘口」。
//
// 为什么不是 `npx serve` 就完事：
//
//  1. `--target web` 的 wasm 胶水靠 `fetch` 取 .wasm，`file://` 下会被浏览器拒掉，
//     所以必须有一个真的 HTTP 源（MIME 也得对：.wasm 必须是 application/wasm）。
//  2. 页面里那些结论（golden 比对、浏览器渲染出的 PNG）如果只活在截图里，
//     事后没法复核。落盘口让它们变成 records/ 里的**文件**——截图可以骗人，
//     文件可以被人重新算一遍摘要。
//
// 故意不引入依赖：M0 的依赖越少，"跑不起来"的原因就越少。
//
// 用法：node scripts/serve-wasm-harness.mjs [--port 8787] [--out records/m0] [--self-test]
//
// 记录只会写到 `--out` 目录；PNG 会先验签名再落盘——一个签名不对的文件
// 混进 records/ 比没有这个文件更糟。

import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(HERE, '..');

/** 页面所在的目录。`pkg/` 是 `wasm-pack build --target web` 的产物（已在 .gitignore 里）。 */
const WWW_ROOT = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www');

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

/** 请求体上限。一份 256x256 的 PNG 约 9 KB，10 MB 已经很宽容了。 */
const MAX_BODY_BYTES = 10 * 1024 * 1024;

const MIME = new Map([
  ['.html', 'text/html; charset=utf-8'],
  ['.js', 'text/javascript; charset=utf-8'],
  ['.mjs', 'text/javascript; charset=utf-8'],
  ['.wasm', 'application/wasm'],
  ['.json', 'application/json; charset=utf-8'],
  ['.css', 'text/css; charset=utf-8'],
  ['.png', 'image/png'],
  ['.map', 'application/json; charset=utf-8'],
]);

const USAGE = `用法：node scripts/serve-wasm-harness.mjs [选项]

选项：
  --port <n>    监听端口（默认 8787，只绑 127.0.0.1）
  --out <dir>   记录目录（默认 records/m0）
  --self-test   只跑本脚本的自检，不启动服务
  -h, --help    显示本帮助

服务起来后打开打印出来的 URL；页面上的「运行自检」会把结论 POST 回本服务，
写进 <out>/browser-harness.json 与 <out>/probe-browser-webgpu.png。`;

// ---------------------------------------------------------------------------
// FNV-1a 64
// ---------------------------------------------------------------------------

/// FNV-1a 64 的 JS 版，与 `dhampir_timeline::fnv1a64` 同算法。
///
/// 为什么要在脚本里再写一遍：它用来算**期望值**（native 侧那份 PNG 的摘要），
/// 页面拿它做比对。JS 没有 u64，所以用 BigInt，并且每一步都掩到 64 位——
/// 少了这一步，`0xff..ff` 上的行为就和 Rust 不同，摘要会"看起来只是不一样"。
///
/// `--self-test` 用公开测试向量钉住这份实现，别让"两边算法不同"伪装成
/// "两端渲染不一致"。
function fnv1a64Hex(bytes) {
  const MASK = (1n << 64n) - 1n;
  const PRIME = 0x00000100000001b3n;
  let hash = 0xcbf29ce484222325n;
  for (const byte of bytes) {
    hash = (hash ^ BigInt(byte)) & MASK;
    hash = (hash * PRIME) & MASK;
  }
  return hash.toString(16).padStart(16, '0');
}

// ---------------------------------------------------------------------------
// 落盘口
// ---------------------------------------------------------------------------

/**
 * 校验页面 POST 上来的负载。
 *
 * 抽成纯函数是为了能被 `--self-test` 直接喂坏数据——"校验会不会拒绝"
 * 必须有证据，否则它只是一段看起来很严的代码。
 *
 * @returns {{ok: true, report: object, png: Buffer|null} | {ok: false, error: string}}
 */
export function validateRecord(body) {
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    return { ok: false, error: '负载不是一个 JSON 对象' };
  }
  const { report, png_base64: pngBase64 } = body;
  if (report === null || typeof report !== 'object' || Array.isArray(report)) {
    return { ok: false, error: '缺少 report 对象' };
  }
  if (report.milestone !== 'M0') {
    return {
      ok: false,
      error: `report.milestone 必须是 "M0"，收到 ${JSON.stringify(report.milestone)}`,
    };
  }
  if (typeof report.probe?.golden_check_passed !== 'boolean') {
    return { ok: false, error: 'report.probe.golden_check_passed 缺失或不是布尔值' };
  }

  if (pngBase64 === null || pngBase64 === undefined) {
    return { ok: true, report, png: null };
  }
  if (typeof pngBase64 !== 'string') {
    return { ok: false, error: 'png_base64 不是字符串' };
  }
  const png = Buffer.from(pngBase64, 'base64');
  if (png.length <= PNG_SIGNATURE.length) {
    return { ok: false, error: `解码后的 PNG 只有 ${png.length} 字节` };
  }
  if (!png.subarray(0, PNG_SIGNATURE.length).equals(PNG_SIGNATURE)) {
    return { ok: false, error: 'png_base64 解码后不是 PNG（签名不对）' };
  }
  return { ok: true, report, png };
}

/**
 * 把校验过的负载写进 `outDir`。
 *
 * 返回值是"写了哪些文件"的完整列表——页面会把它显示出来，
 * 于是"已落盘"这句话在截图里也带证据。
 */
export function writeRecord(outDir, { report, png }) {
  mkdirSync(outDir, { recursive: true });
  const files = [];

  // 时间戳进记录是有先例的（`adapter.json` 里也有），但**只能有它一个**非确定项。
  const payload = { ...report, received_unix_epoch_seconds: Math.floor(Date.now() / 1000) };
  const jsonPath = join(outDir, 'browser-harness.json');
  // JSON.stringify 的换行永远是 `\n`，与平台无关；显式补一个行尾，和别的记录文件一致。
  writeFileSync(jsonPath, `${JSON.stringify(payload, null, 2)}\n`, 'utf8');
  files.push(jsonPath);

  if (png) {
    const pngPath = join(outDir, 'probe-browser-webgpu.png');
    writeFileSync(pngPath, png);
    files.push(pngPath);
  }

  return files;
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

function readBody(request) {
  return new Promise((resolvePromise, rejectPromise) => {
    const chunks = [];
    let total = 0;
    request.on('data', (chunk) => {
      total += chunk.length;
      if (total > MAX_BODY_BYTES) {
        rejectPromise(new Error(`请求体超过 ${MAX_BODY_BYTES} 字节`));
        request.destroy();
        return;
      }
      chunks.push(chunk);
    });
    request.on('end', () => resolvePromise(Buffer.concat(chunks)));
    request.on('error', rejectPromise);
  });
}

function json(res, status, body) {
  const text = JSON.stringify(body);
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(text),
  });
  res.end(text);
}

/** 把 URL 路径映射到 www 目录内的真实文件；越界一律拒绝。 */
function resolveStatic(urlPath) {
  const decoded = decodeURIComponent(urlPath.split('?')[0]);
  const relative = decoded === '/' ? 'index.html' : decoded.replace(/^\/+/, '');
  const target = normalize(join(WWW_ROOT, relative));
  // 字符串前缀比较不够安全（`/www-evil` 也以 `/www` 开头是另一回事，
  // 但 `..` 归一化后必须仍然在 WWW_ROOT 之下——这里卡死它）。
  if (target !== WWW_ROOT && !target.startsWith(WWW_ROOT + sep)) {
    return null;
  }
  return target;
}

function serveStatic(res, target) {
  if (target === null || !existsSync(target) || !statSync(target).isFile()) {
    json(res, 404, { ok: false, error: '没有这个文件' });
    return;
  }
  const body = readFileSync(target);
  res.writeHead(200, {
    'content-type': MIME.get(extname(target)) ?? 'application/octet-stream',
    'content-length': body.length,
    // 开发用：别让浏览器缓存住旧的 wasm，否则"改了没生效"会浪费半天。
    'cache-control': 'no-store',
  });
  res.end(body);
}

async function handleRecord(request, res, outDir) {
  let body;
  try {
    body = JSON.parse((await readBody(request)).toString('utf8'));
  } catch (error) {
    json(res, 400, { ok: false, error: `请求体不是 JSON：${error.message}` });
    return;
  }

  const verdict = validateRecord(body);
  if (!verdict.ok) {
    console.error(`✗ 落盘被拒：${verdict.error}`);
    json(res, 400, { ok: false, error: verdict.error });
    return;
  }

  const files = writeRecord(outDir, verdict);
  const relative = files.map((file) => file.slice(REPO_ROOT.length + 1).split(sep).join('/'));
  console.log(`✓ 已落盘 ${relative.join('、')}`);
  json(res, 200, { ok: true, files: relative });
}

function startServer(port, outDir) {
  const server = createServer((request, res) => {
    if (request.method === 'POST' && request.url === '/__record') {
      handleRecord(request, res, outDir).catch((error) => {
        console.error(`✗ 落盘出错：${error.message}`);
        json(res, 500, { ok: false, error: error.message });
      });
      return;
    }
    if (request.method === 'GET' || request.method === 'HEAD') {
      serveStatic(res, resolveStatic(request.url ?? '/'));
      return;
    }
    json(res, 405, { ok: false, error: '只支持 GET / HEAD / POST /__record' });
  });

  server.on('error', (error) => {
    console.error(`✗ 起不来：${error.message}`);
    // 端口被占是最常见的失败，判死而不是"假装在跑"。
    process.exitCode = 2;
  });

  server.listen(port, '127.0.0.1', () => {
    console.log(`dhampir M0 浏览器宿主自检`);
    console.log(`  页面  ${WWW_ROOT}`);
    console.log(`  记录  ${outDir}`);
    console.log('');
    console.log(`  http://127.0.0.1:${port}/`);
  });
  return server;
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

function selfTest() {
  const checks = [];
  const check = (name, condition) => checks.push({ name, ok: Boolean(condition) });

  // ① 摘要实现必须与 Rust 侧（也是 FNV-1a 的公开向量）一致。
  check('fnv1a64("") == 偏移基数', fnv1a64Hex(Buffer.alloc(0)) === 'cbf29ce484222325');
  check('fnv1a64("a") == 公开向量', fnv1a64Hex(Buffer.from('a')) === 'af63dc4c8601ec8c');
  check('fnv1a64("foobar") == 公开向量', fnv1a64Hex(Buffer.from('foobar')) === '85944171f73967e8');
  check('fnv1a64 长度固定 16 位', fnv1a64Hex(Buffer.from([0xff, 0xff, 0xff])).length === 16);

  const goodReport = { milestone: 'M0', probe: { golden_check_passed: true } };
  const png = Buffer.concat([PNG_SIGNATURE, Buffer.from('fake')]);
  const goodPng = png.toString('base64');

  // ② 好负载必须过——校验不是恒红的。
  const accepted = validateRecord({ report: goodReport, png_base64: goodPng });
  check('好负载被接受', accepted.ok === true && accepted.png.length === png.length);

  // ③ 坏负载必须被拒，且每一条都要有自己的理由。
  const rejects = [
    ['null', null],
    ['数组', []],
    ['缺 report', { png_base64: null }],
    ['milestone 不是 M0', { report: { ...goodReport, milestone: 'M1' } }],
    ['没有 golden 结论', { report: { milestone: 'M0', probe: {} } }],
    ['PNG 不是字符串', { report: goodReport, png_base64: 123 }],
    ['PNG 太短', { report: goodReport, png_base64: Buffer.from([0x89]).toString('base64') }],
    [
      'PNG 签名不对',
      {
        report: goodReport,
        png_base64: Buffer.concat([Buffer.from('not a png!'), png]).toString('base64'),
      },
    ],
  ];
  for (const [name, body] of rejects) {
    const verdict = validateRecord(body);
    check(`拒绝：${name}`, verdict.ok === false && typeof verdict.error === 'string');
  }

  // ④ 落盘：写出来的 JSON 必须是 LF、且能被重新读回来。
  const tmpDir = join(REPO_ROOT, 'target', 'harness-selftest');
  const files = writeRecord(tmpDir, { report: goodReport, png });
  const text = readFileSync(files[0], 'utf8');
  check('落盘的 JSON 以 LF 结尾', text.endsWith('\n') && !text.includes('\r\n'));
  check('落盘的 JSON 能读回来', JSON.parse(text).milestone === 'M0');
  check('落盘的 PNG 字节相同', readFileSync(files[1]).equals(png));

  const failed = checks.filter((c) => !c.ok);
  for (const { name, ok } of checks) {
    console.log(`${ok ? '✓' : '✗'} ${name}`);
  }
  console.log(`\n${checks.length - failed.length}/${checks.length} 项通过`);
  return failed.length === 0 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

function main(argv) {
  let port = 8787;
  let outDir = join(REPO_ROOT, 'records', 'm0');
  let runSelfTest = false;

  const args = [...argv];
  while (args.length > 0) {
    const arg = args.shift();
    switch (arg) {
      case '--port': {
        const value = args.shift();
        if (value === undefined || !/^\d+$/.test(value)) {
          console.error(`✗ --port 后面要跟一个端口号，收到 ${JSON.stringify(value)}`);
          return 2;
        }
        port = Number(value);
        break;
      }
      case '--out': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') {
          console.error('✗ --out 后面要跟一个目录');
          return 2;
        }
        outDir = resolve(REPO_ROOT, value);
        break;
      }
      case '--self-test':
        runSelfTest = true;
        break;
      case '-h':
      case '--help':
        console.log(USAGE);
        return 0;
      default:
        // 不认识的参数一律判死：静默忽略参数是"看起来全绿"最常见的来源。
        console.error(`✗ 不认识的参数：${arg}\n\n${USAGE}`);
        return 2;
    }
  }

  if (runSelfTest) {
    return selfTest();
  }

  if (!existsSync(join(WWW_ROOT, 'index.html'))) {
    console.error(`✗ ${join(WWW_ROOT, 'index.html')} 不存在`);
    return 2;
  }
  if (!existsSync(join(WWW_ROOT, 'pkg', 'dhampir_wasm.js'))) {
    console.error(
      '✗ 还没打包 wasm。先跑：\n' +
        '    wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev',
    );
    return 2;
  }

  startServer(port, outDir);

  // 顺带把 native 侧那份 PNG 的摘要算出来，打印一个带 ?expect= 的地址。
  // 页面拿它做比对，"一致"因此不是页面在自说自话。摘要由 JS 的 FNV-1a 算出，
  // 而页面用的是 wasm 里 Rust 的那一份——两边的算法由公开向量各自钉着。
  const nativePng = join(outDir, 'probe-native-dx12.png');
  if (existsSync(nativePng)) {
    const digest = fnv1a64Hex(readFileSync(nativePng));
    console.log(`  http://127.0.0.1:${port}/?expect=${digest}`);
    console.log(`        （?expect 是 ${nativePng.slice(REPO_ROOT.length + 1)} 的 FNV-1a 摘要）`);
  }
  console.log('\n记录将写入 ' + outDir);
  return 0;
}

process.exitCode = main(process.argv.slice(2));

export { fnv1a64Hex };
