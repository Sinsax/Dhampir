#!/usr/bin/env node
// 同源代理那条路的守卫。
//
// 为什么需要它：`--serve` / `--local` 时，页面由本进程服务、后端在另一个端口。
// 浏览器访问的每一个后端地址都要**经本进程转发**。这条转发有两个不对称的失效模式：
//
//   1. **漏路由**：转发用白名单，后端加了新路由而这里没跟 -> 静默 404。
//      本轮真的踩到：`/export/<jobId>`（轮询）与 `/export/<jobId>/download` 都没在名单里，
//      页面的表现是「导出失败：查询失败：HTTP 404」。
//   2. **提前收摊**：页面服务在报告（下载）之前就 close 了。
//      同源模式下页面把后端的相对下载地址按**自己的来源**解析，也就是解析到本服务上；
//      先 close 再去下载拿到的是 ECONNREFUSED —— 那个报错看着像「后端没起」，方向是错的。
//
// 两条都**不会让任何既有守卫变红**：`check-local-backend.mjs` 直接打后端、不经过转发，
// 而 `web-check --local`（唯一能抓到它们的入口）不在守卫清单里。
// 所以补这一条，把两个不变式钉住。
//
// 这个守卫**不起服务、不进浏览器**：它读 `scripts/web-check.mjs` 的源码，
// 判的是「路由与收摊的**结构**」。结构判据才拦得住"下次又写成白名单"。
//
// 用法：
//   node scripts/check-same-origin-proxy.mjs             检查
//   node scripts/check-same-origin-proxy.mjs --self-test 只跑守卫自检

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
// 判据的**实现**也拿过来测：文本判据拦得住"又写成白名单"的形状，
// 但拦不住"insideWebDir 自己写错了"。越界读文件那类问题只能靠行为用例。
import { insideWebDir } from './web-static.mjs';

const REPO_ROOT = join(import.meta.dirname, '..');
const TARGET = join(REPO_ROOT, 'scripts', 'web-check.mjs');

/** 静态判定：文件在不在，而不是路径在不在名单里。 */
const STATIC_CHECK = 'existsSync(file) && statSync(file).isFile()';
/** 转发后端的那道门。 */
const FORWARD_GATE = 'if (backendPort !== 0) {';
/** 页面自己的静态文件：发出去就结束，不该再往下走到转发。 */
const STATIC_SERVE = 'createReadStream(file).pipe(res);';
const BACKEND_KILL = 'backendProcess.kill()';
const SERVER_CLOSE = 'server.close();';
/** 静态路由：web/ **整个目录**按存在性服务，不是逐文件列举。 */
const WHOLE_DIR = 'insideWebDir(WEB_DIR, path)';
/**
 * 曾经逐条列举过的那两行。它们回来了就说明"整目录"那条规则被拆掉了。
 * 判据抓的是**这个形状**：本轮加 `web/app.css` 时，正是因为静态段在逐文件列举，
 * 样式表被转给后端拿回 404 —— 页面照常渲染、只是完全没有样式。
 */
const PER_FILE_WHITELISTS = ["path === '/app.js'", "path.startsWith('/export/')"];
/**
 * 响应头**整份转发**的标志。原来这里是四条头的白名单，
 * 后果与请求侧的白名单一模一样：后端加一个响应头就静默丢掉
 * （本轮：`x-dhampir-frame` 到不了页面，页面的校验反而报"后端回的是另一帧"）。
 * **同一个毛病出现第二次了**，所以这次钉的是"整份"，不是再补一条名单。
 */
const HEADER_PASSTHROUGH = 'upstream.headers.forEach(';
/** 曾经那四条的白名单写法。回来了就说明又退回去了。 */
const HEADER_WHITELIST = "for (const name of ['content-type', 'content-length', 'content-range', 'accept-ranges'])";

/**
 * 判据。**吃源码文本而不是读文件** —— 这样自检能喂一份故意写坏的进来，
 * 验它真的会红；否则这个守卫完全可能是空转的。
 */
