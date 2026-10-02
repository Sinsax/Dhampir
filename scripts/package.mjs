// 打**发布产物**：Dhampir 只出两样东西，下游宿主只吃这两样。
//
//     node scripts/package.mjs [--out <目录>] [--clean-wasm] [--no-zip] [--bundle-licenses]
//
// 产物形状（这就是与下游宿主的**全部**契约；下游那边的接入文档在 `docs/dhampir/`）：
//
//     <out>/
//       preview/
//         engine.js                      ← web/engine.js
//         pkg/dhampir_wasm.js            ← wasm-pack 输出
//         pkg/dhampir_wasm_bg.wasm
//       bin/
//         dhampir(.exe)                  ← release 二进制（出片用）
//       VERSION                          ← git sha + 构建时间 + **project_schema** + license
//       LICENSE                          ← LICENSE-APACHE（Apache-2.0 §4：分发要随附全文）
//       THIRD-PARTY-LICENSES.md          ← 生成的第三方清单（scripts/licenses.mjs）
//       licenses/                        ← 只有加 --bundle-licenses 时才在（每个依赖自带的文本）
//     <out>.zip
//
// # 为什么要有这个脚本
//
// 现在下游宿主是**指着本仓的源码树**跑的（`VTEDIT_DHAMPIR_WASM=<仓根>`，然后去读
// `web/` 与 `crates/dhampir-wasm/www/pkg/`）。那条路有两个已经踩到的坑：
//
//   1. 下游宿主得知道本仓的**内部目录结构** —— 这里一挪目录，那边就断；
//   2. 它读的是**活的源码树** —— "改了没生效 / 生效了又说不清是哪一版"，
//      实测遇到的是"改完必须强刷新"（记在下游仓 docs/dhampir/compare-loop.md）。
//
// 打成定版产物之后，下游宿主只需要知道**一个目录 + 一个版本号**。
//
// # `--clean-wasm` 是什么、为什么默认不开
//
// `cargo clean -p dhampir-wasm` **清不掉 wasm 目标的依赖** —— 它只清 host 目标，
// 于是依赖（`dhampir-core` / `dhampir-timeline`）的改动**进不去**，
// 而 `wasm-pack build` 会报 `Finished in 0.13s` **装作没事**（实测踩过）。
// 真清是 `cargo clean --target wasm32-unknown-unknown` —— 但它会把整个 wasm 目标
// 清掉（实测 41283 个文件 / 20 GiB，重建 29s），所以**默认不开**，需要时显式加。
//
// 另一条纪律：`cargo build` 与 `wasm-pack` 的输出**一律走 stdio: inherit** ——
// 在受限环境下捕获子进程管道会 EPERM，而那种失败看起来像"命令没跑"。

import { execFileSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] && !process.argv[i + 1].startsWith('--') ? process.argv[i + 1] : fallback;
}
const has = (name) => process.argv.includes(name);

const run = (cmd, args) => {
  console.log('  $ ' + cmd + ' ' + args.join(' '));
  // `DHAMPIR_GIT_SHA` 让产物能自报"是哪一次构建"（`dhampir_build_id` 读它）。
  // 为什么必须注入：`HOST_API_VERSION` 只在导出面变化时才 +1，一次纯实现修复
  // （例如 2026-10-02 的动图上传改数组纹理）**不改版本号** —— 没有这个 sha，
  // "浏览器里跑的是不是修好的那份"就无从判断（那次排查正是卡在这里）。
  execFileSync(cmd, args, { cwd: REPO, stdio: 'inherit', env: { ...process.env, DHAMPIR_GIT_SHA: sha } });
};

