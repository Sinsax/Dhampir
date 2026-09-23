#!/usr/bin/env node
// web 层的不变量守卫。
//
// 四条不变量（plan §2）：
//   1. 没有任何 Rust crate 依赖 web/ —— 它是依赖图的叶子；
//   2. engine.js 不依赖任何前端框架 —— 下游换框架不用改它；
//   3. app.js 不自己实现业务规则 —— 校验/求值/渲染都来自 Rust；
//   4. 里程碑视频在，且非空。
//
// 用法：
//   node scripts/check-web-invariants.mjs             检查
//   node scripts/check-web-invariants.mjs --self-test 只跑守卫自检

import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// 引号用 fromCharCode 构造。直接写嵌套引号在两个方向上都会出事：
// 写进这个文件时要转义一层，读起来也分不清哪个是"这一段代码的引号"。
const SQ = String.fromCharCode(39);
const DQ = String.fromCharCode(34);

const FRAMEWORKS = ['react', 'vue', 'svelte', 'angular', 'jquery', 'lit', 'preact', 'solid-js'];

/** 前端框架/打包器的特征串。**词首匹配是故意保守的**：宁可误报也不放过。 */
export const FORBIDDEN_IN_ENGINE = []
  .concat(FRAMEWORKS.map((name) => 'from ' + DQ + name))
  .concat(FRAMEWORKS.map((name) => 'from ' + SQ + name))
  .concat(['vite', 'webpack', 'rollup']);

/** app.js 里不该出现的业务规则特征。校验只有一份实现，在 Rust 里。 */
export const FORBIDDEN_IN_APP = [
  'clip_overlap', 'duration_not_positive', 'unsupported_schema',
  'opacity_out_of_range', 'keyframe_out_of_clip', 'unknown_effect',
];

export function scanEngine(text) {
  const problems = [];
  for (const needle of FORBIDDEN_IN_ENGINE) {
    if (text.includes(needle)) problems.push('engine.js 不该出现前端框架/打包器特征：' + needle);
  }
  return problems;
}

export function scanApp(text) {
  const problems = [];
  for (const needle of FORBIDDEN_IN_APP) {
    if (text.includes(needle)) {
      problems.push('app.js 出现了业务规则特征「' + needle + '」——校验只该有一份实现（在 Rust 里）');
    }
  }
  return problems;
}

/** 一个目录（或文件）里最新的 mtime。目录递归。 */
export function newestMtime(path) {
  if (!existsSync(path)) return null;
  const stats = statSync(path);
  if (stats.isFile()) return stats.mtimeMs;
  let newest = stats.mtimeMs;
  for (const entry of readdirSync(path, { withFileTypes: true })) {
    const child = join(path, entry.name);
    const childNewest = newestMtime(child);
    if (childNewest !== null && childNewest > newest) newest = childNewest;
  }
  return newest;
}

/**
 * wasm pkg 比它的源码旧吗？
 *
 * # 为什么这条值得有守卫
 *
 * `crates/dhampir-wasm/www/pkg` 是 gitignore 的构建产物，**没有版本号、也没有校验**。
 * 改了 Rust 却没重建 pkg 时，浏览器拿旧 wasm 跑，于是：
 *
 *   * 新导出的函数不存在 -> "engine.doc is not a function"；
 *   * 或者更坏：新旧形状对不上，在 wasm 里撞一个 trap，
 *     页面上只有一句 "启动失败：unreachable executed"。
 *
 * 两种情况报的错**都与真正的原因（没重建）毫无关系**。这条把它变成一句能照做的话。
 */
export function scanStaleWasmPkg(pkgMtimeMs, newestSourceMs, pkgPath) {
  const problems = [];
  // pkg 不在就不判：它是构建产物，没构建过不是"不变量被破坏"。
  if (pkgMtimeMs === null || newestSourceMs === null) return problems;
  if (pkgMtimeMs < newestSourceMs) {
    problems.push(
      'wasm pkg 比它的源码旧（' + pkgPath + '）：页面会拿旧 wasm 跑，' +
      '报出来的错与原因毫无关系。重建：wasm-pack build crates/dhampir-wasm ' +
      '--target web --out-dir www/pkg --dev'
    );
  }
  return problems;
}

