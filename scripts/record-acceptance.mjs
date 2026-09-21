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

  const record = {
    schema: 1,
    milestone: args.milestone,
    title: spec.title,
    source: spec.source,
    generated_at: new Date().toISOString(),
    node: process.version,
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
