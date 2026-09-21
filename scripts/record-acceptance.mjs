#!/usr/bin/env node
// 跑一个里程碑的「退出标准」，把每一项的命令、退出码、输出落盘成可复核的记录。
//
// 为什么要有这个东西：plan 里的执行规则是"上一个里程碑的退出标准全绿才允许进入
// 下一个"。手敲一遍命令再口头说"过了"，就不是全绿，是记忆。这里把它变成：
//
//   ① 每一项都留下原始输出（不是摘要，是原样）
//   ② 退出码为准，不靠读日志下结论
//   ③ acceptance.json 里每项都要有 exit_code 与 ok，缺一项就是缺一项
//   ④ 目录里一个文件都没写出来 → 拒绝通过（"没跑"和"跑过了"必须区分开）
//
// 用法：
//   node scripts/record-acceptance.mjs                 # 默认跑 m0
//   node scripts/record-acceptance.mjs --milestone m0
//   node scripts/record-acceptance.mjs --list          # 只看有哪些项
//   node scripts/record-acceptance.mjs --self-test

import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

// ---------------------------------------------------------------------------
// 每个里程碑的退出标准。
//
// `source` 指向 plan 里的原文位置——改了判定口径就必须回来改这里，不能只改代码。
// ---------------------------------------------------------------------------
export const MILESTONES = {
  m0: {
    title: 'M0 骨架与双编译贯通',
    source: 'plan/video-editor-plan.md §3「退出标准（三条全绿才算过，源自指导文档 §9.4）」',
    criteria: [
      {
        id: 'native-check',
        says: 'cargo check --workspace（native）通过',
        cmd: ['cargo', ['check', '--workspace', '--color', 'never']],
      },
      {
        id: 'wasm-check',
        says: 'cargo check -p dhampir-wasm --target wasm32-unknown-unknown 通过',
        // 备注见 plan §3：wasm 侧不能用 --workspace，dhampir-worker 是 native-only
        cmd: ['cargo', ['check', '-p', 'dhampir-wasm', '--target', 'wasm32-unknown-unknown', '--color', 'never']],
      },
      {
        id: 'native-tests',
        says: '工作区测试全绿（cargo test --workspace）',
        // plan §3 的第 3 条要求"帧号 → 时间码换算在 native #[test] 与 wasm 侧断言
        // 输出完全一致"，native 这一半在这里跑
        cmd: ['cargo', ['test', '--workspace', '--color', 'never']],
      },
      {
        id: 'cross-runtime',
        says: '同一份 golden 报告在 wasm32 运行时上逐字节相等（双运行时等值）',
        cmd: ['node', ['scripts/run-wasm-tests.mjs', '--out', 'records/m0']],
      },
      {
        id: 'guard-core-purity',
        says: 'dhampir-core 里没有 #[cfg] / cfg!',
        cmd: ['node', ['scripts/check-core-purity.mjs']],
      },
      {
        id: 'guard-dep-graph',
        says: 'crate 依赖方向单向无环',
        cmd: ['node', ['scripts/check-dep-graph.mjs']],
      },
      {
        id: 'guard-text-hygiene',
        says: '全仓 LF + 无 BOM + 合法 UTF-8',
        cmd: ['node', ['scripts/check-text-hygiene.mjs']],
      },
    ],
  },
};

export function parseArgs(argv) {
  const out = { milestone: 'm0', selfTest: false, list: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--milestone') {
      const value = argv[i + 1];
      if (value === undefined || value.startsWith('--')) return { error: '--milestone 后面缺名字' };
      out.milestone = value;
      i += 1;
    } else if (arg === '--list') out.list = true;
    else if (arg === '--self-test') out.selfTest = true;
    else if (arg === '-h' || arg === '--help') out.help = true;
    else return { error: `不认识的参数：${arg}` };
  }
  return out;
}

/** 数出 cargo test 报告里跑过了多少条测试（用 `test result:` 行求和）。 */
export function countTests(text) {
  let passed = 0;
  let failed = 0;
  for (const m of text.matchAll(/^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed/gm)) {
    passed += Number(m[1]);
    failed += Number(m[2]);
  }
  return { passed, failed };
}

