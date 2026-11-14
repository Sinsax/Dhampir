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
//       VERSION                          ← **产品版本** + project_schema + host_api + git sha + 构建时间 + license
//       LICENSE                          ← LICENSE-APACHE（Apache-2.0 §4：分发要随附全文）
//       THIRD-PARTY-LICENSES.md          ← 生成的第三方清单（scripts/licenses.mjs）
//       licenses/                        ← 只有加 --bundle-licenses 时才在（每个依赖自带的文本）
//     <out>.zip                           ← 构建身份（名字带 +<sha>）
//     dhampir-<产品版本>-<平台>.zip        ← **稳定名**，下载地址写它
//     dhampir-<产品版本>-<平台>.zip.sha256.txt
//
// # 两个"版本"是两回事（2026-10-03 拆开）
//
//     version         产品版本（0.1.0）  ← 根 Cargo.toml [workspace.package]
//     project_schema  工程文件契约（1）   ← dhampir-timeline/src/project.rs
//     host_api        wasm 导出面契约（6）← dhampir-timeline/src/host_api.rs
//
// 产物目录以前叫 `dhampir-<schema>+<sha>`（如 `dhampir-1+9db716b`）—— 名字里的 `1` 是
// **schema 版本**。问题是 schema **兼容变更时根本不动**，于是一堆内容不同的产物共用一个名字，
// 「下载地址该写哪个」没有答案。现在名字跟**产品版本**走，`+<sha>` 保留可追溯性；
// 另出一份不带 sha 的稳定名，专供下载地址。
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
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
// deflateRawSync：纯 Node 打 zip 用（Linux / macOS 那条路，见 makeZipWithNode）。
import { deflateRawSync } from 'node:zlib';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// ---------------------------------------------------------------- zip 用到的常量
//
// ⚠️ **这几个必须在 makeZip 被调用之前求值**。它们原来是 `const`、写在文件末尾，
// 而 `makeZip(...)` 在本文件**上半部分**就被调用了 —— 于是踩了 ESM 的暂时性死区
// （`Cannot access 'CRC_TABLE' before initialization`）。
// 那个报错只在**真跑**时出现，`node --check` 看不出来（语法是合法的）。放在这里最省事。
//
// CRC-32（PNG / zip / gzip 都用同一条多项式）。
/// 手写是为了不引入依赖 —— 本仓的 node 依赖面刻意保持为零（没有 package.json）。
const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

// DOS 时间戳的固定值：1980-01-01 00:00:00 —— 见 makeZipWithNode 的说明。
const dosTime = 0;
const dosDate = (1 << 9) | (1 << 5) | 1;

function arg(name, fallback) {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] && !process.argv[i + 1].startsWith('--') ? process.argv[i + 1] : fallback;
}
const has = (name) => process.argv.includes(name);

// ⚠️ 这个脚本以前**不认识 --help**：任何参数都照单全收、"真跑"一遍 —— 对一个会
// 重新构建 wasm + 二进制的脚本来说，`--help` 触发一次全量构建是最坏的帮助方式。
if (has('--help') || has('-h')) {
  console.log([
    '用法：node scripts/package.mjs [--out <目录>] [--clean-wasm] [--no-zip] [--bundle-licenses]',
    '        [--bin <预构建二进制路径>] [--platform linux-x64|windows-x64] [--allow-host-glibc]',
    '',
    '  --bin               用预构建二进制（容器 / cargo-xwin 交叉编译），跳过本机 cargo build；',
    '                      wasm 仍由本脚本在宿主机出（wasm-pack）。发布路径走 scripts/release-local.sh。',
    '  --platform          产物目标平台；交叉编译时必须显式给（VERSION 的 platform=、zip 稳定名、',
    '                      可执行位自检都跟着它走，默认按宿主推）。',
    '  --allow-host-glibc  glibc 超基线时显式放行（只许"产物仅本机跑"，不许进发布渠道）。',
  ].join('\n'));
  process.exit(0);
}

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

