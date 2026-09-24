#!/usr/bin/env node
// 特效注册表守卫：**登记表与渲染实现必须一一对上**。
//
// 为什么需要它：S2/S3 之后，「加一个特效」变成「在登记表里加一项 + 写一个管线」。
// 那让加特效变便宜了，但也打开了一个新的失效模式：
//
//   * **登记了没实现** —— 用户能选中它、校验也过，渲染时却什么都不发生。
//     这最坏：画面看起来"正常"，只是特效没生效，没有任何一处报错。
//   * **实现了没登记** —— 用户填了这个 kind，校验直接判 unknown_effect。
//     工程师明明写了代码，用户却被告知"没有这个特效"。
//
// 两种都是"代码与声明不一致"，而这类不一致**不会自己暴露**。
//
// 判据：
//   1. 登记表里每个 kind，必须声明一条**渲染器真的实现**的管线；
//   2. 每个 kind 必须声明像素空间（否则缩放与否会变成静默偏差）；
//   3. 走 SeparableBlur 的特效必须有 radius 参数（否则管线拿不到核宽）；
//   4. 渲染侧**不许**再按 kind 字符串硬编码 —— 那是 S3 修掉的东西，回退要红。
//
// 用法：
//   node scripts/check-effect-registry.mjs             检查
//   node scripts/check-effect-registry.mjs --self-test 只跑守卫自检

import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');

const EFFECTS_RS = join(REPO_ROOT, 'crates', 'dhampir-core', 'src', 'effects.rs');
const TIMELINE_RS = join(REPO_ROOT, 'crates', 'dhampir-core', 'src', 'render', 'timeline.rs');

/** 渲染器真的实现了的管线变体（写在这里，因为它是"渲染侧的事实"）。 */
export const IMPLEMENTED_PIPELINES = ['SeparableBlur'];
/** 合法的像素空间。 */
export const VALID_SPACES = ['Source', 'Document'];
/**
 * 从 effects.rs 的 REGISTRY 抠出登记项：kind / pipeline / space / 有没有 radius。
 *
 * 不做完整 Rust 解析 —— 这里要的是**能变红**，不是通用。
 * 抠不到时返回空数组，上层据此判红（空集合不许通过）。
 */