// ---------------------------------------------------------------- 版本信息
// **契约版本从 Rust 常量里读，不写死** —— 写死的话，改了契约而忘了改脚本，
//下游宿主会拿一个"看起来对"的版本号去放行一个不兼容的产物。
function projectSchemaVersion() {
  const src = readFileSync(join(REPO, 'crates/dhampir-timeline/src/project.rs'), 'utf8');
  const m = src.match(/pub const PROJECT_SCHEMA_VERSION:\s*u32\s*=\s*(\d+)\s*;/);
  if (!m) {
    console.error('✗ 读不出 PROJECT_SCHEMA_VERSION（在 crates/dhampir-timeline/src/project.rs）——');
    console.error('  它是产物与下游宿主之间的契约版本，读不到就**不许猜**，直接失败。');
    process.exit(1);
  }
  return Number(m[1]);
}


/// **宿主 API 版本**（`HOST_API_VERSION`，同一个来源：Rust 常量）。
///
/// 为什么要和 project_schema 分开记：两者是**两条独立的兼容线**。
/// project_schema 管工程文件能不能读；HOST_API_VERSION 管 wasm 导出面/形状对不对得上。
/// 动图这一版（v6）正是**只动了后者**：工程文件形状一个键都没变，
/// 但多了两条导出 —— 下游拿老版本号放行就会调到 `undefined`。
///
/// 同一条纪律：读不到就**失败**，不猜。猜出来的版本号比没有更坏。
function hostApiVersion() {
  const src = readFileSync(join(REPO, 'crates/dhampir-timeline/src/host_api.rs'), 'utf8');
  const m = src.match(/pub const HOST_API_VERSION:\s*u32\s*=\s*(\d+)\s*;/);
  if (!m) {
    console.error('✗ 读不出 HOST_API_VERSION（在 crates/dhampir-timeline/src/host_api.rs）——');
    console.error('  它是 wasm 导出面与下游宿主之间的契约版本，读不到就**不许猜**，直接失败。');
    process.exit(1);
  }
  return Number(m[1]);
}

function gitSha() {
  try {
    return execFileSync('git', ['rev-parse', '--short', 'HEAD'], { cwd: REPO, encoding: 'utf8' }).trim();
  } catch (e) {
    return 'unknown';
  }
}

const schema = projectSchemaVersion();
const hostApi = hostApiVersion();
const sha = gitSha();
const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
const version = schema + '+' + sha;
const out = resolve(arg('--out', join(REPO, 'dist', 'dhampir-' + version)));

console.log('产物目录 : ' + out);
console.log('契约版本 : project_schema = ' + schema + '   host_api = ' + hostApi + '  (git ' + sha + ')');
console.log('');

// ---------------------------------------------------------------- 构建
if (has('--clean-wasm')) {
  // 见文件头的说明：不清的话依赖改动进不去，而 wasm-pack 会假装成功。
  run('cargo', ['clean', '--target', 'wasm32-unknown-unknown']);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, 'preview', 'pkg'), { recursive: true });
mkdirSync(join(out, 'bin'), { recursive: true });

run('cargo', ['build', '--release', '--bin', 'dhampir']);
run('wasm-pack', ['build', 'crates/dhampir-wasm', '--target', 'web', '--release', '--out-dir', join(out, 'preview', 'pkg')]);

// ---------------------------------------------------------------- 摊平
cpSync(join(REPO, 'web/engine.js'), join(out, 'preview', 'engine.js'));
const exe = process.platform === 'win32' ? 'dhampir.exe' : 'dhampir';
cpSync(join(REPO, 'target/release', exe), join(out, 'bin', exe));

// ---------------------------------------------------------------- 许可证
// Apache-2.0 §4：**分发时**要随附许可证全文、保留声明、并如实列出第三方组件。
// 所以产物里固定带这两份；`--bundle-licenses` 再把每个依赖自带的文本抽进 licenses/。
cpSync(join(REPO, 'LICENSE-APACHE'), join(out, 'LICENSE'));
const thirdParty = join(REPO, 'THIRD-PARTY-LICENSES.md');
if (!existsSync(thirdParty)) {
  console.error('✗ 缺 THIRD-PARTY-LICENSES.md —— 先跑 `node scripts/licenses.mjs --write`');
  process.exit(1);
}
cpSync(thirdParty, join(out, 'THIRD-PARTY-LICENSES.md'));
if (has('--bundle-licenses')) run('node', ['scripts/licenses.mjs', '--bundle', out]);

