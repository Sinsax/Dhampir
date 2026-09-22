#!/usr/bin/env node
// dhampir · WGSL 构造普查
//
// 这份工具回答一个问题：**本仓的 WGSL 到底用了哪些构造**。
// 它是《WGSL 可移植性子集》（plan/wgsl-portable-subset.md）的证据，不是闸。
//
// ── 为什么要有它 ────────────────────────────────────────────────────────────
//
// 子集文档若只是"凭印象列几条允许/禁止"，它很快就会和代码分家：有人用了新构造，
// 文档还停在上一版，而"文档说允许"照旧读起来像结论。所以文档里那张「允许」表
// **由本工具解析并核对**：表里没申报的构造一旦真的出现在代码里，本工具判红。
// 文档因此要么被更新，要么被拦住——不会悄悄烂掉。
//
// ── 闸在哪（这里不重复造） ──────────────────────────────────────────────────
//
// 真正的**闸**是 `crates/dhampir-core/src/render/wgsl_subset.rs` 的 `FORBIDDEN`
// 表（`cargo test` 里跑）。本工具不另写一份禁词表——它**从那个 Rust 文件里解析**
// 出来，再数一遍。两份清单若各写一份，迟早"一份被改对、另一份留在原样"
// （那句话就写在 wgsl_subset.rs 的模块文档里）。解析到的条数与源码声明的条数
// 对不上时直接退出 2：宁可说自己用不了，也不给一份"看着通过了"的报告。
//
// ── 用法 ────────────────────────────────────────────────────────────────────
//
//   node scripts/dhampir-wgsl-census.mjs [--out <目录>] [--declared <文档>] [--md]
//     --out      输出目录（默认 target/wgsl-census/；会被写成 wgsl-census.json/.txt）
//     --declared 解析该文档的「允许」表做核对（缺省不核对，只普查）
//     --md       额外打印一份可直接贴进文档的表格（markdown）
//   node scripts/dhampir-wgsl-census.mjs --self-test
//
// 退出码：0 普查完成（且核对通过）／1 核对不通过（禁词出现、或用了未申报的构造）
//         ／2 用不了（没扫到文件、读不了、解析不了）
//
// 扫描范围：`crates/**/*.wgsl`（跳过 node_modules / target / .git）。
// 计数一律在**剥掉注释之后**的代码上做——两份 WGSL 的文件头都写着"不用 fwidth"，
// 不剥注释的话，这份普查会去举报它自己的文档。

import { mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const GUARD_PATH = 'crates/dhampir-core/src/render/wgsl_subset.rs';
const DECLARED_TABLE_HEADER_HINT = '构造';

// ---------------------------------------------------------------------------
// 要数的构造
//
// 一条 token 的写法约定：
//   · 以 `(` / `<` 结尾 —— 连这个符号一起匹配，数的就是**调用点 / 类型构造**
//     （`fract(` 不会数到 `fracture`；`vec3<` 不会数到 `vec3x`）
//   · 以 `@` 开头 —— 逐字面量匹配（`\b` 对 `@` 无效，见自检里那条用例）
//   · 其余 —— **词首一处 `\b`**，也就是说长标识符只要以它开头就算命中：
//     `fwidth` 也会数到 `fwidthCoarse`、`atomic` 也会数到 `atomicAdd`、
//     `loop` 也会数到 `loopback`（最后这个确实是误报，但方向是保守的）。
//     这一条不是疏忽，是**故意**的：守卫用的是 `contains()`，同样会命中 `fwidthCoarse`。
//     普查要是比守卫钝，就会出现"普查说 0 次、`cargo test` 却红了"的死角。
//     自检里那条"至少和守卫一样敏感"的用例把这个方向钉住了。
//
// 【口径】不同条目之间**会重叠**，`f32` 也会数到 `f32(` 里的那个 `f32`，所以
// 下面那张总表的数字**不能相加**——它是"逐条构造各出现多少次"，不是互斥分类。
//
// 表偏长是故意的：子集文档的「允许」列要穷尽，缺一格就等于留了一个没申报的口子。
// ---------------------------------------------------------------------------
const TOKENS = [
  // —— 导数（禁） ——
  ['fwidth', '导数'], ['fwidthCoarse', '导数'], ['fwidthFine', '导数'],
  ['dpdx', '导数'], ['dpdxCoarse', '导数'], ['dpdxFine', '导数'],
  ['dpdy', '导数'], ['dpdyCoarse', '导数'], ['dpdyFine', '导数'],
  // —— 纹理 ——
  ['textureSample(', '纹理采样'], ['textureSampleLevel(', '纹理采样'], ['textureSampleBias(', '纹理采样'],
  ['textureSampleCompare(', '纹理采样'], ['textureSampleCompareLevel(', '纹理采样'],
  ['textureSampleGrad(', '纹理采样'], ['textureGather(', '纹理采样'],
  ['sampler', '纹理采样'],
  ['textureLoad(', '纹理读取'], ['textureStore(', '纹理写入'],
  ['textureDimensions(', '纹理查询'], ['textureNumLevels(', '纹理查询'], ['textureNumSamples(', '纹理查询'],
  ['textureBarrier(', '同步'],
  // —— 原子与工作组同步 ——
  ['atomic', '原子'], ['workgroupBarrier(', '同步'], ['storageBarrier(', '同步'],
  ['workgroupUniformLoad(', '同步'],
  // —— 控制流 ——
  ['loop', '控制流'], ['for (', '控制流'], ['while (', '控制流'], ['if (', '控制流'],
  ['break', '控制流'], ['continue', '控制流'], ['discard', '控制流'], ['demote', '控制流'],
  ['switch', '控制流'], ['case ', '控制流'], ['default', '控制流'],
  // —— 精度 / 扩展 / 常量的隐式行为 ——
  ['f16', '精度'], ['i16', '精度'], ['mediump', '精度'], ['precise', '精度'],
  ['enable', '扩展'], ['override', '管线常量'], ['quantizeToF16(', '精度'],
  // —— 浮点数学（高危区集中在这里） ——
  ['abs(', '浮点'], ['acos(', '浮点'], ['asin(', '浮点'], ['atan(', '浮点'], ['atan2(', '浮点'],
  ['ceil(', '浮点'], ['clamp(', '浮点'], ['cos(', '浮点'], ['cosh(', '浮点'],
  ['degrees(', '浮点'], ['exp(', '浮点'], ['exp2(', '浮点'], ['floor(', '浮点'],
  ['fma(', '浮点'], ['fract(', '浮点'], ['inverseSqrt(', '浮点'], ['ldexp(', '浮点'],
  ['log(', '浮点'], ['log2(', '浮点'], ['max(', '浮点'], ['min(', '浮点'], ['mix(', '浮点'],
  ['modf(', '浮点'], ['pow(', '浮点'], ['radians(', '浮点'], ['round(', '浮点'],
  ['sign(', '浮点'], ['sin(', '浮点'], ['sinh(', '浮点'], ['smoothstep(', '浮点'],
  ['sqrt(', '浮点'], ['step(', '浮点'], ['tan(', '浮点'], ['tanh(', '浮点'], ['trunc(', '浮点'],
  // —— 向量 / 矩阵 ——
  ['dot(', '向量'], ['cross(', '向量'], ['length(', '向量'], ['normalize(', '向量'],
  ['distance(', '向量'], ['reflect(', '向量'], ['refract(', '向量'], ['faceForward(', '向量'],
  ['determinant(', '矩阵'], ['transpose(', '矩阵'],
  // —— 整数 / 位运算 / 打包 ——
  ['select(', '选择'], ['all(', '逻辑'], ['any(', '逻辑'],
  ['countOneBits(', '整数'], ['countLeadingZeros(', '整数'], ['countTrailingZeros(', '整数'],
  ['reverseBits(', '整数'], ['firstLeadingBit(', '整数'], ['firstTrailingBit(', '整数'],
  ['extractBits(', '整数'], ['insertBits(', '整数'],
  ['pack4x8snorm(', '打包'], ['unpack4x8snorm(', '打包'],
  ['pack4x8unorm(', '打包'], ['unpack4x8unorm(', '打包'],
  ['pack2x16float(', '打包'], ['unpack2x16float(', '打包'],
  // —— 模块与绑定 ——
  ['struct ', '类型'], ['array<', '类型'], ['var<uniform>', '绑定'], ['var<storage', '绑定'],
  ['var<workgroup>', '绑定'], ['var<private>', '绑定'], ['let ', '绑定'], ['var ', '绑定'],
  ['fn ', '函数'], ['alias ', '类型'],
  ['@group(', '属性'], ['@binding(', '属性'], ['@builtin(', '属性'], ['@location(', '属性'],
  ['@vertex', '入口'], ['@fragment', '入口'], ['@compute', '入口'],
  ['@workgroup_size(', '属性'], ['@interpolate(', '属性'], ['@invariant', '属性'],
  ['@id(', '属性'], ['@align(', '属性'], ['@size(', '属性'], ['@must_use', '属性'],
  // —— 标量与向量类型 ——
  ['bool', '类型'], ['i32', '类型'], ['u32', '类型'], ['f32', '类型'],
  ['vec2<', '类型'], ['vec3<', '类型'], ['vec4<', '类型'],
  ['mat2x2<', '类型'], ['mat3x3<', '类型'], ['mat4x4<', '类型'],
  ['i32(', '转换'], ['u32(', '转换'], ['f32(', '转换'], ['bool(', '转换'],
  ['bitcast<', '转换'],
  // —— 纹理类型（本仓在用 `texture_2d<f32>`；少了这一条它就能不经申报地出现） ——
  ['texture_2d<', '纹理类型'], ['texture_3d<', '纹理类型'],
  ['texture_cube<', '纹理类型'], ['texture_storage_2d<', '纹理类型'],
];

// ---------------------------------------------------------------------------
// 剥注释（与 wgsl_subset.rs 的 strip_wgsl_comments 同一语义，**只差一处**）
//
// 差别：Rust 那份把行注释末尾的换行也一起吃掉（"连行尾的换行一起吃掉"），
// 这里**保留**换行——因为这份工具要数"去注释后多少行非空"，吃掉换行会把两行代码
// 并成一行。对判定没有影响：禁词是按 token 匹配的，空白怎么处理都一样。
// （唯一的边角：`texture` 换行后接 `Sample(` 这种跨行的写法，Rust 那份会拼出禁词、
//   这份不会。少一个误报，方向是对的。）
//
// 这份实现是**第二次**写同一件事，所以它有自己的自检：剥注释若哪天变成"整份吃掉"，
// 下面的普查会瞬间变成"什么构造都没用"——一份全零的报告比没有报告更坏。
// ---------------------------------------------------------------------------
function stripWgslComments(src) {
  let out = '';
  let i = 0;
  while (i < src.length) {
    const c = src[i];
    if (c === '/' && src[i + 1] === '/') {
      while (i < src.length && src[i] !== '\n') i++; // 换行留给下一轮原样输出
    } else if (c === '/' && src[i + 1] === '*') {
      i += 2;
      while (i < src.length && !(src[i] === '*' && src[i + 1] === '/')) i++;
      i += 2;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

function countToken(code, token) {
  const first = token[0];
  const isWordish = /[A-Za-z_]/.test(first);
  if (!isWordish) {
    // `@builtin(` / `var<uniform>` 这类：逐字面量数。
    let n = 0;
    let from = 0;
    for (;;) {
      const at = code.indexOf(token, from);
      if (at === -1) return n;
      n++;
      from = at + token.length;
    }
  }
  const escaped = token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const re = new RegExp(`\\b${escaped}`, 'g');
  return (code.match(re) ?? []).length;
}

// ---------------------------------------------------------------------------
// 从 Rust 守卫里解析禁词表（唯一一份来源）
// ---------------------------------------------------------------------------
function parseForbiddenFromGuard(rustSource) {
  const head = rustSource.match(/FORBIDDEN:\s*\[\(&str,\s*&str\);\s*(\d+)\]\s*=\s*\[/);
  if (!head) return { error: '在 wgsl_subset.rs 里找不到 FORBIDDEN 声明' };
  const declaredCount = Number(head[1]);
  const start = head.index + head[0].length;
  const end = rustSource.indexOf('];', start);
  if (end === -1) return { error: 'FORBIDDEN 的数组没找到收尾的 `];`' };
  const body = rustSource.slice(start, end).replace(/\/\/[^\n]*/g, '');
  const literals = [...body.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
  if (literals.length !== declaredCount * 2) {
    return {
      error: `FORBIDDEN 声明 ${declaredCount} 条，但只解析出 ${literals.length} 个字符串`
        + `（应为 ${declaredCount * 2} 个：每条一个 token 一个理由）`,
    };
  }
  const entries = [];
  for (let i = 0; i < literals.length; i += 2) entries.push({ token: literals[i], why: literals[i + 1] });
  return { entries };
}

// ---------------------------------------------------------------------------
// 从文档里解析「允许」表
//
// 契约（写死在这里，文档那边照抄这一段）：文档里要有一行标记
//     <!-- wgsl-allow-table -->
// 紧跟其后的**第一张表**就是允许表；该表第一列的 `` `token` `` 就是申报的构造。
//
// 为什么不用"表头含『构造』"来认表：文档里还有「禁止的构造」「慎用的构造」几张表，
// 表头全都含那两个字，靠表头认表会把禁止表里的 token 也当成申报过的——
// 那等于把这道核对变成恒真。标记是显式的，认错不可能。
// ---------------------------------------------------------------------------
const ALLOW_TABLE_MARKER = '<!-- wgsl-allow-table -->';

function parseRow(line) {
  return line.trim().replace(/^\|/, '').replace(/\|$/, '').split('|').map((c) => c.trim());
}

function parseDeclaredTable(docSource, docPath) {
  const lines = docSource.split('\n');
  const markerAt = lines.findIndex((l) => l.trim() === ALLOW_TABLE_MARKER);
  if (markerAt === -1) {
    return { error: `${docPath}：找不到允许表标记 ${ALLOW_TABLE_MARKER}` };
  }
  const tokens = [];
  let headerSeen = false;
  for (let i = markerAt + 1; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (!trimmed.startsWith('|')) {
      if (headerSeen) break;   // 表结束了
      continue;                // 标记与表之间的空行/说明
    }
    const cells = parseRow(trimmed);
    if (!headerSeen) {
      if (!cells.includes(DECLARED_TABLE_HEADER_HINT)) {
        return { error: `${docPath}：标记后面那张表的表头不含「${DECLARED_TABLE_HEADER_HINT}」——标记贴错表了？` };
      }
      headerSeen = true;
      continue;
    }
    if (cells.every((c) => /^-+$/.test(c) || c === '')) continue; // 分隔行
    const m = cells[0].match(/^`([^`]+)`$/);
    if (!m) {
      return { error: `${docPath}：允许表有一行的第一列不是 \`token\` 形式：${trimmed}` };
    }
    tokens.push(m[1]);
  }
  if (!headerSeen) return { error: `${docPath}：标记后面没有表` };
  if (tokens.length === 0) return { error: `${docPath}：允许表里一条构造都没解析出来` };
  return { tokens };
}

// ---------------------------------------------------------------------------
// 扫文件
// ---------------------------------------------------------------------------
const SKIP_DIRS = new Set(['node_modules', 'target', '.git', 'dist', 'pkg']);

function findWgslFiles(root) {
  const found = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      if (entry.isDirectory()) {
        if (SKIP_DIRS.has(entry.name)) continue;
        walk(join(dir, entry.name));
      } else if (entry.name.endsWith('.wgsl')) {
        found.push(join(dir, entry.name).split(sep).join('/'));
      }
    }
  };
  walk(root);
  return found;
}

// ---------------------------------------------------------------------------
function census(files, forbidden) {
  const perFile = {};
  const totals = {};
  for (const file of files) {
    const raw = readFileSync(file, 'utf8');
    const code = stripWgslComments(raw);
    const counts = {};
    for (const [token] of TOKENS) {
      const n = countToken(code, token);
      if (n > 0) {
        counts[token] = n;
        totals[token] = (totals[token] ?? 0) + n;
      }
    }
    perFile[file] = {
      bytes: Buffer.byteLength(raw, 'utf8'),
      code_lines: code.split('\n').filter((l) => l.trim() !== '').length,
      counts,
    };
  }
  const forbiddenHits = forbidden
    .map(({ token, why }) => ({ token, why, count: totals[token] ?? 0 }))
    .filter((x) => x.count > 0);
  return { perFile, totals, forbiddenHits };
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------
function selfTest() {
  const checks = [];
  const check = (name, ok, detail) => checks.push({ name, ok, detail });

  check(
    '剥注释：行注释与块注释都去掉，代码原样留下（行尾换行保留）',
    stripWgslComments('// fwidth\nlet a = 1; /* dpdx */ let b = 2; // loop\n') === '\nlet a = 1;  let b = 2; \n',
  );
  check(
    '剥注释：块注释里带 `/` 的代码不会被吃掉',
    stripWgslComments('a/* x */b') === 'ab' && stripWgslComments('a /* a/b */ c') === 'a  c',
  );
  check(
    '整词计数：`fract(` 不会数到 `fracture`、`pow(` 不会数到 `powder`',
    countToken('let fracture = 1.0; let powder = 2.0;', 'fract(') === 0
      && countToken('let x = fract(1.0);', 'fract(') === 1,
  );
  check(
    '逐字面量计数：`@builtin(` 能被数到（`\\b` 对 `@` 无效，这条就是为它写的）',
    countToken('fn f(@builtin(position) p: vec4<f32>) {}', '@builtin(') === 1,
  );
  check(
    '词首匹配的方向：`fwidth` 必须能数到 `fwidthCoarse`、`atomic` 必须能数到 `atomicAdd`',
    // 这条钉的是「普查不比守卫钝」。守卫用的是 `contains()`，`fwidthCoarse` 会被它判红；
    // 普查要是改成 `\bfwidth\b` 就看不见了 —— 于是"普查 0 次、cargo test 红了"，
    // 而这份报告还照着绿的样子排版。别把它"顺手修正"成整词边界。
    countToken('fn f() -> f32 { return fwidthCoarse(x); }', 'fwidth') === 1
      && countToken('atomicAdd(&counter, 1u);', 'atomic') === 1,
  );
  check(
    '词首匹配的代价不许反过来吃掉 `textureSampleLevel`（它显式给 LOD，是允许的）',
    // 与 wgsl_subset.rs 里那条同名测试同一个约束：带左括号的禁词不能连它一起打。
    countToken('let a = textureSampleLevel(t, s, uv, 0.0);', 'textureSample(') === 0
      && countToken('let a = textureSampleLevel(t, s, uv, 0.0);', 'textureSampleLevel(') === 1,
  );
  check(
    '禁词表解析：单行与多行的条目都要解析出来，条数必须对上声明',
    (() => {
      const src = 'pub const FORBIDDEN: [(&str, &str); 2] = [\n'
        + '    ("fwidth", "导数"),\n'
        + '    (\n        "textureSample(", "隐式 LOD",\n    ),\n];\n';
      const parsed = parseForbiddenFromGuard(src);
      return !parsed.error && parsed.entries.length === 2 && parsed.entries[1].token === 'textureSample(';
    })(),
  );
  check(
    '禁词表解析：条数对不上时必须报错（不能少解析一条还当成功）',
    (() => {
      const src = 'pub const FORBIDDEN: [(&str, &str); 3] = [("fwidth", "导数")];\n';
      return Boolean(parseForbiddenFromGuard(src).error);
    })(),
  );
  check(
    '禁词表解析：找不到声明时也要报错',
    Boolean(parseForbiddenFromGuard('fn main() {}\n').error),
  );
  // 上面三条用的都是手写源码。这一条读**真的** wgsl_subset.rs：解析失败（有人改了写法、
  // 或文件被挪走）与"词表里有普查看不见的条目"都算自检不通过。少了这一条，
  // 手写用例全绿而真文件解析不了的情况就没人管。
  const realGuard = (() => {
    let src;
    try {
      src = readFileSync(resolve(REPO_ROOT, GUARD_PATH), 'utf8');
    } catch (error) {
      return { ok: false, detail: `读不了 ${GUARD_PATH}：${error.message}` };
    }
    const parsed = parseForbiddenFromGuard(src);
    if (parsed.error) return { ok: false, detail: `${GUARD_PATH}：${parsed.error}` };
    const invisible = parsed.entries.filter(({ token }) => countToken(`let zzz = ${token} ;\n`, token) === 0);
    if (invisible.length > 0) {
      return { ok: false, detail: `这些禁词普查数不到：${invisible.map((e) => e.token).join('、')}` };
    }
    return { ok: true, detail: `真守卫 ${parsed.entries.length} 条禁词，逐条都数得到` };
  })();
  check('真守卫的那份文件必须能被解析出来，且每一条禁词普查都看得见', realGuard.ok, realGuard.detail);
  check(
    '允许表解析：按标记认表，只收标记后面那一张（前面同样以「构造」开头的表不许被收）',
    (() => {
      const doc = '| 构造 | 说明 |\n|---|---|\n| `fract(` | 这张是别的表 |\n'
        + '\n<!-- wgsl-allow-table -->\n'
        + '| 构造 | 哪里用 |\n|---|---|\n| `select(` | 见下 |\n| `floor(` | 见下 |\n';
      const parsed = parseDeclaredTable(doc, '测试.md');
      return !parsed.error && parsed.tokens.join(',') === 'select(,floor(';
    })(),
  );
  check(
    '允许表解析：没有标记就报错（哪怕文档里有一张像模像样的表）',
    Boolean(parseDeclaredTable('| 构造 | 说明 |\n|---|---|\n| `fract(` | 见下 |\n', '测试.md').error),
  );
  check(
    '允许表解析：第一列不是 `token` 时要报错（静默跳过 = 那张表可以随便写）',
    Boolean(parseDeclaredTable('<!-- wgsl-allow-table -->\n| 构造 | 说明 |\n|---|---|\n| fract | 忘了反引号 |\n', '测试.md').error),
  );
  check(
    '允许表解析：标记贴到了别的表前面要报错（表头不含「构造」）',
    Boolean(parseDeclaredTable('<!-- wgsl-allow-table -->\n| 名称 | 说明 |\n|---|---|\n| `fract(` | x |\n', '测试.md').error),
  );
  check(
    '允许表解析：标记后面没有表要报错',
    Boolean(parseDeclaredTable('<!-- wgsl-allow-table -->\n\n没有表\n', '测试.md').error),
  );
  check(
    '命中的判定用的是去注释后的代码（注释里写 `fwidth` 不算命中）',
    (() => {
      const off = censusFromSource('// fwidth 是禁词\nlet a = 1.0;\n', [{ token: 'fwidth', why: 'x' }]);
      const on = censusFromSource('let a = fwidth(1.0);\n', [{ token: 'fwidth', why: 'x' }]);
      return off.forbiddenHits.length === 0 && on.forbiddenHits.length === 1;
    })(),
  );

  for (const c of checks) console.log(`  ${c.ok ? '✓' : '✗'} ${c.name}${c.ok ? '' : `  ← ${c.detail ?? '不成立'}`}`);
  const failed = checks.filter((c) => !c.ok).length;
  console.log(`\n${failed === 0 ? '✓' : '✗'} 自检 ${checks.length - failed}/${checks.length}`);
  return failed;
}

// 自检用：直接在源码字符串上跑一遍普查（不碰盘）
function censusFromSource(src, forbidden) {
  const code = stripWgslComments(src);
  const totals = {};
  for (const [token] of TOKENS) {
    const n = countToken(code, token);
    if (n > 0) totals[token] = n;
  }
  const forbiddenHits = forbidden
    .map(({ token, why }) => ({ token, why, count: totals[token] ?? 0 }))
    .filter((x) => x.count > 0);
  return { totals, forbiddenHits };
}

// ---------------------------------------------------------------------------
function parseArgs(argv) {
  const out = {
    out: resolve(REPO_ROOT, 'target/wgsl-census'),
    declared: null,
    md: false,
    selfTest: false,
  };
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    if (key === '--self-test') {
      out.selfTest = true;
    } else if (key === '--md') {
      out.md = true;
    } else if (key === '--out' || key === '--declared') {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) return { error: `${key} 后面要跟一个路径` };
      out[key.slice(2)] = resolve(REPO_ROOT, value);
      i++;
    } else {
      return { error: `不认识的参数 ${JSON.stringify(key)}` };
    }
  }
  return out;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.error) {
    console.error(args.error);
    return 2;
  }
  if (args.selfTest) return selfTest() === 0 ? 0 : 1;

  const guardAbs = resolve(REPO_ROOT, GUARD_PATH);
  let guardSource;
  try {
    guardSource = readFileSync(guardAbs, 'utf8');
  } catch (error) {
    console.error(`读不了守卫 ${GUARD_PATH}：${error.message}`);
    return 2;
  }
  const parsed = parseForbiddenFromGuard(guardSource);
  if (parsed.error) {
    console.error(`${GUARD_PATH}：${parsed.error}`);
    return 2;
  }
  const forbidden = parsed.entries;

  let files;
  try {
    files = findWgslFiles(REPO_ROOT);
  } catch (error) {
    console.error(`扫不了仓库：${error.message}`);
    return 2;
  }
  if (files.length === 0) {
    console.error('没扫到任何 .wgsl——"没扫到"和"扫过了没问题"是两件事');
    return 2;
  }

  const result = census(files, forbidden);
  result.scanned = files.map((f) => relative(REPO_ROOT, resolve(REPO_ROOT, f)).split(sep).join('/'));
  result.forbidden_source = GUARD_PATH;
  result.forbidden_table = forbidden.map(({ token, why }) => ({ token, why }));

  // ---- 与文档的「允许」表核对 ----
  let declaredCheck = null;
  if (args.declared) {
    const rel = relative(REPO_ROOT, args.declared).split(sep).join('/');
    let docSource;
    try {
      docSource = readFileSync(args.declared, 'utf8');
    } catch (error) {
      console.error(`读不了 ${rel}：${error.message}`);
      return 2;
    }
    const table = parseDeclaredTable(docSource, rel);
    if (table.error) {
      console.error(table.error);
      return 2;
    }
    const declaredSet = new Set(table.tokens);
    const usedTokens = Object.keys(result.totals).sort();
    const undeclared = usedTokens.filter((t) => !declaredSet.has(t));
    const declaredUnused = table.tokens.filter((t) => !(t in result.totals));
    declaredCheck = {
      doc: rel,
      declared: table.tokens.length,
      undeclared_but_used: undeclared,
      declared_but_unused: declaredUnused,
    };
    result.declared_check = declaredCheck;
  }

  // ---- 落盘 ----
  mkdirSync(args.out, { recursive: true });
  const jsonPath = join(args.out, 'wgsl-census.json');
  const txtPath = join(args.out, 'wgsl-census.txt');
  writeFileSync(jsonPath, `${JSON.stringify(result, null, 2)}\n`, 'utf8');

  const lines = [];
  lines.push('dhampir · WGSL 构造普查');
  lines.push(`扫描：${result.scanned.length} 份文件`);
  for (const [file, info] of Object.entries(result.perFile)) {
    lines.push(`  ${file}`);
    lines.push(`    ${info.bytes} 字节、去注释后 ${info.code_lines} 行非空、用了 ${Object.keys(info.counts).length} 种构造`);
  }
  lines.push('');
  lines.push(`构造总表（${Object.keys(result.totals).length} 种，按计数降序）：`);
  const sorted = Object.entries(result.totals).sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1));
  for (const [token, n] of sorted) lines.push(`  ${String(n).padStart(5)}  ${token}`);
  lines.push('');
  lines.push(`禁词（来源 ${GUARD_PATH}，共 ${result.forbidden_table.length} 条）：`);
  for (const { token, why } of result.forbidden_table) {
    const n = result.totals[token] ?? 0;
    lines.push(`  ${n === 0 ? '·' : '✗'} ${token.padEnd(18)} 出现 ${n} 次  —— ${why}`);
  }
  if (declaredCheck) {
    lines.push('');
    lines.push(`与文档的「允许」表核对（${declaredCheck.doc}）：申报 ${declaredCheck.declared} 条；`
      + `用了但没申报 ${declaredCheck.undeclared_but_used.length} 条；`
      + `申报了但当前没用 ${declaredCheck.declared_but_unused.length} 条`);
    if (declaredCheck.undeclared_but_used.length > 0) {
      lines.push(`  未申报却在用：${declaredCheck.undeclared_but_used.join('、')}`);
    }
    if (declaredCheck.declared_but_unused.length > 0) {
      lines.push(`  申报但没用：${declaredCheck.declared_but_unused.join('、')}`);
    }
  }
  writeFileSync(txtPath, `${lines.join('\n')}\n`, 'utf8');

  console.log(lines.join('\n'));
  console.log('');
  console.log(`  明细 → ${relative(REPO_ROOT, jsonPath).split(sep).join('/')}`
    + ` ／ ${relative(REPO_ROOT, txtPath).split(sep).join('/')}`);

  if (args.md) {
    console.log('');
    console.log('可直接贴进文档的表（构造 | 用处 | M2 现值）：');
    for (const [token, n] of sorted) console.log(`| \`${token}\` | | ${n} |`);
  }

  if (result.forbiddenHits.length > 0) {
    console.error('');
    console.error(`✗ 代码里出现了 ${result.forbiddenHits.length} 条禁词：`);
    for (const h of result.forbiddenHits) {
      // 理由必须跟着失败一起出来。只报"出现了 fwidth"的话，接手的人要么去问、
      // 要么去猜、要么把这一项删掉——三种结果都比把理由写在这里差
      // （这句理由的出处就是 wgsl_subset.rs 里那句同样的说明）。
      console.error(`    ${h.token} × ${h.count}  —— ${h.why}`);
    }
    return 1;
  }
  if (declaredCheck && declaredCheck.undeclared_but_used.length > 0) {
    console.error('');
    console.error(`✗ 有 ${declaredCheck.undeclared_but_used.length} 种构造在用、却没写进`
      + `${declaredCheck.doc} 的「允许」表：${declaredCheck.undeclared_but_used.join('、')}`);
    return 1;
  }
  return 0;
}

process.exitCode = main();