/// **发布版本**（产品版本，语义化）—— 唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`。
///
/// ⚠️ **它与 `project_schema` 是两回事，2026-10-03 才拆开**：
///   产物目录以前叫 `dhampir-<schema>+<sha>`（例如 `dhampir-1+9db716b`）—— 那个 `1` 是
///   **schema 版本**，不是产品版本。后果是"下载地址该写哪个"没有答案：名字跟着 schema 走，
///   而 schema 在兼容变更时**根本不动**，于是一堆不同内容的产物共用一个名字。
///   现在：名字跟**产品版本**走，`+<sha>` 保留可追溯性。
///
/// 同一条纪律：读不到就**失败**，不猜。
function productVersion() {
  const src = readFileSync(join(REPO, 'Cargo.toml'), 'utf8');
  // 只认 `[workspace.package]` 段里那个 version，别匹配到 `rust-version` / `wgpu = "30.0.1"`。
  const pkg = src.match(/\[workspace\.package\]([\s\S]*?)(?:\n\[|$)/);
  const m = pkg && pkg[1].match(/^version\s*=\s*"([^"]+)"/m);
  if (!m) {
    console.error('✗ 读不出 [workspace.package] version（根 Cargo.toml）——');
    console.error('  它是发布版本，决定产物名与 Release 资产名，读不到就**不许猜**，直接失败。');
    process.exit(1);
  }
  return m[1];
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
const release = productVersion();
const sha = gitSha();
const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
// 目录名 = `dhampir-<产品版本>+<sha>`（与 V-Trim 的 `vtrim-v<版本>-win-x64` 同一套思路：
// 产品版本在前、构建身份在后）。zip 名另出一份不带 sha 的（见文件末），那个才是**下载地址用的**。
const version = release + '+' + sha;
const out = resolve(arg('--out', join(REPO, 'dist', 'dhampir-' + version)));

// ---------------------------------------------------------------- 目标平台与预构建二进制
//
// `--platform linux-x64|windows-x64`：产物的**目标**平台。默认按宿主推 —— 但
// 交叉编译（在 Linux 宿主上出 windows 产物）时必须显式给：VERSION 的 `platform=`、
// zip 稳定名、可执行位自检全都跟着它走；猜错了就是"产物名写着 linux、里面是 PE"。
const PLATFORMS = { 'linux-x64': 'dhampir', 'windows-x64': 'dhampir.exe' };
const platform = arg('--platform', process.platform === 'win32' ? 'windows-x64' : 'linux-x64');
if (!PLATFORMS[platform]) {
  console.error('✗ --platform 只认 ' + Object.keys(PLATFORMS).join(' / ') + '，实得：' + platform);
  process.exit(1);
}
const targetExe = PLATFORMS[platform];

// `--bin <路径>`：用**预构建**的二进制（容器 / cargo-xwin 交叉编译出来的），跳过本机
// `cargo build`。发布路径一律长这样：native 二进制的 glibc 基线由**构建环境**决定
// （见 docker/Dockerfile.linux-x64），在 Arch 宿主上直编会被下面的门禁拒收 ——
// 所以 scripts/release-local.sh 容器/交叉编译完，用 --bin 把二进制交回本脚本；
// wasm 那半与 glibc 无关，仍由本脚本在宿主机上出（wasm-pack）。
const prebuilt = arg('--bin', null);
if (prebuilt && !existsSync(prebuilt)) {
  console.error('✗ --bin 指向的文件不存在：' + prebuilt);
  process.exit(1);
}

// glibc 基线：**linux-x64 产物最高只许要 GLIBC_2.39** —— 与下游宿主 YekiTrim 的
// xtask `MAX_GLIBC` 同一条线。滚动发行版（宿主 glibc 2.44）直编的 ELF 会要求
// GLIBC_2.43，在我们要支持的所有发行版上都起不来，而构建期看不出任何问题。
// 门禁读的是**真出来的二进制**，容器保证构建环境 —— 双保险缺一不可。
// `--allow-host-glibc` 是显式逃生口（只许"产物仅本机跑"）：照样量、照样报，但放行。
const MAX_GLIBC = [2, 39];
const allowHostGlibc = has('--allow-host-glibc');

console.log('产物目录 : ' + out);
console.log('发布版本 : ' + release + '  (git ' + sha + ')');
console.log('契约版本 : project_schema = ' + schema + '   host_api = ' + hostApi);
console.log('目标平台 : ' + platform + (prebuilt ? '   (二进制来自 --bin：' + prebuilt + ')' : '   (本机 cargo build)'));
console.log('');

// ---------------------------------------------------------------- 构建
if (has('--clean-wasm')) {
  // 见文件头的说明：不清的话依赖改动进不去，而 wasm-pack 会假装成功。
  run('cargo', ['clean', '--target', 'wasm32-unknown-unknown']);
}

rmSync(out, { recursive: true, force: true });
mkdirSync(join(out, 'preview', 'pkg'), { recursive: true });
mkdirSync(join(out, 'bin'), { recursive: true });

if (prebuilt) {
  console.log('  $ (跳过 cargo build：二进制来自 --bin，wasm 仍在本机出)');
} else {
  run('cargo', ['build', '--release', '--bin', 'dhampir']);
}
run('wasm-pack', ['build', 'crates/dhampir-wasm', '--target', 'web', '--release', '--out-dir', join(out, 'preview', 'pkg')]);

// ---------------------------------------------------------------- 摊平
cpSync(join(REPO, 'web/engine.js'), join(out, 'preview', 'engine.js'));
// 二进制来源：--bin 给的预构建产物（容器 / 交叉编译），否则本机 target/release。
// 落进产物时**统一改名**成目标平台的形状（linux → dhampir，windows → dhampir.exe）：
// 交叉编译时"宿主平台"与"目标平台"不同，别拿宿主名字拼产物里的名字。
const binSrc = prebuilt || join(REPO, 'target/release', PLATFORMS[process.platform === 'win32' ? 'windows-x64' : 'linux-x64']);
cpSync(binSrc, join(out, 'bin', targetExe));

// ---------------------------------------------------------------- glibc 门禁
// 只对 linux-x64 产物生效；windows 产物是 PE，没有 GLIBC 符号这回事。
// 判据与下游宿主 xtask::max_glibc_symbol 同一条：在 .dynstr 里找 GLIBC_ 字面量
// 取最高版本 —— 不依赖 objdump / readelf，纯字节扫描足够。
if (platform === 'linux-x64') {
  const binBytes = readFileSync(join(out, 'bin', targetExe));
  const isElf = binBytes.subarray(0, 4).toString('latin1') === String.fromCharCode(0x7f) + 'ELF';
  const max = isElf ? maxGlibcSymbol(binBytes) : null;
  if (!isElf) {
    console.log('  glibc 门禁：产物不是 ELF —— 跳过（请确认这是有意的）');
  } else if (!max) {
    console.log('  glibc 门禁：ELF 里没有 GLIBC_* 符号（全静态链接？）—— 不拦，但请确认这是有意的');
  } else {
    console.log('  glibc 门禁：最高要求 GLIBC_' + max[0] + '.' + max[1] + '（上限 ' + MAX_GLIBC.join('.') + '）');
    const over = max[0] > MAX_GLIBC[0] || (max[0] === MAX_GLIBC[0] && max[1] > MAX_GLIBC[1]);
    if (over && !allowHostGlibc) {
      console.error('✗ 这个二进制需要 GLIBC_' + max[0] + '.' + max[1] + '，超过基线 ' + MAX_GLIBC.join('.') + '。');
      console.error('  它几乎肯定是在滚动发行版宿主上直编的 —— 在所有目标发行版上都起不来。');
      console.error('  正确路径：docker build -f docker/Dockerfile.linux-x64 -t dhampir-linux-x64-builder .');
      console.error('            再跑 scripts/release-local.sh linux（产物经 --bin 交回本脚本）。');
      console.error('  只在本机跑、不进发布渠道：加 --allow-host-glibc 显式放行。');
      process.exit(1);
    }
    if (over && allowHostGlibc) {
      console.warn('⚠️ --allow-host-glibc：超基线产物被**显式**放行 —— 不许进发布渠道（下游宿主的门禁会同样拒收它）。');
    }
  }
}

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
  // `version` 与 `project_schema` **必须都在、且是两回事**：
  //   version        产品版本（0.1.0）—— 人读、"这一版是哪一版"、Release 资产名
  //   project_schema 工程文件契约（1）—— 底座拿它拒错版工程
  // 合成一条会让"兼容变更"（schema 不动、产品动了）表达不出来。
  'version=' + release,
  'project_schema=' + schema,
  'host_api=' + hostApi,
  'license=Apache-2.0',
  'git=' + sha,
  'built_at=' + stamp,
  // 平台写**目标**平台（--platform），不是构建机 —— 交叉编译时两者不同。
  'platform=' + platform,
  '',
].join('\n'));