export function parseRegistry(text) {
  const block = text.match(/pub const REGISTRY[^=]*=\s*&\[([^\]]*)\]/);
  if (!block) return { entries: [], error: '没找到 REGISTRY 数组' };
  const names = block[1].split(',').map((s) => s.trim()).filter((s) => s.length > 0);
  if (names.length === 0) return { entries: [], error: 'REGISTRY 是空的' };

  const entries = [];
  for (const name of names) {
    const re = new RegExp('pub const ' + name + '\\s*:\\s*EffectSpec\\s*=\\s*EffectSpec\\s*\\{([\\s\\S]*?)\\n\\};');
    const found = text.match(re);
    if (!found) {
      return { entries: [], error: 'REGISTRY 里列了 ' + name + '，但找不到它的 EffectSpec 定义' };
    }
    const body = found[1];
    const kind = (body.match(/kind:\s*"([^"]+)"/) || [])[1];
    if (!kind) return { entries: [], error: name + ' 没有 kind 字段' };
    entries.push({
      constName: name,
      kind,
      pipeline: (body.match(/pipeline:\s*EffectPipeline::(\w+)/) || [])[1] || null,
      space: (body.match(/space:\s*EffectSpace::(\w+)/) || [])[1] || null,
      hasRadius: /"radius"/.test(body),
    });
  }
  return { entries, error: null };
}
/** 纯函数判定：与自检同路。返回问题清单（空 = 通过）。 */
export function judge({ entries, renderText }) {
  const problems = [];

  if (entries.length === 0) {
    problems.push('登记表里一个特效都没有 —— 这条守卫会变成永远通过，先修登记表');
    return problems;
  }

  for (const entry of entries) {
    if (!entry.pipeline) {
      problems.push(entry.kind + ' 没有声明 pipeline —— 渲染器不知道该走哪条路');
    } else if (!IMPLEMENTED_PIPELINES.includes(entry.pipeline)) {
      problems.push(
        entry.kind + ' 声明了管线 ' + entry.pipeline + '，但渲染器没实现它（已实现：' +
        IMPLEMENTED_PIPELINES.join(' / ') + '）'
      );
    }
    if (!entry.space) {
      problems.push(entry.kind + ' 没有声明 space —— 缩放与否会变成静默偏差');
    } else if (!VALID_SPACES.includes(entry.space)) {
      problems.push(entry.kind + ' 声明了未知空间 ' + entry.space);
    }
    if (entry.pipeline === 'SeparableBlur' && !entry.hasRadius) {
      problems.push(entry.kind + ' 走 SeparableBlur 却没有 radius 参数 —— 管线拿不到核宽');
    }
  }

  // 渲染侧不许再按字符串名字硬编码特效 —— S3 的要点就是"认管线不认名字"。
  if (/effect\.kind\s*==\s*"/.test(renderText || '')) {
    problems.push(
      '渲染侧又出现了按 kind 字符串比较的写法 —— 加特效会被静默跳过，应当查注册表的 pipeline'
    );
  }

  return problems;
}
function runSelfTest() {
  let passed = 0;
  const expect = (name, problems, shouldBeEmpty) => {
    if ((problems.length === 0) !== shouldBeEmpty) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };
  const ok = [{ kind: 'gaussian_blur', pipeline: 'SeparableBlur', space: 'Document', hasRadius: true }];

  expect('正常登记 -> 通过', judge({ entries: ok, renderText: '' }), true);
  expect('空登记表 -> 必须红（否则守卫永远通过）', judge({ entries: [], renderText: '' }), false);
  expect(
    '没声明 pipeline -> 必须红',
    judge({ entries: [{ kind: 'x', pipeline: null, space: 'Document', hasRadius: true }], renderText: '' }),
    false
  );
  expect(
    '声明了没实现的管线 -> 必须红',
    judge({ entries: [{ kind: 'x', pipeline: 'ColorMatrix', space: 'Document', hasRadius: true }], renderText: '' }),
    false
  );
  expect(
    '没声明 space -> 必须红（静默偏差的来源）',
    judge({ entries: [{ kind: 'x', pipeline: 'SeparableBlur', space: null, hasRadius: true }], renderText: '' }),
    false
  );
  expect(
    '未知 space -> 必须红',
    judge({ entries: [{ kind: 'x', pipeline: 'SeparableBlur', space: 'Screen', hasRadius: true }], renderText: '' }),
    false
  );
  expect(
    'SeparableBlur 没有 radius -> 必须红',
    judge({ entries: [{ kind: 'x', pipeline: 'SeparableBlur', space: 'Document', hasRadius: false }], renderText: '' }),
    false
  );
  expect(
    '渲染侧按 kind 字符串比 -> 必须红（这是 S3 要防的回退）',
    judge({ entries: ok, renderText: 'if effect.kind == "gaussian_blur" { }' }),
    false
  );

  console.log('OK 特效注册表守卫自检通过（' + passed + ' 条断言）');
}
function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }

  if (!existsSync(EFFECTS_RS)) {
    console.error('  - 找不到 crates/dhampir-core/src/effects.rs');
    console.error('✗ 特效注册表守卫无法执行');
    process.exitCode = 1;
    return;
  }
  const text = readFileSync(EFFECTS_RS, 'utf8');
  const { entries, error } = parseRegistry(text);
  if (error) {
    console.error('  - ' + error);
    console.error('✗ 特效注册表守卫无法执行（登记表抠不出来，不许当成通过）');
    process.exitCode = 1;
    return;
  }

  const renderText = existsSync(TIMELINE_RS) ? readFileSync(TIMELINE_RS, 'utf8') : '';
  const problems = judge({ entries, renderText });

  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ 特效注册表与渲染实现不一致');
    process.exitCode = 1;
    return;
  }
  const names = entries.map((e) => e.kind).join(' / ');
  console.log(
    '✓ 特效注册表与渲染实现一致（' + entries.length + ' 个特效：' + names +
      '；管线与空间都已声明，渲染侧没有按名字硬编码）'
  );
}

main();