/**
 * 把一次 spawnSync 的结果判成通过 / 不通过。
 *
 * 为什么把它抽成函数：这个记录工具**唯一**的结论就是 `ok` / `exit_code`，
 * 而它最危险的失效模式是"其实永远报绿"——那样 acceptance.json 会一直很好看，
 * 而里程碑根本没跑过。判定逻辑放在 main() 里就没法单测，抽出来才钉得住。
 *
 * 先前这里挂过一条临时判据（`__probe-must-fail`，拿一个坏参数去喂别的守卫，
 * 期望它红）来证明"本工具能红"。那是错的解法：它让 M0 的验收记录里永远
 * 躺着一条故意失败的项，于是 `green` 永远为 false。证明"能红"应该落在
 * 判定函数上，而不是污染里程碑记录。
 */
export function judgeResult(run) {
  if (run.error) {
    return { ok: false, exit_code: null, note: `起不来：${run.error.message}` };
  }
  // 负退出码 / null 退出码 = 被信号或断言打断。这不是"测试失败"，是**没有结论**，
  // 所以既不能读成成功，也不能说成"测试挂了"——本机上它就是 -1073740791
  // （libuv 的 UV_HANDLE_CLOSING 断言），与测试内容无关。
  if (typeof run.status !== 'number' || run.status < 0) {
    return {
      ok: false,
      exit_code: run.status ?? null,
      note: `异常退出（退出码 ${run.status}）——被信号或断言打断，不是测试失败`,
    };
  }
  return { ok: run.status === 0, exit_code: run.status };
}

/** 把输出按 LF 落盘。记录文件自己也得守"LF + 无 BOM"的规矩。 */
function writeLf(path, text) {
  writeFileSync(path, text.replace(/\r\n/g, '\n').replace(/^\uFEFF/, ''));
}

// ---------------------------------------------------------------------------
// 这份记录属于哪一棵树
//
// `generated_at` 只说明"什么时候跑的"，回答不了"跑在哪个提交上"——而记录的价值
// 恰恰在于别人能照着它复核。所以补两个字段：
//
//   commit：`git rev-parse --short=12 HEAD`
//   dirty ：`git status --porcelain` 剔除 `records/` 之后还有没有条目
//
// **为什么 dirty 必须忽略 `records/`**：本工具自己就在往 `records/` 里写文件
// （`acceptance.json` + 每项一份 `.txt`），不排除的话每次跑完 dirty 都是 true，
// 这个字段就永远取同一个值、一点信息都没有。
//
// **`null` 不等于干净**：git 不可用 / 不在仓库里 / 命令失败时两个字段降级为
// `null`。`dirty: null` 的意思是"**未知**"，跟 `false`（确认干净）是两件事——
// 读记录的人不能把"不知道"当成"没问题"。
// ---------------------------------------------------------------------------

/** 本工具自己的落盘目录。它的改动不参与 dirty 判定（见上面那段）。 */
const RECORDS_DIR = 'records/';

function isRecordPath(path) {
  // porcelain 里的路径一律用 `/`；含特殊字符时 git 会给整个路径加双引号。
  const clean = path.trim().replace(/^"|"$/g, '').split('\\').join('/');
  return clean === 'records' || clean.startsWith(RECORDS_DIR);
}

/**
 * 从 `git status --porcelain` 的输出里剔除 `records/` 下的条目，返回剩下的条目。
 *
 * porcelain 的格式是固定的 `XY <path>`：前两列是状态码（`??`、` M`、`M `…），
 * 从第 3 列起才是路径。重命名写成 `R  old -> new`，两侧都要看——只要**任一侧**
 * 在 records/ 之外，这条就说明"树上有别的东西动了"，必须保留。
 *
 * 抽成可导出的纯函数是为了能单测：这段解析写错的样子是"dirty 永远 false"，
 * 它不会报错、只会让字段失去意义。
 */
export function stripRecordEntries(porcelainText) {
  const kept = [];
  for (const line of porcelainText.split('\n')) {
    if (line.trim().length === 0) continue;
    const paths = line.slice(3).split(' -> ');
    if (paths.some((p) => !isRecordPath(p))) kept.push(line);
  }
  return kept;
}