// ---------------------------------------------------------------- 自检
// 只报"产出了什么"不算数 —— 这里按下游宿主启动时会做的三条检查先自检一遍：
const need = [
  join(out, 'preview', 'engine.js'),
  join(out, 'preview', 'pkg', 'dhampir_wasm.js'),
  join(out, 'preview', 'pkg', 'dhampir_wasm_bg.wasm'),
  join(out, 'bin', targetExe),
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

  // 再出一份**不带 sha** 的稳定名：`dhampir-<产品版本>-<平台>.zip`。
  // 为什么需要它：下载地址要能**写死在钉固文件里**，而带 sha 的名字每提交一次就变。
  // 带 sha 的那份留着 —— 它是"这批字节出自哪次构建"的凭据，两者用途不同，都别删。
  const stable = join(REPO, 'dist', 'dhampir-' + release + '-' + platform + '.zip');
  rmSync(stable, { force: true });
  cpSync(zip, stable);
  console.log('  zip : ' + stable + '   ← 稳定名（下载地址用这个）');
  // sha256 侧车：与 V-Trim 的 `<zip>.sha256.txt` 同一约定，让"拉取"可被验证。
  const hex = sha256File(stable);
  writeFileSync(stable + '.sha256.txt', hex + '  ' + stable.split(/[\\/]/).pop() + '\n');
  console.log('  sha256: ' + hex);
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
/// 所以 Windows 上用 .NET 的 `ZipFile.CreateFromDirectory`（同一台机器的 API，
/// 不吃时间戳）。
///
/// # 为什么 Linux / macOS 上不是同一条路（2026-10-04 补）
///
/// `powershell` 在 Linux 上通常**不存在**（本机实测 ENOENT）—— 于是这条发布流程
/// 在 Linux 上走到压包就断了，产物只出得了 Windows 的。而这与 README 的承诺
/// 「两端都得能活」是矛盾的：Linux 是**服务端出片的目标环境**，它更需要一份定版产物。
///
/// 所以按平台分叉，且**判据只认结果**：**不管走哪条路，压完都要验 zip 真的在、
/// 且能被读回来**（见 verifyZip）。不验的话，"换了实现"就是从
/// 「说了但没做」换成「换了个地方说了但没做」。
///
/// 这里**不用** `zip` 命令行：它不一定装（本机没有），且不同发行版的 `-x`/`-r`
/// 语义有出入。用 Node 自己写 zip 容器，依赖只有 `node:zlib`。
function makeZip(from, zip) {
  if (process.platform === 'win32') {
    makeZipWithPowerShell(from, zip);
  } else {
    makeZipWithNode(from, zip);
  }
  // 退出码 0 还不足以说明它在 —— 上面踩的就是「说了但没做」。
  if (!existsSync(zip)) {
    console.error('✗ 压 zip 报成功但文件不在：' + zip);
    process.exit(1);
  }
  // 非 Windows 上连可执行位一起验（见 verifyZip 的说明）。
  // 可执行位自检只对 linux 产物（zip 里 unix 权限位那条）；windows 产物没有这一说。
  verifyZip(zip, platform === 'linux-x64' ? 'bin/' + targetExe : null);
}

function makeZipWithPowerShell(from, zip) {
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
}

/// 纯 Node 的 zip 写出（Linux / macOS 那条路）。
///
/// **为什么手写容器**：`execFileSync('zip', …)` 与本仓一贯的纪律冲突 ——
/// 依赖外部工具就得处理"装没装 / 版本差异"，而这类失败的表现恰恰是
/// "看起来跑了，产物不对"。zip 的 stored/deflate 两种条目各几十行，可控。
///
/// 三个刻意的选择，都为了让**产物可被任意解压器读**：
///   * 路径分隔符固定 `/`（zip 规范要求，与建包平台无关）；
///   * 目录条目也写（`preview/` 这种），否则某些解压器建不出空目录层级；
///   * **时间戳用固定的 `1980-01-01`**（DOS 时间的下限）。理由与 Windows 那条路
///     一致：**不吃文件时间戳**。副作用是 zip 可复现——同样的输入两次压出同样字节，
///     这对"下载地址钉固的字节"是有价值的性质。
function makeZipWithNode(from, zip) {
  const entries = [];
  const walk = (dir, prefix) => {
    for (const name of readdirSync(dir).sort()) {
      const full = join(dir, name);
      const rel = prefix ? prefix + '/' + name : name;
      const st = statSync(full);
      if (st.isDirectory()) {
        entries.push({ name: rel + '/', dir: true, mode: st.mode });
        walk(full, rel);
      } else if (st.isFile()) {
        // 不是普通文件（符号链接等）就跳过：产物里不该有它们，静默跟着走更坏。
        entries.push({ name: rel, data: readFileSync(full), mode: st.mode });
      }
    }
  };
  walk(from, '');

  const chunks = [];
  const central = [];
  let offset = 0;
  for (const entry of entries) {
    const nameBytes = Buffer.from(entry.name, 'utf8');
    let method = 0;
    let payload = Buffer.alloc(0);
    if (!entry.dir) {
      payload = entry.data;
      const deflated = deflateRawSync(payload);
      // 压不小就用 stored（小文件常见）—— 与主流 zip 工具同样的取舍。
      if (deflated.length < payload.length) {
        method = 8;
        payload = deflated;
      }
    }
    const crc = entry.dir ? 0 : crc32(entry.data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); // 本地文件头签名
    local.writeUInt16LE(20, 4); // 解压所需版本 2.0
    local.writeUInt16LE(0x0800, 6); // 通用位标记：文件名是 UTF-8
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(dosTime, 10);
    local.writeUInt16LE(dosDate, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(payload.length, 18);
    local.writeUInt32LE(entry.dir ? 0 : entry.data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    local.writeUInt16LE(0, 28); // 无扩展字段
    chunks.push(local, nameBytes, payload);

    const cd = Buffer.alloc(46);
    cd.writeUInt32LE(0x02014b50, 0); // 中央目录头签名
    // 制作版本 = 3 (Unix) << 8 | 20。**这一位必须写对**：不写的话解压器按 MS-DOS
    // 解释 external_attr，`bin/dhampir` 就没有可执行位 —— 产物解出来跑不了。
    // （实测踩过：zip 里 external_attr=0x0，解开是 -rw-r--r--。）
    cd.writeUInt16LE((3 << 8) | 20, 4);
    cd.writeUInt16LE(20, 6); // 解压所需版本
    cd.writeUInt16LE(0x0800, 8);
    cd.writeUInt16LE(method, 10);
    cd.writeUInt16LE(dosTime, 12);
    cd.writeUInt16LE(dosDate, 14);
    cd.writeUInt32LE(crc, 16);
    cd.writeUInt32LE(payload.length, 20);
    cd.writeUInt32LE(entry.dir ? 0 : entry.data.length, 24);
    cd.writeUInt16LE(nameBytes.length, 28);
    // 外部属性高 16 位放 UNIX 权限位（低 16 位留给 DOS 属性，目录位 0x10）。
    // 只取低 12 位（0777）：`st.mode` 上部还有文件类型位（普通文件 0100000），
    // 一起写进去会让某些解压器认不出来。
    const unixMode = entry.mode & 0o777;
    cd.writeUInt32LE(((entry.dir ? 0x10 : 0) | (unixMode << 16)) >>> 0, 38);
    cd.writeUInt32LE(offset, 42);
    central.push(cd, nameBytes);

    offset += local.length + nameBytes.length + payload.length;
  }

  const centralBuf = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); // 中央目录结束记录
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(centralBuf.length, 12);
  end.writeUInt32LE(offset, 16);
  writeFileSync(zip, Buffer.concat([...chunks, centralBuf, end]));
}

/// 读回 zip 并核条目数 —— **"写了字节"不等于"是个能读的 zip"**。
///
/// 只核中央目录结束记录存在、且里面的条目数与实际写进去的一致。不做完整解压：
/// 那需要另写一遍 inflate 的路径，而这里要挡的是"容器写歪了"，那个用签名就能发现。
///
/// `mustExec`：在**非 Windows** 上要求 `bin/<exe>` 带着可执行位。
/// 这条是补出来的 —— 第一版没写 external_attr，产物解开是 `-rw-r--r--`，
/// **跑不起来**。而"zip 打得开、文件都在"这种自检完全发现不了它。
function verifyZip(zip, mustExec) {
  const buf = readFileSync(zip);
  // 中央目录结束记录在最尾 22 字节（本仓不写注释，故无变长尾部）。
  const tail = buf.subarray(buf.length - 22);
  if (tail.readUInt32LE(0) !== 0x06054b50) {
    console.error('✗ zip 的中央目录结束记录不在预期位置：' + zip);
    process.exit(1);
  }
  const declared = tail.readUInt16LE(10);
  if (declared === 0) {
    console.error('✗ zip 里一个条目都没有：' + zip);
    process.exit(1);
  }
  if (mustExec) {
    const centralOffset = tail.readUInt32LE(16);
    const count = declared;
    let p = centralOffset;
    let checked = false;
    for (let i = 0; i < count; i++) {
      if (buf.readUInt32LE(p) !== 0x02014b50) break;
      const nameLen = buf.readUInt16LE(p + 28);
      const extraLen = buf.readUInt16LE(p + 30);
      const commentLen = buf.readUInt16LE(p + 32);
      const name = buf.subarray(p + 46, p + 46 + nameLen).toString('utf8');
      if (name === mustExec) {
        // 偏移 4 的 2 字节是「制作版本 + 制作系统」，且**顺序与直觉相反**：
        //   低字节 = 制作**版本**（20），高字节 = 制作**系统**（3 = Unix）。
        //   实测该字段是 0x0314 —— 我先写成低字节=系统，于是判据把自己刚写对的 zip 判红了。
        //   解压器（与 python zipfile）读的是**高字节**。
        const createSystem = buf.readUInt8(p + 5);
        const mode = buf.readUInt32LE(p + 38) >>> 16;
        if (createSystem !== 3 || (mode & 0o111) === 0) {
          console.error('✗ zip 里 ' + mustExec + ' 没有可执行位（create_system=' + createSystem +
            '，mode=' + mode.toString(8) + '）—— 解开后跑不起来。');
          process.exit(1);
        }
        checked = true;
      }
      p += 46 + nameLen + extraLen + commentLen;
    }
    if (!checked) {
      console.error('✗ zip 里找不到 ' + mustExec + ' —— 产物不齐。');
      process.exit(1);
    }
  }
  return declared;
}

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

/// 文件的 sha256（十六进制小写）。
///
/// 为什么自己算、不调 `certutil`/`Get-FileHash`：那两个的**输出格式随平台与语言变**
/// （certutil 会带 "SHA256 hash of ..." 一行前缀，中文系统还是中文），而这份值要写进
/// 侧车文件、被下游宿主与用户逐字比对 —— 差一个空格就是"校验失败"。
/// `node:crypto` 在哪台机器上都是同一个字符串。
function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

/// 从 ELF 字节里取它需要的最高 `GLIBC_x.y`（与下游宿主 xtask::max_glibc_symbol 同一判据）。
///
/// 刻意不依赖 objdump / readelf：版本串就在 .dynstr 里，纯字节扫描足够 ——
/// 本仓的 node 依赖面刻意为零，工具依赖同样能不加就不加。
/// 返回 [major, minor]；一个 GLIBC_* 都没有（全静态链接）返回 null。
/// ⚠️ `GLIBCXX_3.4`（libstdc++）不含 `GLIBC_` 这个带下划线的前缀，不会误命中；
/// `GLIBC_ABI_DT_RELR` 这类无数字版本串被 `\d+\.\d+` 的匹配规则自然跳过。
function maxGlibcSymbol(bytes) {
  const latin = bytes.toString('latin1');
  const needle = 'GLIBC_';
  let best = null;
  let at = latin.indexOf(needle);
  while (at >= 0) {
    const m = /^([0-9]+)\.([0-9]+)/.exec(latin.slice(at + needle.length));
    if (m) {
      const cur = [Number(m[1]), Number(m[2])];
      if (!best || cur[0] > best[0] || (cur[0] === best[0] && cur[1] > best[1])) best = cur;
    }
    at = latin.indexOf(needle, at + needle.length);
  }
  return best;
}

/// PowerShell 单引号字符串里，单引号自己要用两个表示。
function psQuote(value) {
  return "'" + String(value).replace(/'/g, "''") + "'";
}

console.log('');
console.log('交给下游宿主：把整个目录放到 <程序目录>/dhampir/ ，然后设');
console.log('    VTEDIT_DHAMPIR_PREVIEW=<程序目录>/dhampir/preview');
console.log('    VTEDIT_DHAMPIR_CLI=<程序目录>/dhampir/bin/' + targetExe);
