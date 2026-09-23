#!/usr/bin/env node
// web/backend.js 那条缝的守卫。
//
// 为什么需要它：**「后端在哪」是三种部署形态里唯一的判定点**。
// 判定错了的表现不是崩，而是"页面看起来能用但拿的是别处的数据" ——
// 那种错几乎不可能靠看发现。所以这个判定点要有测试。
//
// 这个守卫**不进浏览器**：它直接把 web/backend.js 当模块 import 进来测。
// backend.js 只用了 fetch 与 URLSearchParams，Node 里都有。
//
// 用法：
//   node scripts/check-backend-seam.mjs             检查
//   node scripts/check-backend-seam.mjs --self-test 只跑守卫自检

import { activeBackendFrom } from '../web/backend.js';

/** 判定规则。抽成纯函数是为了能喂一个**故意坏的**实现进来验它真的会红。 */
export function judge(activeBackend) {
  const problems = [];

  const pick = (search) => activeBackend(search);

  // 默认必须是**降级模式** —— 没有后端也要能用。
  const fallback = pick('');
  if (fallback.kind !== 'static') {
    problems.push('没有查询参数时应当是降级模式（static），实际是 ' + fallback.kind);
  }
  if (!fallback.projectUrl || !fallback.mediaUrl) {
    problems.push('降级模式必须自带工程与素材地址，否则断网就没有任何东西可用');
  }

  // 无关参数不该改变判定。
  if (pick('?foo=1').kind !== 'static') problems.push('无关参数影响了判定');
  // 认不出来的后端名应当**退回降级**，而不是猜。
  if (pick('?backend=weird').kind !== 'static') problems.push('认不出的 backend 应当退回降级模式');

  // 显式指定本机模式。
  const local = pick('?backend=local');
  if (local.kind !== 'local') problems.push('?backend=local 应当切到本机模式');
  if (!String(local.baseUrl).startsWith('http://127.0.0.1:')) {
    problems.push('本机模式必须指向 127.0.0.1，实际是 ' + local.baseUrl);
  }
  // 端口要能覆盖，否则同时跑两个实例时会连错。
  const custom = pick('?backend=local&port=9123');
  if (!String(custom.baseUrl).endsWith(':9123')) {
    problems.push('port 参数没有生效：' + custom.baseUrl);
  }
  // 工程 id 要能带过来 —— 不带就会拼出 /projects/undefined。
  const withProject = pick('?backend=local&project=demo');
  if (withProject.projectId !== 'demo') {
    problems.push('project 参数没有生效：' + withProject.projectId);
  }

  return problems;
}

function runSelfTest() {
  let passed = 0;
  const total = (backend) => judge(backend).length;
  const atLeast = (name, count, backend) => {
    if (total(backend) < count) throw new Error('自检失败：' + name);
    passed += 1;
  };
  const exactly = (name, count, backend) => {
    if (total(backend) !== count) throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(judge(backend)));
    passed += 1;
  };

  // 真的实现应当通过。
  exactly('真的实现通过', 0, activeBackendFrom);

  // 故意坏掉的实现必须被抓住 —— 否则这个守卫可能是空转。
  atLeast('永远返回本机 -> 抓住', 1, function () {
    return { kind: 'local', baseUrl: 'http://127.0.0.1:1', projectId: 'x' };
  });
  atLeast('忽略 port -> 抓住', 1, function (search) {
    const real = activeBackendFrom(search);
    if (real.kind === 'local') return { kind: 'local', baseUrl: 'http://127.0.0.1:8791', projectId: real.projectId };
    return real;
  });
  atLeast('认不出的后端名也当本机 -> 抓住', 1, function (search) {
    if (String(search).includes('backend=weird')) {
      return { kind: 'local', baseUrl: 'http://127.0.0.1:8791', projectId: undefined };
    }
    return activeBackendFrom(search);
  });

  console.log('✓ backend 缝的守卫自检通过（' + passed + ' 条断言）');
}

function main() {
  if (process.argv.includes('--self-test')) { runSelfTest(); return; }
  const problems = judge(activeBackendFrom);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ web/backend.js 的判定点不满足约定');
    process.exitCode = 1;
    return;
  }
  console.log('✓ backend 缝成立（降级为默认、本机可切、端口与工程可覆盖）');
}

main();