export function judge(source) {
  const problems = [];
  const at = (needle) => source.indexOf(needle);

  // ---- 1. 转发**不许**是路径白名单 ----
  if (/\bbackendOwned\b/.test(source)) {
    problems.push('转发又变回路径白名单了（backendOwned）—— 后端加一条路由就会静默 404，'
      + '而症状是「导出失败：查询失败：HTTP 404」，看不出是路由没转');
  }

  const staticAt = at(STATIC_CHECK);
  const forwardAt = at(FORWARD_GATE);
  const serveAt = at(STATIC_SERVE);
  const killAt = at(BACKEND_KILL);
  const closeAt = at(SERVER_CLOSE);

  if (staticAt < 0) problems.push('找不到「静态文件按存在性判定」那一步（' + STATIC_CHECK + '）');
  if (forwardAt < 0) problems.push('找不到转发后端的那道门（' + FORWARD_GATE + '）');
  if (serveAt < 0) problems.push('找不到静态文件的服务语句（' + STATIC_SERVE + '）');
  if (killAt < 0) problems.push('找不到后端收摊（' + BACKEND_KILL + '）');
  if (closeAt < 0) problems.push('找不到页面服务收摊（' + SERVER_CLOSE + '）');

  if (problems.length > 0) return problems;

  // ---- 2. 顺序：先判静态、发完就 return，再轮到后端 ----
  if (!(staticAt < forwardAt)) {
    problems.push('静态文件判定没有排在转发之前 —— 顺序反了会把页面自己的文件也转给后端');
  }
  if (!(serveAt < forwardAt)) {
    problems.push('静态文件的服务语句没有排在转发之前');
  }
  // 静态那一段必须**自己结束**（return），否则会继续往下把同一个响应再写一遍。
  if (serveAt >= 0 && forwardAt > serveAt) {
    const between = source.slice(serveAt, forwardAt);
    if (!/\breturn;/.test(between)) {
      problems.push('静态文件服务之后没有 return —— 同一个响应会被转发那一段再写一次');
    }
  }

  // ---- 3. 静态路由是「web/ 整个目录」，不是逐文件列举 ----
  if (at(WHOLE_DIR) < 0) {
    problems.push('静态路由没走「web/ 整目录」那条规则（' + WHOLE_DIR + '）—— '
      + '逐文件列举的后果是每加一个前端文件都要记得回来改，忘了就是静默 404');
  }
  for (const needle of PER_FILE_WHITELISTS) {
    if (source.includes(needle)) {
      problems.push('静态段又在逐条列举 web 文件了（' + needle + '）—— 这正是'
        + '「加了 web/app.css 却拿回 404、页面完全没有样式」那个故障的形状');
    }
  }

  // ---- 4. 响应头整份转发，不是白名单 ----
  if (at(HEADER_PASSTHROUGH) < 0) {
    problems.push('代理没有整份转发后端响应头（' + HEADER_PASSTHROUGH + '）—— '
      + '白名单漏一个头就是静默失效，而症状与真正的原因毫无关系');
  }
  if (source.includes(HEADER_WHITELIST)) {
    problems.push('代理的响应头又变回白名单了 —— 后端新加的响应头会被静默丢掉'
      + '（本轮：x-dhampir-frame 到不了页面，页面据此判出「后端回的是另一帧」）');
  }

  // ---- 5. 页面服务必须活到报告（下载）结束 ----
  // 收摊要在后端收摊之后：报告那一段正是在后端收摊前跑的。
  if (!(killAt < closeAt)) {
    problems.push('页面服务在报告（下载产物）之前就 close 了 —— 同源模式下下载地址'
      + '解析到本服务上，先关再下载会拿到 ECONNREFUSED');
  }

  return problems;
}