/**
 * 把一次 spawnSync 的结果折成"有没有输出"：失败一律 `null`。
 *
 * `null` 表示**未知**（没装 git / 不在仓库里 / 退出码非 0），不是"空输出"。
 * 单测这条，是因为"git 缺失就把记录工具搞崩"会让整份验收记录写不出来——
 * 记录工具自己崩掉，比少一个字段严重得多。
 */
export function gitOutput(run) {
  if (run.error) return null;
  if (typeof run.status !== 'number' || run.status !== 0) return null;
  return run.stdout ?? '';
}

/**
 * 由两个 git 探针的原始输出折成记录里的两个字段。
 *
 * - `commit`：`git rev-parse --short=12 HEAD` 的输出（去掉首尾空白）。
 * - `dirty` ：剔除 `records/` 之后，porcelain 里还有没有条目。
 *
 * 任一探针为 `null`（未知）时，对应字段就是 `null`：`dirty: null` **不是**"干净"，
 * 别把未知读成 false。
 */
export function judgeTree(commitOutput, statusOutput) {
  const commit = commitOutput === null ? '' : commitOutput.trim();
  return {
    commit: commit.length > 0 ? commit : null,
    dirty: statusOutput === null ? null : stripRecordEntries(statusOutput).length > 0,
  };
}

/** 跑一条 git 子命令，失败/缺失返回 null（见 gitOutput）。 */
function runGit(args) {
  return gitOutput(spawnSync('git', args, { cwd: REPO_ROOT, encoding: 'utf8', shell: false }));
}