export function scanCrateDeps(root, manifests) {
  const problems = [];
  for (const manifest of manifests) {
    const path = join(root, manifest);
    if (!existsSync(path)) continue;
    for (const line of readFileSync(path, 'utf8').split('\n')) {
      const trimmed = line.trim();
      if (trimmed.startsWith('#')) continue;
      // web-sys / webgpu 是浏览器 API 绑定，不是"依赖 web/ 这个目录"，必须放过。
      const looksLikeWebDep = trimmed.includes('web =') || trimmed.includes('"web"') || trimmed.includes('web/');
      if (looksLikeWebDep && !trimmed.includes('web-sys') && !trimmed.includes('webgpu')) {
        problems.push(manifest + ' 里出现了对 web/ 的依赖：' + trimmed);
      }
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
  expect('干净的 engine 通过', scanEngine('export async function loadEngine(u) { return import(u); }').length === 0);
  expect('双引号 import React 被抓', scanEngine('import React from ' + DQ + 'react' + DQ + ';').length > 0);
  expect('单引号 import Vue 被抓', scanEngine('import Vue from ' + SQ + 'vue' + SQ + ';').length > 0);
  expect('vite 被抓', scanEngine('// built with vite').length > 0);
  expect('干净的 app 通过', scanApp('const r = engine.validate(project);').length === 0);
  expect('pkg 不在 -> 不判', scanStaleWasmPkg(null, 100, 'pkg.wasm').length === 0);
  expect('pkg 比源码新 -> 通过', scanStaleWasmPkg(200, 100, 'pkg.wasm').length === 0);
  expect('pkg 比源码旧 -> 必须红', scanStaleWasmPkg(100, 200, 'pkg.wasm').length === 1);
  expect('红的时候要给出重建命令', scanStaleWasmPkg(100, 200, 'pkg.wasm')[0].includes('wasm-pack build'));
  expect('业务规则被抓', scanApp('if (issue.code === ' + DQ + 'clip_overlap' + DQ + ') {}').length > 0);

  const dir = mkdtempSync(join(tmpdir(), 'dhampir-web-guard-'));
  try {
    mkdirSync(join(dir, 'crates', 'a'), { recursive: true });
    const manifest = join(dir, 'crates', 'a', 'Cargo.toml');
    writeFileSync(manifest, '[dependencies]\nweb-sys = ' + DQ + '0.3' + DQ + '\n');
    expect('web-sys 不算依赖 web/', scanCrateDeps('/', ['crates/a/Cargo.toml']).length === 0);
    writeFileSync(manifest, '[dependencies]\nweb = { path = ' + DQ + '../../web' + DQ + ' }\n');
    expect('真依赖 web/ 被抓', scanCrateDeps(dir, ['crates/a/Cargo.toml']).length > 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
  console.log('✓ web 守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }
  const problems = [];

  const enginePath = join(REPO_ROOT, 'web', 'engine.js');
  const appPath = join(REPO_ROOT, 'web', 'app.js');
  if (!existsSync(enginePath)) problems.push('缺少 web/engine.js');
  else problems.push(...scanEngine(readFileSync(enginePath, 'utf8')));
  if (!existsSync(appPath)) problems.push('缺少 web/app.js');
  else problems.push(...scanApp(readFileSync(appPath, 'utf8')));

  const cratesDir = join(REPO_ROOT, 'crates');
  const manifests = [];
  if (existsSync(cratesDir)) {
    for (const entry of readdirSync(cratesDir)) {
      const relative = join('crates', entry, 'Cargo.toml');
      if (existsSync(join(REPO_ROOT, relative))) manifests.push(relative);
    }
  }
  problems.push(...scanCrateDeps(REPO_ROOT, manifests));

  const video = join(REPO_ROOT, 'milestones', 'edited-milestone.mp4');
  if (!existsSync(video)) problems.push('缺少里程碑文件 milestones/edited-milestone.mp4');
  else if (!(statSync(video).size > 0)) problems.push('里程碑文件是空的');

  // ---- wasm pkg 是不是旧的 ----
  // 这一条**在这轮之前不存在**，而它正是让"启动失败：unreachable executed"
  // 这种错看起来毫无头绪的原因之一：拿旧 wasm 跑新前端。
  const pkgWasm = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www', 'pkg', 'dhampir_wasm_bg.wasm');
  let pkgMtime = null;
  try { pkgMtime = statSync(pkgWasm).mtimeMs; } catch (error) { pkgMtime = null; }
  const newestSource = [
    join(REPO_ROOT, 'crates', 'dhampir-wasm', 'src'),
    join(REPO_ROOT, 'crates', 'dhampir-core', 'src'),
    join(REPO_ROOT, 'crates', 'dhampir-timeline', 'src'),
    join(REPO_ROOT, 'Cargo.toml'),
  ].map(newestMtime).filter((value) => value !== null).reduce((a, b) => Math.max(a, b), 0);
  problems.push(...scanStaleWasmPkg(pkgMtime, newestSource === 0 ? null : newestSource, pkgWasm));

  if (existsSync(join(REPO_ROOT, 'web', 'node_modules'))) {
    problems.push('web/node_modules 存在——说明引了 npm 依赖，与「零构建」的约定冲突');
  }

  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ web 层不变量被破坏');
    process.exitCode = 1;
    return;
  }
  console.log('✓ web 层不变量成立（叶子依赖 / 引擎无框架 / 业务规则单份 / 里程碑在）');
}

main();