writeFileSync(join(out, 'VERSION'), [
  'project_schema=' + schema,
  'host_api=' + hostApi,
  'license=Apache-2.0',
  'git=' + sha,
  'built_at=' + stamp,
  'platform=' + process.platform + '-' + process.arch,
  '',
].join('\n'));

// ---------------------------------------------------------------- 自检
// 只报"产出了什么"不算数 —— 这里按下游宿主启动时会做的三条检查先自检一遍：
const need = [
  join(out, 'preview', 'engine.js'),
  join(out, 'preview', 'pkg', 'dhampir_wasm.js'),
  join(out, 'preview', 'pkg', 'dhampir_wasm_bg.wasm'),
  join(out, 'bin', exe),
  join(out, 'VERSION'),
  join(out, 'LICENSE'),
  join(out, 'THIRD-PARTY-LICENSES.md'),
];
const missing = need.filter((p) => !existsSync(p));
if (missing.length > 0) {
  console.error('✗ 产物不齐，缺：');
  for (const p of missing) console.error('    ' + p);
  process.exit(1);
}

console.log('');
console.log('✓ 产物齐全：');
for (const p of need) console.log('    ' + p.replace(REPO + '\\', '').replace(REPO + '/', ''));

if (!has('--no-zip')) {
  const zip = out + '.zip';
  rmSync(zip, { force: true });
  makeZip(out, zip);
  console.log('  zip : ' + zip);
}

/// 打 zip。
///
/// # 为什么不用 `Compress-Archive`
///
/// 它在 Windows PowerShell 5.1 上会因为**某个文件的 LastWriteTime 转不成
/// DateTimeOffset** 而中途失败（实测：报一句 `ErrorWhenSetting`，
/// **zip 根本不生成**）。而它是**非终止错误** —— 脚本继续往下跑，
/// 最后打印一行 `zip : <路径>`，那个路径却不存在。
///
/// 那正是最难查的一类失败：产物清单说打了包，实际没有。
/// 所以这里换成 .NET 的 `ZipFile.CreateFromDirectory`（同一台机器的 API，
/// 不吃时间戳），并在**压完立刻验一次文件在不在** —— 少一个「应该成功了」。
function makeZip(from, zip) {
  const script = join(REPO, 'target', 'make-zip.ps1');
  mkdirSync(dirname(script), { recursive: true });
  writeFileSync(
    script,
    [
      'Add-Type -AssemblyName System.IO.Compression.FileSystem',
      '$src = ' + psQuote(from),
      '$dst = ' + psQuote(zip),
      'if (Test-Path $dst) { Remove-Item $dst -Force }',
      '[System.IO.Compression.ZipFile]::CreateFromDirectory($src, $dst, [System.IO.Compression.CompressionLevel]::Optimal, $false)',
      'if (Test-Path $dst) { exit 0 } else { exit 1 }',
      '',
    ].join('\n'),
  );
  try {
    execFileSync('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', script], {
      cwd: REPO,
      stdio: 'inherit',
    });
  } catch (e) {
    console.error('✗ 压 zip 失败：' + zip);
    process.exit(1);
  }
  // 退出码 0 还不足以说明它在 —— 上面踩的就是「说了但没做」。
  if (!existsSync(zip)) {
    console.error('✗ 压 zip 报成功但文件不在：' + zip);
    process.exit(1);
  }
}

/// PowerShell 单引号字符串里，单引号自己要用两个表示。
function psQuote(value) {
  return "'" + String(value).replace(/'/g, "''") + "'";
}

console.log('');
console.log('交给下游宿主：把整个目录放到 <程序目录>/dhampir/ ，然后设');
console.log('    VTEDIT_DHAMPIR_PREVIEW=<程序目录>/dhampir/preview');
console.log('    VTEDIT_DHAMPIR_CLI=<程序目录>/dhampir/bin/' + exe);