export function selfTest() {
  const cases = [];
  const check = (name, ok) => cases.push({ name, ok });

  check('--milestone 吃掉自己的值', parseArgs(['--milestone', 'm0']).milestone === 'm0');
  check('--milestone 之后的参数仍能被识别', parseArgs(['--milestone', 'm0', '--list']).list === true);
  check('--milestone 缺值要报错', typeof parseArgs(['--milestone']).error === 'string');
  check('不认识的参数要报错', typeof parseArgs(['--nope']).error === 'string');
  check('默认跑 m0', parseArgs([]).milestone === 'm0');

  check(
    '数得出通过/失败条数',
    JSON.stringify(countTests('test result: ok. 42 passed; 0 failed; 0 ignored\ntest result: FAILED. 1 passed; 2 failed\n')) ===
      JSON.stringify({ passed: 43, failed: 2 }),
  );
  check('空输出得到 0/0，不谎报', JSON.stringify(countTests('编译中…\n')) === JSON.stringify({ passed: 0, failed: 0 }));

  // 判定本身必须能被证伪——这取代了先前那条污染里程碑记录的 `__probe-must-fail`。
  check('退出码 0 判绿', judgeResult({ status: 0 }).ok === true);
  check('退出码 1 判红', judgeResult({ status: 1 }).ok === false);
  check('负退出码（libuv 断言）判红，绝不读成成功', judgeResult({ status: -1073740791 }).ok === false);
  check('被信号杀死（status = null）判红', judgeResult({ status: null }).ok === false);
  check('起不来判红并写明原因', judgeResult({ error: new Error('spawn ENOENT') }).ok === false);
  check(
    '红的时候必须留下可读的理由',
    /异常退出/.test(judgeResult({ status: -1 }).note ?? '') && /起不来/.test(judgeResult({ error: new Error('x') }).note ?? ''),
  );
  // 反向再钉一次：红必须能被红出来。若哪天判定被改成"只要跑到就算过"，这些会先炸。
  check('判定不是恒绿：存在至少一组输入判红', [0, 1, -1, null].some((s) => judgeResult({ status: s }).ok === false));

  // 「这份记录属于哪棵树」的三个函数。没有这几条，dirty 的解析可以悄悄坏成
  // "永远 false"（比如漏掉了 records/ 的排除，或反过来把什么都排除掉）。
  check('porcelain：records/ 下的未跟踪条目被剔除', stripRecordEntries('?? records/m0/acceptance.json\n').length === 0);
  check('porcelain：records/ 下的已修改条目被剔除', stripRecordEntries(' M records/m0/native-check.txt\n').length === 0);
  check('porcelain：records/ 本身（不带斜杠）也算记录目录', stripRecordEntries('?? records\n').length === 0);
  check(
    'porcelain：records/ 之外的条目仍算改动',
    stripRecordEntries('?? records/m0/x.txt\n M crates/dhampir-timeline/src/lib.rs\n').length === 1,
  );
  check(
    'porcelain：重命名条目只要任一侧在 records/ 之外就保留',
    stripRecordEntries('R  records/m0/a.txt -> src/b.rs\n').length === 1 &&
      stripRecordEntries('R  src/a.rs -> records/m0/b.txt\n').length === 1,
  );
  check('porcelain：空输出 / 空行 → 没有条目', stripRecordEntries('').length === 0 && stripRecordEntries('\n\n').length === 0);

  check('dirty：只有 records/ 改动 → false（否则这个字段永远是 true）', judgeTree('abc123d0a1f7', '?? records/m0/x.txt\n').dirty === false);
  check('dirty：records/ 之外有改动 → true', judgeTree('abc123d0a1f7', ' M scripts/check-dep-graph.mjs\n').dirty === true);
  check('commit：取 --short=12 的输出并去掉换行', judgeTree('98b517a3d0a1\n', '').commit === '98b517a3d0a1');
  check(
    'git 缺失/失败 → commit 与 dirty 都是 null（不崩）',
    judgeTree(null, null).commit === null && judgeTree(null, null).dirty === null,
  );
  check('dirty: null 是"未知"，绝不等同于 false', judgeTree(null, null).dirty !== false && judgeTree(null, '').dirty === false);
  check(
    'gitOutput：spawn 失败 / 退出非 0 都降级为 null',
    gitOutput({ error: new Error('spawn git ENOENT') }) === null &&
      gitOutput({ status: 128, stdout: '' }) === null &&
      gitOutput({ status: 0, stdout: 'ok\n' }) === 'ok\n',
  );

  // 每一项都必须能独立判死：缺 exit_code 或缺 ok 都算不完整
  for (const [name, spec] of Object.entries(MILESTONES)) {
    check(`${name}: 每一项都有 id / says / cmd`, spec.criteria.every((c) => c.id && c.says && Array.isArray(c.cmd) && c.cmd.length === 2));
    check(`${name}: id 不重复`, new Set(spec.criteria.map((c) => c.id)).size === spec.criteria.length);
    check(`${name}: 写明了 plan 原文出处`, typeof spec.source === 'string' && spec.source.length > 0);
  }

  const failed = cases.filter((c) => !c.ok);
  for (const c of cases) console.log(`  ${c.ok ? '✓' : '✗'} ${c.name}`);
  if (failed.length > 0) {
    console.error(`\n✗ 自检未通过：${failed.length} / ${cases.length}`);
    return 1;
  }
  console.log(`\n✓ 自检通过（${cases.length} 条用例）`);
  return 0;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    console.log('用法：node scripts/record-acceptance.mjs [--milestone m0] [--list] [--self-test]');
    return 0;
  }
  if (args.error) {
    console.error(`参数错误：${args.error}`);
    return 2;
  }
  if (args.selfTest) return selfTest();

  const spec = MILESTONES[args.milestone];
  if (!spec) {
    console.error(`✗ 不认识的里程碑：${args.milestone}（已知：${Object.keys(MILESTONES).join(', ')}）`);
    return 2;
  }
  if (args.list) {
    console.log(`${spec.title} —— 退出标准 ${spec.criteria.length} 条`);
    console.log(`出处：${spec.source}`);
    for (const c of spec.criteria) console.log(`  ${c.id.padEnd(20)} ${c.says}`);
    return 0;
  }

  const outDir = join(REPO_ROOT, 'records', args.milestone);
  mkdirSync(outDir, { recursive: true });

  const results = [];
  for (const criterion of spec.criteria) {
    const [command, commandArgs] = criterion.cmd;

    // node 脚本先确认存在：否则 spawnSync 会报一个语焉不详的 ENOENT，
    // 而"脚本被删了"应该是一条明确的红，不是一条含糊的红。
    if (command === 'node' && !existsSync(join(REPO_ROOT, commandArgs[0]))) {
      results.push({ id: criterion.id, says: criterion.says, ok: false, exit_code: null, note: `脚本不存在：${commandArgs[0]}` });
      console.log(`✗ ${criterion.id.padEnd(20)}      ${criterion.says}（脚本不存在：${commandArgs[0]}）`);
      continue;
    }

    const started = Date.now();
    const run = spawnSync(command, commandArgs, {
      cwd: REPO_ROOT,
      encoding: 'utf8',
      shell: false,
      maxBuffer: 64 * 1024 * 1024,
    });
    const seconds = Math.round((Date.now() - started) / 100) / 10;
    const stdout = run.stdout ?? '';
    const stderr = run.stderr ?? '';
    const output = `${stdout}${stderr}`;

    // 原始输出原样落盘：复核的人要看到的是命令说了什么，不是我摘了什么。
    // stdout 与 stderr 分开写——cargo 把 "Running ..." 打给 stderr、测试结果打给
    // stdout，拼在一起会让顺序看着像个 bug（结果在前、谁跑的在后）。
    writeLf(
      join(outDir, `${criterion.id}.txt`),
      `$ ${command} ${commandArgs.join(' ')}\n\n----- stdout -----\n${stdout}\n----- stderr -----\n${stderr}`,
    );
    const tests = countTests(output);
    const verdict = judgeResult(run);

    results.push({
      id: criterion.id,
      says: criterion.says,
      command: `${command} ${commandArgs.join(' ')}`,
      exit_code: verdict.exit_code,
      seconds,
      ok: verdict.ok,
      ...(criterion.id.endsWith('tests') ? { tests } : {}),
      ...(verdict.note ? { note: verdict.note } : {}),
    });
    console.log(
      `${verdict.ok ? '✓' : '✗'} ${criterion.id.padEnd(20)} ${seconds}s  ${criterion.says}` +
        (verdict.ok ? '' : ` ← ${verdict.note ?? `退出码 ${verdict.exit_code}`}`),
    );
  }

  // 一个文件都没写出来 → 拒绝通过。这条是为了防"脚本自己写错了路径，
  // 于是把空集合报成全绿"。
  const written = results.filter((r) => existsSync(join(outDir, `${r.id}.txt`)));
  const problems = [];
  if (written.length !== results.length) {
    problems.push(`只写出了 ${written.length} / ${results.length} 份原始输出——拒绝在空集合上通过`);
  }
  const failed = results.filter((r) => !r.ok);
  if (failed.length > 0) {
    problems.push(`${failed.length} 项没绿：${failed.map((r) => r.id).join(', ')}`);
  }

  // 这份记录属于哪棵树。git 不可用 / 不在仓库里时两个字段是 null（未知）——
  // 绝不能因此崩掉：记录工具自己崩了，比少一个字段严重得多。
  const tree = judgeTree(runGit(['rev-parse', '--short=12', 'HEAD']), runGit(['status', '--porcelain']));

  const record = {
    schema: 1,
    milestone: args.milestone,
    title: spec.title,
    source: spec.source,
    generated_at: new Date().toISOString(),
    node: process.version,
    // 记录跑在哪个提交上、树干不干净。`dirty: null` 是"未知"，不是"干净"——
    // 排除 records/ 的理由见文件上方「这份记录属于哪一棵树」那段。
    commit: tree.commit,
    dirty: tree.dirty,
    criteria: results,
    green: problems.length === 0,
    exit_code: problems.length === 0 ? 0 : 1,
  };
  writeLf(join(outDir, 'acceptance.json'), `${JSON.stringify(record, null, 2)}\n`);

  console.log('');
  if (problems.length > 0) {
    for (const p of problems) console.error(`✗ ${p}`);
    console.error(`  记录 → records/${args.milestone}/acceptance.json`);
    return 1;
  }
  console.log(`✓ ${spec.title}：${results.length} 条退出标准全绿`);
  for (const r of results) {
    if (r.tests && r.tests.passed + r.tests.failed > 0) {
      console.log(`  ${r.id}：${r.tests.passed} passed / ${r.tests.failed} failed`);
    }
  }
  console.log(`  原始输出 → records/${args.milestone}/*.txt（stdout / stderr 分开写）`);
  console.log(`  记录 → records/${args.milestone}/acceptance.json`);
  return 0;
}

// 只设 process.exitCode，见 scripts/wasm-test-node-exit-shim.cjs 里记的 libuv 断言坑：
// 本工具的全部产物就是"退出码 + 落盘"，退出码不能是个概率事件。
process.exitCode = main();