function runSelfTest() {
  let passed = 0;
  const real = readFileSync(TARGET, 'utf8');
  const expect = (name, count, source) => {
    const problems = judge(source);
    if (problems.length !== count) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };

  // 反向用例只要求**至少红一条**：一个坏改动常常同时踩两条判据
  // （改了顺序通常连「发完就 return」也一起破坏），
  // 硬要求恰好一条会把「抓到了」误判成「自检失败」。
  const atLeast = (name, source) => {
    const problems = judge(source);
    if (problems.length < 1) throw new Error('自检失败：' + name + '（一条都没红）');
    passed += 1;
  };
  const expectTrue = (name, condition) => {
    if (condition !== true) throw new Error('自检失败：' + name);
    passed += 1;
  };

  // 真的源码应当通过。
  expect('真的实现通过', 0, real);

  // 每条判据各来一个反向用例 —— 抠掉它必须变红，否则它在空转。
  // 1) 转发又写成白名单（本轮的真实故障）。
  atLeast('转发改成白名单 -> 抓住',
    real.replace(FORWARD_GATE, "  const backendOwned = path === '/export';\n" + FORWARD_GATE));

  // 2) 静态判定没了。
  atLeast('静态判定没了 -> 抓住', real.replace(STATIC_CHECK, 'true'));

  // 3) 静态发完不 return：同一个响应会被转发那段再写一遍。
  atLeast('静态发完不 return -> 抓住',
    real.replace('    ' + STATIC_SERVE + '\n    return;', '    ' + STATIC_SERVE));

  // 4) 顺序反了：把转发那道门挪到最前面，静态判定就排到它后面了。
  atLeast('静态判定排到转发后面 -> 抓住', (() => {
    const gateAt = real.indexOf(FORWARD_GATE);
    return FORWARD_GATE + '\n' + real.slice(0, gateAt) + real.slice(gateAt + FORWARD_GATE.length);
  })());

  // 5) 页面服务提前收摊（本轮的真实故障）。
  atLeast('页面服务提前收摊 -> 抓住',
    real.replace(BACKEND_KILL, SERVER_CLOSE + ' ' + BACKEND_KILL));

  // 6) 静态段退回逐文件列举（本轮的真实故障：加了 web/app.css 却 404）。
  atLeast('静态段退回逐文件列举 -> 抓住',
    real.replace(WHOLE_DIR, "path === '/app.js' ? join(WEB_DIR, 'app.js') : null"));

  // 7) 响应头退回白名单（本轮的真实故障：x-dhampir-frame 被丢掉）。
  atLeast('响应头退回白名单 -> 抓住',
    real.replace(HEADER_PASSTHROUGH,
      '/* 白名单回来了 */ ' + HEADER_WHITELIST + ' void ('));

  // ---- 静态路由的**行为**：越界必须挡住，正常路径必须放行 ----
  // 文本判据挡不住"insideWebDir 自己写错了"，而它写错的后果是**读到仓库外面**
  // （页面看起来完全正常）。所以这几条必须真的调它。
  const webDir = join(REPO_ROOT, 'web');
  expectTrue('正常文件放行', insideWebDir(webDir, '/app.css') === join(webDir, 'app.css'));
  expectTrue('子目录放行', insideWebDir(webDir, '/export/http.js') === join(webDir, 'export', 'http.js'));
  expectTrue('空路径挡住', insideWebDir(webDir, '') === null);
  expectTrue('根路径不放行（它由上面单独处理）', insideWebDir(webDir, '/') === null);
  expectTrue('上一级挡住', insideWebDir(webDir, '/../package.json') === null);
  expectTrue('深层越界挡住', insideWebDir(webDir, '/../../etc/passwd') === null);
  expectTrue('NUL 挡住', insideWebDir(webDir, '/app.js' + String.fromCharCode(0) + '.png') === null);
  // 前缀相同的兄弟目录**不是**子目录 —— startsWith(WEB_DIR) 那种写法会在这里放行。
  expectTrue('前缀兄弟目录挡住', insideWebDir(webDir, '/../web-evil/secret.js') === null);

  console.log('✓ 同源代理守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }
  const source = readFileSync(TARGET, 'utf8');
  const problems = judge(source);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 同源代理的路由与收摊不满足约定');
    process.exitCode = 1;
    return;
  }
  console.log('✓ 同源代理成立（转发按存在性而非白名单、静态优先且先 return、页面服务活到下载结束）');
}

main();
