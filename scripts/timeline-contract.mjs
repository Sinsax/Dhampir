#!/usr/bin/env node
// 时间线契约的两个派生物：JSON Schema 与 TS 类型。
//
//   node scripts/timeline-contract.mjs --write      重新生成（改过 Rust 类型之后跑）
//   node scripts/timeline-contract.mjs              检查是否与 Rust 一致（守卫用法）
//   node scripts/timeline-contract.mjs --self-test  只跑本脚本的自检
//
// 为什么必须"生成 + 守卫"而不是手写 .d.ts：**手写的类型一定会漂**。
// 契约改一个字段、忘了改 TS，前端拿到的是「看起来对」的类型，直到运行时才炸。
// 所以 TS 从 Rust 类型推导（schemars 出 JSON Schema），并且每次守卫都重新推导一遍。

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
export const SCHEMA_PATH = join(REPO_ROOT, 'schema', 'timeline-v1.schema.json');
export const DTS_PATH = join(REPO_ROOT, 'schema', 'timeline-v1.d.ts');

/** 去掉 BOM。仓库约定是全仓无 BOM，而 PowerShell 的 Out-File 爱加一个。 */
export function stripBom(text) {
  return text.charCodeAt(0) === 0xfeff ? text.slice(1) : text;
}

/** 跑 Rust 侧的导例程，拿回 JSON Schema 文本。 */
export function schemaFromRust() {
  const out = execFileSync(
    'cargo',
    ['run', '-q', '-p', 'dhampir-timeline', '--features', 'json-schema', '--example', 'emit_schema'],
    { cwd: REPO_ROOT, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] },
  );
  return stripBom(out).trim() + '\n';
}

// ---------------------------------------------------------------------------
// JSON Schema -> TypeScript
// ---------------------------------------------------------------------------

/** 一个 JSON Schema 节点 -> TS 类型表达式。 */
export function tsType(node, defs) {
  if (node === undefined || node === null) return 'unknown';
  if (Array.isArray(node.anyOf) && node.anyOf.length > 0) {
    return [...new Set(node.anyOf.map((sub) => tsType(sub, defs)))].join(' | ');
  }
  if (Array.isArray(node.oneOf) && node.oneOf.length > 0) {
    return [...new Set(node.oneOf.map((sub) => tsType(sub, defs)))].join(' | ');
  }
  if (Array.isArray(node.enum)) {
    return node.enum.map((value) => JSON.stringify(value)).join(' | ');
  }
  if (node.const !== undefined) {
    // schemars 把带文档注释的枚举变体单独出成 const 分支（enum 数组里放不下描述）。
    // 少了这一支，`const: "subtitle"` 会掉进下面的 type 分支退化成 `string`，
    // 联合类型被 `| string` 吞掉——约束在 TS 侧就没了（TrackKind 上真实发生过）。
    return JSON.stringify(node.const);
  }
  if (typeof node.$ref === 'string') {
    const name = node.$ref.split('/').pop();
    return name;
  }
  if (Array.isArray(node.type)) {
    // schemars 有时用 ["integer","null"] 这种写法表达 Option。
    const parts = [...new Set(node.type.map((kind) => tsType({ ...node, type: kind }, defs)))];
    return parts.join(' | ');
  }
  switch (node.type) {
    case 'array': {
      const inner = tsType(node.items, defs);
      return inner.includes(' | ') ? '(' + inner + ')[]' : inner + '[]';
    }
    case 'object': {
      if (node.additionalProperties !== undefined && node.additionalProperties !== true) {
        const value = node.additionalProperties === false ? 'never' : tsType(node.additionalProperties, defs);
        return 'Record<string, ' + value + '>';
      }
      return 'Record<string, unknown>';
    }
    case 'integer':
    case 'number':
      return 'number';
    case 'string':
      return 'string';
    case 'boolean':
      return 'boolean';
    case 'null':
      return 'null';
    default:
      return 'unknown';
  }
}

