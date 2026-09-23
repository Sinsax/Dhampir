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

/**
 * 判定规则。抽成纯函数是为了能喂一个**故意坏的**实现进来验它真的会红。
 *
 * async 是因为 mediaUrlFor 本来就是异步接口 —— 同步硬拆会让这里测的不是真形状。
 */
export async function judge(activeBackend) {
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

  // ---- 分离模式（remote） ----
  // **它与 local 是同一个工厂，只差 URL 从哪来。** 如果这两条路的客户端实现不同，
  // 那就是两套要各自维护的客户端 —— 所以这里验的是"地址来源"而不是别的。
  const remote = pick('?backend=remote&url=http://example.test:9000');
  if (remote.kind !== 'remote') problems.push('?backend=remote&url=... 应当切到分离模式，实际是 ' + remote.kind);
  if (String(remote.baseUrl) !== 'http://example.test:9000') {
    problems.push('remote 的 url 没有生效：' + remote.baseUrl);
  }
  // 末尾斜杠会被拼成 //projects，所以必须归一。
  if (String(pick('?backend=remote&url=http://example.test:9000/').baseUrl) !== 'http://example.test:9000') {
    problems.push('remote 的 url 末尾斜杠没有被归一');
  }
  // **没给 url 的 remote 必须退回降级**：猜一个地址比没有地址更危险 ——
  // 那会连到别的东西上而没人知道。
  if (pick('?backend=remote').kind !== 'static') problems.push('remote 没给 url 时应当退回降级，而不是猜一个地址');
  if (pick('?backend=remote&url=').kind !== 'static') problems.push('remote 给了空 url 时应当退回降级');

  // ---- 素材地址必须**按 asset id** 给 ----
  const mediaUrl = await custom.mediaUrlFor('a.mp4');
  if (mediaUrl !== 'http://127.0.0.1:9123/assets/a.mp4/media') {
    problems.push('mediaUrlFor 没有按 asset id 拼地址：' + mediaUrl);
  }
  // id 里可能有空格/中文/斜杠，必须转义 —— 不转义会拼出一个坏 URL，
  // 而表现是"这一路素材取不到"，很难联想到转义。
  const awkward = 'a b/名.mp4';
  const escaped = await custom.mediaUrlFor(awkward);
  if (!escaped.includes(encodeURIComponent(awkward))) {
    problems.push('asset id 没有被转义：' + escaped);
  }
  // 降级模式忽略 asset id（它只有一份素材）—— 这是它的**局限**，但要如实：忽略就说忽略。
  const fallbackMedia = await pick('').mediaUrlFor('whatever.mp4');
  if (fallbackMedia !== '/media/proxy.mp4') {
    problems.push('降级模式应当忽略 asset id 并给固定地址，实际是 ' + fallbackMedia);
  }

  return problems;
}

async function runSelfTest() {
  let passed = 0;
  const total = async (backend) => (await judge(backend)).length;
  const atLeast = async (name, count, backend) => {
    if ((await total(backend)) < count) throw new Error('自检失败：' + name);
    passed += 1;
  };
  const exactly = async (name, count, backend) => {
    const problems = await judge(backend);
    if (problems.length !== count) {
      throw new Error('自检失败：' + name + ' -> ' + JSON.stringify(problems));
    }
    passed += 1;
  };

  // 真的实现应当通过。
  await exactly('真的实现通过', 0, activeBackendFrom);

  // 故意坏掉的实现必须被抓住 —— 否则这个守卫可能是空转。
  //
  // **每个假实现都要给全接口**（含 mediaUrlFor）：只改被测的那一个方面。
  // 少给一个方法的话，judge 会在这里抛异常而不是判红 —— 那验的就不是判据了。
  const fake = (kind, baseUrl, projectId) => ({
    kind: kind,
    baseUrl: baseUrl,
    projectId: projectId,
    projectUrl: '/sample-project.doc.json',
    mediaUrl: '/media/proxy.mp4',
    mediaUrlFor: async (assetId) => baseUrl + '/assets/' + encodeURIComponent(assetId) + '/media',
  });

  await atLeast('永远返回本机 -> 抓住', 1, function () {
    return fake('local', 'http://127.0.0.1:1', 'x');
  });
  await atLeast('忽略 port -> 抓住', 1, function (search) {
    const real = activeBackendFrom(search);
    if (real.kind === 'local') return fake('local', 'http://127.0.0.1:8791', real.projectId);
    return real;
  });
  await atLeast('认不出的后端名也当本机 -> 抓住', 1, function (search) {
    if (String(search).includes('backend=weird')) return fake('local', 'http://127.0.0.1:8791', undefined);
    return activeBackendFrom(search);
  });
  await atLeast('remote 没 url 也猜一个地址 -> 抓住', 1, function (search) {
    if (String(search).includes('backend=remote')) return fake('remote', 'http://127.0.0.1:8791', undefined);
    return activeBackendFrom(search);
  });
  await atLeast('素材地址不按 id 给 -> 抓住', 1, function (search) {
    const real = activeBackendFrom(search);
    if (real.kind === 'static') return real;
    // 永远返回同一个地址：多素材工程会全部拿到同一路素材，而画面看起来"正常"。
    return {
      kind: real.kind, baseUrl: real.baseUrl, projectId: real.projectId,
      mediaUrlFor: async () => real.baseUrl + '/assets/proxy.mp4/media',
    };
  });
  await atLeast('素材 id 不转义 -> 抓住', 1, function (search) {
    const real = activeBackendFrom(search);
    if (real.kind === 'static') return real;
    return {
      kind: real.kind, baseUrl: real.baseUrl, projectId: real.projectId,
      mediaUrlFor: async (assetId) => real.baseUrl + '/assets/' + assetId + '/media',
    };
  });

  console.log('✓ backend 缝的守卫自检通过（' + passed + ' 条断言）');
}

async function main() {
  if (process.argv.includes('--self-test')) { await runSelfTest(); return; }
  const problems = await judge(activeBackendFrom);
  if (problems.length > 0) {
    for (const problem of problems) console.error('  - ' + problem);
    console.error('✗ web/backend.js 的判定点不满足约定');
    process.exitCode = 1;
    return;
  }
  console.log('✓ backend 缝成立（降级为默认、本机与分离可切、端口与工程 id 可覆盖、素材按 asset id 取）');
}

// **自检抛异常也要看得见。** 不接住的话它只会变成一个 UnhandledPromiseRejection，
  // 而退出码背后的原因就丢了 —— 那正是"运行失败"与"判据不通过"分不清的老毛病。
main().catch((error) => {
  console.error('守卫自己跑挂了：' + String(error && error.stack ? error.stack : error));
  process.exitCode = 2;
});