/** 整个 schema -> .d.ts 文本。 */
export function emitDts(schema) {
  const defs = schema.$defs === undefined ? {} : schema.$defs;
  const lines = [
    '// 本文件由 scripts/timeline-contract.mjs 生成——**不要手改**。',
    '// 上游是 Rust 类型：crates/dhampir-timeline/src/schema.rs。',
    '//',
    '// 契约冻结在 schema v1；破坏性改动一律 +1（见 plan/video-editor-plan.md 的 M4 退出标准）。',
    '',
  ];
  for (const name of Object.keys(defs).sort()) {
    const node = defs[name];
    const isStringEnum =
      Array.isArray(node.enum) && node.enum.every((value) => typeof value === 'string');
    if (isStringEnum) {
      lines.push('export type ' + name + ' = ' + tsType(node, defs) + ';');
      lines.push('');
      continue;
    }
    const isObject =
      node.type === 'object' &&
      node.properties !== undefined &&
      node.additionalProperties === undefined;
    if (isObject) {
      lines.push('export interface ' + name + ' {');
      const required = new Set(Array.isArray(node.required) ? node.required : []);
      for (const key of Object.keys(node.properties).sort()) {
        const optional = required.has(key) ? '' : '?';
        const comment = typeof node.properties[key].description === 'string'
          ? node.properties[key].description.split('\n')[0]
          : '';
        if (comment !== '') lines.push('  /** ' + comment + ' */');
        lines.push('  ' + key + optional + ': ' + tsType(node.properties[key], defs) + ';');
      }
      lines.push('}');
      lines.push('');
      continue;
    }
    lines.push('export type ' + name + ' = ' + tsType(node, defs) + ';');
    lines.push('');
  }
  return lines.join('\n');
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

const ZOD = (condition, message) => {
  if (!condition) throw new Error(message);
};

function runSelfTest() {
  let passed = 0;
  const cases = [];

  cases.push(['引用', tsType({ $ref: '#/$defs/Clip' }, {}) === 'Clip']);
  cases.push(['数组', tsType({ type: 'array', items: { type: 'string' } }, {}) === 'string[]']);
  cases.push([
    '联合数组要加括号',
    tsType({ type: 'array', items: { anyOf: [{ type: 'string' }, { type: 'null' }] } }, {}) === '(string | null)[]',
  ]);
  cases.push([
    '整数与数字都是 number',
    tsType({ type: 'integer' }, {}) === 'number' && tsType({ type: 'number' }, {}) === 'number',
  ]);
  cases.push([
    'type 数组表达可选',
    tsType({ type: ['string', 'null'] }, {}) === 'string | null',
  ]);
  cases.push([
    '字符串枚举',
    tsType({ enum: ['a', 'b'] }, {}) === '"a" | "b"',
  ]);
  cases.push([
    'const 出字面量',
    tsType({ type: 'string', const: 'subtitle' }, {}) === '"subtitle"',
  ]);
  cases.push([
    'enum 与 const 混在 oneOf 里',
    tsType(
      {
        oneOf: [
          { type: 'string', enum: ['video', 'audio'] },
          { type: 'string', const: 'subtitle' },
        ],
      },
      {},
    ) === '"video" | "audio" | "subtitle"',
  ]);
  cases.push([
    '附加属性 -> Record',
    tsType({ type: 'object', additionalProperties: { type: 'number' } }, {}) === 'Record<string, number>',
  ]);
  cases.push(['BOM 要被剥掉', stripBom('\ufeff{}') === '{}' && stripBom('{}') === '{}']);

  const dts = emitDts({
    $defs: {
      Easing: { enum: ['linear', 'ease_in'] },
      Clip: {
        type: 'object',
        properties: { id: { type: 'string' }, duration: { type: 'integer' } },
        required: ['id'],
      },
    },
  });
  cases.push(['枚举出 type', dts.includes('export type Easing = "linear" | "ease_in";')]);
  cases.push(['对象出 interface', dts.includes('export interface Clip {')]);
  cases.push(['必填不带问号', dts.includes('  id: string;')]);
  cases.push(['可选出问号', dts.includes('  duration?: number;')]);

  for (const [name, ok] of cases) {
    if (!ok) throw new Error('自检失败：' + name);
    passed += 1;
  }
  console.log('✓ 契约脚本自检通过（' + passed + ' 条断言）');
}

function main() {
  const argv = process.argv.slice(2);
  if (argv.includes('--self-test')) {
    runSelfTest();
    return;
  }

  const write = argv.includes('--write');
  const fresh = schemaFromRust();

  if (write) {
    writeFileSync(SCHEMA_PATH, fresh);
    writeFileSync(DTS_PATH, emitDts(JSON.parse(fresh)));
    console.log('✓ 已重新生成 schema/timeline-v1.schema.json 与 schema/timeline-v1.d.ts');
    return;
  }

  const problems = [];
  if (!existsSync(SCHEMA_PATH)) {
    problems.push('缺少 schema/timeline-v1.schema.json（跑 --write 生成）');
  } else if (stripBom(readFileSync(SCHEMA_PATH, 'utf8')) !== fresh) {
    problems.push('schema/timeline-v1.schema.json 与当前 Rust 类型不一致——跑 --write');
  }
  if (!existsSync(DTS_PATH)) {
    problems.push('缺少 schema/timeline-v1.d.ts（跑 --write 生成）');
  } else {
    const fromCommitted = emitDts(JSON.parse(stripBom(readFileSync(SCHEMA_PATH, 'utf8'))));
    if (stripBom(readFileSync(DTS_PATH, 'utf8')) !== fromCommitted) {
      problems.push('schema/timeline-v1.d.ts 与 schema 不一致——跑 --write');
    }
  }

  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 契约派生物与 Rust 类型不同步');
    process.exitCode = 1;
    return;
  }
  console.log('✓ 时间线契约与派生物一致（schema + TS 类型）');
}

main();
