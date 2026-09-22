#!/usr/bin/env node
// 驱动一轮**浏览器腿**的 corpus 取证：起本地服务 → 无头 Chrome 打开取证页 → 等这一轮
// 跑完 → 读服务端的判定 → 落截图与旁证 JSON。
//
// 为什么要有这个脚本，而不是"打开页面点一下运行"：
//
//  1. 手点没法复跑。半年后有人问"records/m2 里的 80 张 PNG 是哪一次、哪个浏览器、
//     哪块卡跑出来的"，手点只能回答"不知道"。这里每次跑都覆盖同一批文件名，
//     旁证 JSON 里记下 Chrome 版本、页面 URL、服务端判定、截图摘要。
//  2. **判定不在脚本里**。这一轮的事实（每一帧的路径/字节数/FNV-1a 摘要、账本与明细
//     自洽、与 native 的字节关系）由 `serve-corpus-harness.mjs` 在落盘前逐条复算；
//     本脚本只做三件事：把页面跑起来、等它对完账、把结论收进记录。判定散成两份，
//     就会在"哪一份说了算"上打架。
//  3. **但脚本要独立复核两条**：盘上真实的帧数、以及服务端与页面自己在"写了多少帧"
//     上的说法。全都信服务端的话，服务端自己写错了就没人发现。同理还有"送出去多少字节"：
//     页面的 `text.length` 是 UTF-16 码元数，曾经把 112215 字节的 readings.txt 报成 78791，
//     所以那个数只许来自写文件的那一侧，再由这里与盘上文件对一遍。
//  4. 截图会骗人。所以硬条件（服务端完整性全过、帧数三方一致、页面没抛异常）任一条
//     不成立时**不写 PNG**——宁可没有这张图，也不要一张"看起来全绿"的图。而 M2 要量
//     的**发现**（与 native 是否逐字节相同、同帧两次是否一致）照写、如实标注：
//     它们不是错误，是这一轮的结论。
//  5. **"用了哪块卡"这件事浏览器答不出来**：Chrome 的 `GPUAdapterInfo` 只给
//     `vendor`/`architecture`，`name` 与 `description` 是空串（实测见 `target/m2-gpu-probe.mjs`
//     的结论，也写在 wasm 侧的 `IN_PAGE_NOTE` 里）。能补这一环的只有宿主——所以本脚本
//     从 CDP `SystemInfo.getInfo` 读宿主设备表，POST 给服务端，服务端把它与页面报的
//     `in_page.vendor` 对上，写出 `host-gpu.json`。这张表**必须先于页面往外送 adapter.json
//     到达服务端**：解卡身份要用页面报的 vendor 当匹配起点，而页面一加载就会往外送。所以
//     本脚本把 URL 里的 `?autorun` 摘掉，等设备表送进去了再自己触发这一轮——顺序是确定的，
//     不是和页面加载抢出来的。
//
// 用法：
//   node scripts/run-browser-corpus.mjs                       # 默认跟 records/m1/dx12 比
//   node scripts/run-browser-corpus.mjs --out records/m2/browser --leg m2
//   node scripts/run-browser-corpus.mjs --headed              # 无头下拿不到 WebGPU 时试这个
//   node scripts/run-browser-corpus.mjs --chrome-arg --force_low_power_gpu   # T2.6 选核显
//   node scripts/run-browser-corpus.mjs --self-test
//
// 退出码：0 完整性全过（发现里的差异不算失败）；1 硬条件没过；2 参数或环境问题。

import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  Cdp,
  buildBrowserArgs,
  fetchPageTarget,
  findBrowser,
  freePort,
  sleep,
  waitForOccupiedPort,
  withTimeout,
} from './capture-harness-screenshot.mjs';

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WWW_ROOT = join(REPO_ROOT, 'crates', 'dhampir-wasm', 'www');

const USAGE = `用法：node scripts/run-browser-corpus.mjs [选项]

选项：
  --out <dir>        这条腿的产物目录（默认 records/m2/browser；服务端会写进去）
  --leg <slug>       腿名（默认 m2；必须是 [a-z0-9-]，它同时是 adapter.json 的 backend_slug）
  --native <dir>     native 腿目录（默认 records/m1/dx12）：从它的 run.json 取期望摘要，
                     并把两条腿的 readings.txt 摆在一起比；给 none 表示不比
  --expect <hex>     直接给期望的 frames_digest（与 --native 二选一）
  --chrome-arg <a>   追加一条浏览器命令行参数（可重复）。T2.6 选核显用 --force_low_power_gpu
  --timeout <sec>    等页面跑完的上限（默认 300）
  --width <n>        窗口宽度（默认 1600）
  --height <n>       窗口高度（默认 1200）
  --headed           不用无头模式
  --keep-profile     结束时不删 Chrome profile 目录（排查用）
  --self-test        只跑本脚本的自检，不启动浏览器与服务
  -h, --help         显示本帮助

环境变量：
  DHAMPIR_CHROME     指定浏览器可执行文件（默认按常见路径找 Chrome，再退到 Edge）

说明：
  URL 里的 ?autorun 会被本脚本摘掉：这一轮由脚本在宿主设备表送到之后**自己触发**
  （顺序确定，不靠和页面加载抢时间）。手工复跑时直接用服务端横幅上那条带 autorun 的 URL。`;

// ---------------------------------------------------------------------------
// 宿主设备表（"用了哪块卡"这件事浏览器答不出来，得由宿主补）
// ---------------------------------------------------------------------------

/**
 * 宿主设备表的**来源声明**。
 *
 * 这个字符串是**边界上的第二份**（第一份在 `serve-corpus-harness.mjs` 的 `HOST_GPU_SOURCE`，
 * 那里是判据所在），所以它必须能被证伪：写错了服务端直接 HTTP 400 拒收整轮、记录里不会
 * 出现一个来源不明的卡——**不会静默漂**。这也正是不能在这里 import 服务端那个常量的原因：
 * 服务端脚本没有 import 保护，`import` 它会把服务端**真的起起来**。
 */
export const HOST_GPU_SOURCE = 'CDP SystemInfo.getInfo';

/**
 * 把 CDP `SystemInfo.getInfo` 的 `gpu` 段整理成要 POST 给服务端的宿主设备表。**纯函数**。
 *
 * 整理到哪一步为止是有讲究的：这里**只做两件驱动有资格做的事**——把上游给的东西原样
 * 搬过来、以及在"上游没给出可用的表"时把话说清楚（那是**这台机器/这个浏览器**的问题，
 * 不该报成"服务端拒收"）。**形状判定不在这里做**：一块卡够不够格写进记录、来源对不对、
 * 解出来的卡是不是唯一，全部由服务端一个地方说了算（见 `validateHostGpuText`/`matchHostGpu`）。
 * 两边各判一遍的话，漂开的那天没人知道该信谁。
 *
 * 搬过来的字段是原样保留的：`deviceString` 是最终要写进记录的那句话，`driverVendor`/
 * `driverVersion` 是"哪个驱动"这个问题的答案；`vendorString` 实测是空串，但**照样留着**——
 * 它是上游给的事实，抹掉它会让将来的人以为 CDP 从来不给这一栏。
 */
export function hostGpuPayload(gpu) {
  const devices = gpu?.devices;
  if (!Array.isArray(devices) || devices.length === 0) {
    return { error: 'CDP SystemInfo.getInfo 没给出 GPU 设备表（gpu.devices = '
      + `${JSON.stringify(devices)}）：这份记录答不出"用了哪块卡"，而那是它的全部意义` };
  }
  return { source: HOST_GPU_SOURCE, devices };
}

/**
 * 把 URL 里的 `?autorun` 摘掉，并说明摘没摘。**纯函数**（自检直接喂串）。
 *
 * 摘它的理由见文件头第 5 条：这一轮什么时候开跑是**驱动**的决定（设备表得先送到），
 * 不是页面的。没有 `autorun` 时原样返回——那不是错误，只是这一次不需要摘。
 */
export function suppressAutorun(url) {
  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return { url, removed: false, error: `服务端横幅里的 URL 不是合法 URL：${JSON.stringify(url)}` };
  }
  if (!parsed.searchParams.has('autorun')) return { url, removed: false };
  parsed.searchParams.delete('autorun');
  return { url: parsed.toString(), removed: true };
}

// ---------------------------------------------------------------------------
// 解析服务端的两行横幅与判定行
// ---------------------------------------------------------------------------

/**
 * 从服务端输出里抠出 `PORT <n>`、`URL <url>`、`腿名 <slug>`、`产物 <dir>`。
 *
 * 为什么从输出里读端口，而不是在这里先挑一个空闲端口传进去：`--port 0` 让系统挑，
 * 挑中的那个端口**一定没有被别人占**；而"先 freePort() 再 spawn"之间有一段窗口，
 * 同一台机器上别的进程可能正好把它拿走（本仓已被这件事咬过一次：两个 freePort()
 * 吐出同一个号，Chrome 绑不上调试端口却报"连不上 DevTools"）。
 */
export function parseServerBanner(text) {
  const port = /^PORT (\d+)$/m.exec(text);
  const url = /^URL (http:\/\/127\.0\.0\.1:\d+\/\S*)$/m.exec(text);
  const leg = /^  腿名    (\S+)$/m.exec(text);
  const out = /^  产物    (\S+)$/m.exec(text);
  if (!port || !url) return null;
  return {
    port: Number(port[1]),
    url: url[1],
    leg: leg ? leg[1] : null,
    out_dir: out ? out[1] : null,
  };
}

/** 判定行：`VERDICT {...}`。最后一条说了算（同一次运行只会打一条，但重跑会再打）。 */
export function parseVerdict(text) {
  const matches = [...text.matchAll(/^VERDICT (\{.*\})$/gm)];
  if (matches.length === 0) return null;
  try {
    return JSON.parse(matches[matches.length - 1][1]);
  } catch {
    return null;
  }
}

// ---------------------------------------------------------------------------
// 判定
// ---------------------------------------------------------------------------

/**
 * 这一轮的硬条件与发现。**纯函数**，自检直接喂样本。
 *
 * `hard` 非空 → 物证不可用 → 不写截图，退出码 1。
 * `findings` 是 M2 要量的事实：与 native 是否逐字节相同、同帧两次是否一致、
 * 表里的采样点是否全过。它们**不影响**这一轮"取证成功"与否——M2 要找的就是差异。
 *
 * `hostGpu` 是"用了哪块卡"这一环的三份物证（驱动读到的表、那一口的回话、盘上那份
 * `host-gpu.json`）。它**没有默认值**：这一环是浏览器腿记录的全部意义所在，缺了它
 * 不该被沉默地跳过，只该红。三份都在时，这里至少要能回答"服务端说用了的那块卡，
 * 真的在驱动读到的那张表里"——服务端自己写错时，只有这一步会发现。
 *
 * `onDiskBytes` 是三个文本产物在**盘上的字节数**（驱动自己 `stat` 的），同样没有默认值。
 * 它钉的是"这一轮送出去多少字节"这句话：页面与判定各报一个数，盘上是另一个数，
 * 三个数得是同一个。这条检查是一个真缺陷的回声——页面曾拿 `text.length`（UTF-16 码元数）
 * 当字节数，`readings.txt` 被报成 78791 字节而真身是 112215 字节，字段名还写着 `_bytes`。
 */
export function auditRun({ serverVerdict, pageState, framesOnDisk, hostGpu = null, onDiskBytes = null }) {
  const hard = [];
  const findings = [];

  if (serverVerdict === null) {
    hard.push('服务端没有给出 VERDICT——记录没通过完整性检查，或者根本没走到落盘那一步');
  } else if (serverVerdict.ok !== true) {
    const failed = (serverVerdict.checks ?? []).filter((item) => !item.ok).map((item) => item.detail);
    hard.push(`服务端的完整性判定没过：${serverVerdict.error ?? '（没给原因）'}`);
    for (const detail of failed) hard.push(`  ✗ ${detail}`);
  }
  if (pageState === null) {
    hard.push('页面没给出结论对象（dhampirCorpusHarness.done 没 resolve 出 state）');
  } else {
    if (pageState.error) hard.push(`页面抛了异常：${pageState.error}`);
    if (pageState.frames_written === undefined) {
      hard.push('页面里有结论，但一帧都没写出去（frames_written 缺失）');
    }
  }

  // 三方对账：页面自报写了多少 / 服务端复核了多少 / 盘上到底有多少。
  // 只信一份就会在"服务端自己写错了"时无人发现。
  const written = pageState?.frames_written ?? null;
  const verified = serverVerdict?.frames_verified ?? null;
  if (verified !== null && verified <= 0) hard.push(`服务端复核了 ${verified} 帧——空集合上不许宣布通过`);
  if (written !== null && verified !== null && written !== verified) {
    hard.push(`页面自报写了 ${written} 帧，服务端复核的是 ${verified} 帧`);
  }
  if (framesOnDisk !== null && verified !== null && framesOnDisk !== verified) {
    hard.push(`盘上有 ${framesOnDisk} 张 PNG，服务端复核的是 ${verified} 帧`);
  }
  if (framesOnDisk === 0) hard.push('产物目录里一张 PNG 都没有');

  // ---- 文本产物"写了多少字节"：页面自报 / 判定回话 / 盘上文件，三处一个数 ----
  //
  // 帧那边的三方对账管的是"有几帧"，这里管的是"多少字节"。两件事都不许只信一份：
  // 页面报的数是它**转述**服务端的，判定里的数是服务端自己算的，盘上那个数才是事实。
  if (onDiskBytes === null) {
    hard.push('没给盘上文本产物的字节数——"这一轮送出去多少字节"没人验过（不许沉默跳过）');
  } else {
    const verdictBytes = serverVerdict?.bytes ?? null;
    const pairs = [
      ['adapter.json', pageState?.adapter_bytes ?? null, onDiskBytes.adapter ?? null],
      ['readings.txt', pageState?.readings_bytes ?? null, onDiskBytes.readings ?? null],
      ['run.json', pageState?.run_json_bytes ?? null, onDiskBytes.runJson ?? null],
    ];
    for (const [name, claimed, actual] of pairs) {
      if (actual === null) {
        hard.push(`盘上没有 ${name}——这一轮的文本产物不齐`);
      } else if (!Number.isInteger(claimed) || claimed < 0) {
        hard.push(`页面没说 ${name} 送出去多少字节（${JSON.stringify(claimed)}）`);
      } else if (claimed !== actual) {
        hard.push(`页面说 ${name} 是 ${claimed} 字节，盘上是 ${actual} 字节`);
      }
    }
    if (verdictBytes !== null && onDiskBytes.runJson !== null && verdictBytes !== onDiskBytes.runJson) {
      hard.push(`判定说 run.json 写了 ${verdictBytes} 字节，盘上是 ${onDiskBytes.runJson} 字节`);
    }
    if (pairs.every(([, claimed, actual]) => Number.isInteger(claimed) && claimed === actual)) {
      findings.push({
        ok: true,
        detail: `文本产物字节数：页面自报与盘上文件一致（adapter.json ${onDiskBytes.adapter}、`
          + `readings.txt ${onDiskBytes.readings}、run.json ${onDiskBytes.runJson}）`,
      });
    }
  }

  // ---- 卡身份：驱动读到的表 / 服务端的回话 / 盘上那份文件，三处必须说同一件事 ----
  if (hostGpu === null) {
    hard.push('没给宿主设备表这一环的三份物证——不许沉默跳过（"用了哪块卡"是这份记录的全部意义）');
  } else {
    const posted = hostGpu.posted ?? null;
    const post = hostGpu.post ?? null;
    const onDisk = hostGpu.onDisk ?? null;
    if (posted === null) {
      hard.push('驱动器没能从 CDP 读到宿主设备表（`SystemInfo.getInfo`），这份记录说不出"用了哪块卡"');
    } else if (post === null) {
      hard.push('宿主设备表没有送到服务端（那一口没发出去）');
    } else if (post.status !== 200) {
      hard.push(`宿主设备表被服务端拒收（HTTP ${post.status}）：${post.body?.error ?? '（没给原因）'}`);
    }
    if (onDisk === null) {
      hard.push(`盘上没有 ${HOST_GPU_FILE_ON_RECORD}——adapter.json 里那句 "答案在这个文件里" 就成了空话`);
    } else if (posted !== null) {
      if (onDisk.source !== posted.source) {
        hard.push(`${HOST_GPU_FILE_ON_RECORD} 的来源是 ${JSON.stringify(onDisk.source)}，`
          + `驱动读到的是 ${JSON.stringify(posted.source)}`);
      }
      const onDiskIds = (onDisk.devices ?? []).map((device) => device?.deviceId);
      const missing = (posted.devices ?? [])
        .map((device) => device?.deviceId)
        .filter((id) => !onDiskIds.includes(id));
      if (missing.length > 0) {
        hard.push(`${HOST_GPU_FILE_ON_RECORD} 里少了驱动读到的卡（少了 deviceId ${missing.join('、')}）`
          + '——盘上那份表不是这一轮读到的表');
      }
    }
    // 服务端说"用的是这块卡"，那就得在驱动读到的表里真能找到它。
    const resolvedId = onDisk?.resolved?.device_id ?? null;
    if (resolvedId !== null && posted !== null) {
      const ids = (posted.devices ?? []).map((device) => device?.deviceId);
      if (!ids.includes(resolvedId)) {
        hard.push(`服务端说这一轮用的是 deviceId ${resolvedId}（${onDisk?.resolved?.device}），`
          + `但驱动从宿主读到的表里没有这块卡（表里是 ${ids.join('、')}）`);
      }
    }
    const verdictId = serverVerdict?.host_gpu?.device_id ?? null;
    if (resolvedId !== null && verdictId !== null && resolvedId !== verdictId) {
      hard.push(`判定里说用的是 deviceId ${verdictId}，盘上 ${HOST_GPU_FILE_ON_RECORD} 里写的是 `
        + `${resolvedId}——同一个问题两个答案`);
    }
    if (resolvedId !== null && verdictId === null && serverVerdict?.ok === true) {
      hard.push('判定里没有 host_gpu 这一栏，而盘上有卡身份——判定没说这件事，就不算说过');
    }
    if (onDisk?.resolved?.device) {
      findings.push({
        ok: true,
        detail: `卡身份：${onDisk.resolved.device}（deviceId ${onDisk.resolved.device_id}，`
          + `由宿主设备表补上：${onDisk.resolved.match_reason}）`,
      });
    }
  }

  if (serverVerdict !== null) {
    for (const item of serverVerdict.findings ?? []) {
      findings.push({ ok: item.ok ?? null, detail: item.detail });
    }
    if (Array.isArray(serverVerdict.checks) && serverVerdict.ok === true) {
      findings.push({
        ok: true,
        detail: `服务端 ${serverVerdict.checks.length} 条完整性检查全过（帧数与服务端/页面/盘上三方一致）`,
      });
    }
  }
  return { ok: hard.length === 0, hard, findings };
}

/**
 * 盘上那份卡身份文件的文件名。**又一处边界上的第二份**（判据在服务端的 `HOST_GPU_FILE`，
 * 也是 wasm 侧 `unresolved_identity().resolves_to` 的值）。写错了的表现是这里找不到文件、
 * 判定直接红——不会静默漂。
 */
const HOST_GPU_FILE_ON_RECORD = 'host-gpu.json';

/**
 * 旁证里"用了哪块卡"那一栏的名字。**纯函数**（自检直接喂样本）。
 *
 * 为什么不能直接抄 `adapter.adapter.name`：Chrome 的 `GPUAdapterInfo.name` 是**空串**
 * （见文件头第 5 条）。空串不是名字——"读不出名字"这件事在整份记录里只有**一种**写法，
 * 就是 `null`（`adapter.json` 的 `adapter_name`、`run.json` 的 `backends[0].adapter_name`
 * 都这么写，服务端还会拿这两处互相印证）。旁证里若把空串当名字用，同一件事就有了两种
 * 说法：读的人分不清"这块卡没名字"与"脚本没去问"。
 *
 * 所以顺序是：页面报了**真名字**就用它；否则用服务端结算出来的那一栏；再否则 `null`。
 */
export function legAdapterName(adapterDump, serverVerdict) {
  const raw = adapterDump?.adapter?.name;
  if (typeof raw === 'string' && raw !== '') return raw;
  const settled = serverVerdict?.adapter_name;
  return typeof settled === 'string' && settled !== '' ? settled : null;
}

// ---------------------------------------------------------------------------
// 自检
// ---------------------------------------------------------------------------

export function selfTest() {
  const cases = [];
  const check = (name, ok) => cases.push({ name, ok: Boolean(ok) });

  // ---- 横幅 --------------------------------------------------------------
  const banner = [
    'dhampir M2 浏览器腿（corpus 取证）',
    '  页面    F:/para/Code/Dhampir/crates/dhampir-wasm/www',
    '  产物    F:/para/Code/Dhampir/records/m2/browser',
    '  腿名    m2',
    'PORT 52134',
    'URL http://127.0.0.1:52134/corpus.html?leg=m2&scene=all&frames=0..16&autorun=1&expect=71ecc80cade3d73d',
  ].join('\n');
  const parsed = parseServerBanner(banner);
  check('从横幅里读出端口', parsed?.port === 52134);
  check('从横幅里读出 URL', parsed?.url.endsWith('&expect=71ecc80cade3d73d'));
  check('从横幅里读出腿名', parsed?.leg === 'm2');
  check('从横幅里读出产物目录', parsed?.out_dir === 'F:/para/Code/Dhampir/records/m2/browser');
  check('没有 PORT 行就不认（宁可判死也不猜端口）', parseServerBanner('dhampir M2 浏览器腿') === null);
  check('只有 PORT 没有 URL 也不认', parseServerBanner('PORT 1234') === null);

  // ---- 判定行 ------------------------------------------------------------
  const verdictText = [
    '  ✓ 记录身份：kind=corpus schema=1 milestone=M1（表契约版本）',
    '✓ run.json（305100 字节）——完整性检查全过',
    'VERDICT {"ok":true,"phase":"run","leg":"m2","frames_verified":80}',
  ].join('\n');
  check('从输出里读出判定', parseVerdict(verdictText)?.frames_verified === 80);
  check('没有判定行就返回 null，不编一个', parseVerdict('VERDICT 不是 JSON') === null);
  check('判定行取最后一条（重跑过就以最后一次为准）',
    parseVerdict(`VERDICT {"frames_verified":1}\nVERDICT {"frames_verified":80}`)?.frames_verified === 80);

  // ---- 判定核心：好样本 + 每一条硬条件 -----------------------------------
  const goodVerdict = {
    ok: true,
    leg: 'm2',
    frames_verified: 80,
    frames_digest: '71ecc80cade3d73d',
    expected_frames_digest: '71ecc80cade3d73d',
    digest_matches_expect: true,
    counts: { frames: 80, points: 368, failed: 0, out_of_range: 0, unjudged: 0, clean: true },
    checks: [{ ok: true, detail: '记录身份' }, { ok: true, detail: '盘上 80 帧全部一致' }],
    findings: [{ ok: true, detail: '同帧两次渲染：80 帧全部逐字节一致' }],
    host_gpu: { vendor: 'nvidia', vendor_id: 0x10de, device_id: 10118, device: 'NVIDIA GeForce RTX 4070' },
    bytes: 305089,
  };
  // 三个数字照着 M2 那一轮实测抄（adapter.json 884 / readings.txt 112215 / run.json 305089）：
  // 它们必须与 `goodOnDiskBytes` 一致——这两份是"页面转述的"与"盘上真有的"两个来源。
  const goodPage = {
    ok: true, frames_written: 80, failures: 0, counts: goodVerdict.counts,
    adapter_bytes: 884, readings_bytes: 112215, run_json_bytes: 305089,
  };
  const goodOnDiskBytes = { adapter: 884, readings: 112215, runJson: 305089 };
  // 宿主设备表那三份物证。表的形状照 CDP 实测（本机三块：NVIDIA 独显 + AMD 核显 + Microsoft
  // 基本显示适配器），**两块厂商都在**：只放 NVIDIA 一块的话，"服务端说用了的那块卡在不在
  // 驱动读到的表里"这条检查靠"表里只有一块"也能蒙过去。
  const hostDevices = [
    { vendorId: 0x10de, deviceId: 10118, deviceString: 'NVIDIA GeForce RTX 4070',
      driverVendor: '', driverVersion: '32.0.16.1074', vendorString: '' },
    { vendorId: 0x1002, deviceId: 5056, deviceString: 'AMD Radeon(TM) Graphics',
      driverVendor: '', driverVersion: '32.0.21030.2001', vendorString: '' },
    { vendorId: 0x1414, deviceId: 5140, deviceString: 'Microsoft Basic Render Driver',
      driverVendor: '', driverVersion: '10.0.19041.1', vendorString: '' },
  ];
  const goodHostGpu = {
    posted: hostGpuPayload({ devices: hostDevices }),
    // 设备表先到，此刻页面报的 vendor 还没来——服务端只收不判（与接线自检同一份契约）。
    post: { status: 200, body: { ok: true, devices: 3, written: false, host_gpu: null } },
    onDisk: {
      source: HOST_GPU_SOURCE,
      devices: hostDevices,
      resolved: { vendor: 'nvidia', vendor_id: 0x10de, device_id: 10118,
        device: 'NVIDIA GeForce RTX 4070', driver_version: '32.0.16.1074',
        match_reason: '页面报的是 nvidia，表里唯一一块' },
    },
  };
  const good = auditRun({
    serverVerdict: goodVerdict, pageState: goodPage, framesOnDisk: 80, hostGpu: goodHostGpu,
    onDiskBytes: goodOnDiskBytes,
  });
  check('全绿的一轮判过', good.ok === true);
  check('全绿的一轮也把发现带出来（不吞）', good.findings.length >= 2);
  check('全绿的一轮里卡身份落在发现里（"用了哪块卡"要看得见）',
    good.findings.some((item) => item.detail.includes('NVIDIA GeForce RTX 4070')
      && item.detail.includes('卡身份')));
  check('全绿的一轮里字节数落在发现里（三个文本产物各是多少要看得见）',
    good.findings.some((item) => item.detail.includes('112215') && item.detail.includes('305089')));

  const hardCases = [
    ['服务端没给判定不让过', { serverVerdict: null }, 'VERDICT'],
    ['服务端判定不过不让过', { serverVerdict: { ...goodVerdict, ok: false, error: '3 项完整性检查没通过' } }, '完整性判定没过'],
    ['服务端判定不过时把失败条目也带出来',
      { serverVerdict: { ...goodVerdict, ok: false, error: 'x', checks: [{ ok: false, detail: '盘上没有这个文件' }] } },
      '盘上没有这个文件'],
    ['页面没结论不让过', { pageState: null }, 'done'],
    ['页面抛异常不让过', { pageState: { ...goodPage, error: 'no adapter' } }, '页面抛了异常'],
    ['一帧都没写不让过', { pageState: { ...goodPage, frames_written: undefined } }, '一帧都没写'],
    ['页面与服务端帧数不一致不让过', { pageState: { ...goodPage, frames_written: 79 } }, '页面自报写了 79 帧'],
    ['盘上帧数少于复核数不让过', { framesOnDisk: 79 }, '盘上有 79 张 PNG'],
    ['盘上一张都没有不让过', { framesOnDisk: 0 }, '一张 PNG 都没有'],
    ['服务端复核 0 帧不让过',
      { serverVerdict: { ...goodVerdict, frames_verified: 0 }, pageState: { ...goodPage, frames_written: 0 }, framesOnDisk: 0 },
      '空集合上不许宣布通过'],
    // ---- 文本产物的字节数：这一组对着上面那条真缺陷写 ------------------------
    ['页面拿码元数当字节数不让过（readings.txt 报 78791、真身 112215）',
      { pageState: { ...goodPage, readings_bytes: 78791 } }, 'readings.txt 是 78791 字节'],
    ['盘上字节数与页面自报不符不让过',
      { onDiskBytes: { ...goodOnDiskBytes, runJson: 305088 } },
      'run.json 是 305089 字节，盘上是 305088 字节'],
    ['页面没说送出去多少字节不让过',
      { pageState: { ...goodPage, run_json_bytes: null } }, '页面没说 run.json 送出去多少字节'],
    ['没给盘上字节数不让过（不许沉默跳过）', { onDiskBytes: null }, '没人验过'],
    ['盘上缺一份文本产物不让过',
      { onDiskBytes: { ...goodOnDiskBytes, adapter: null } }, '盘上没有 adapter.json'],
    ['判定回的字节数与盘上不一致不让过',
      { serverVerdict: { ...goodVerdict, bytes: 305088 } }, '判定说 run.json 写了 305088 字节'],
  ];
  for (const [name, patch, keyword] of hardCases) {
    const input = { serverVerdict: goodVerdict, pageState: goodPage, framesOnDisk: 80, hostGpu: goodHostGpu,
      onDiskBytes: goodOnDiskBytes, ...patch };
    const result = auditRun(input);
    const reasons = result.hard.join(' | ');
    check(`硬条件：${name}`, result.ok === false && reasons.includes(keyword));
  }
  // 反向再钉一次：判定不是恒绿，也不是恒红。
  check('判定不是恒绿：存在判红的输入',
    auditRun({ serverVerdict: null, pageState: null, framesOnDisk: 0 }).ok === false);
  check('判定不是恒红：与 native 不同**不算**硬条件不过',
    auditRun({
      serverVerdict: { ...goodVerdict, digest_matches_expect: false, frames_digest: 'ffffffffffffffff' },
      pageState: goodPage,
      framesOnDisk: 80,
      hostGpu: goodHostGpu,
      onDiskBytes: goodOnDiskBytes,
    }).ok === true);
  check('与 native 不同要落进发现里（这是 M2 要找的事实）',
    auditRun({
      serverVerdict: { ...goodVerdict, digest_matches_expect: false, frames_digest: 'ffffffffffffffff' },
      pageState: goodPage,
      framesOnDisk: 80,
      hostGpu: goodHostGpu,
      onDiskBytes: goodOnDiskBytes,
    }).findings.length >= 1);

  // ---- 卡身份那一环：每一条红线各自在守什么 -------------------------------
  //
  // 这一节的存在理由：卡身份是**三份物证跨三个进程**（浏览器给的页面、CDP 给的表、
  // 服务端落盘的文件）对出来的，任何一份单独看都自洽。只断言 `ok === false` 的话，
  // 一个"缺了也就是红一下"的判定也能过，而真正要防的是**红在别处**——比如漏了表却
  // 因为帧数对得上而放行。
  const hostGpuCases = [
    ['这一环整个没给（不许沉默跳过）', null, null, '不许沉默跳过'],
    ['驱动没从 CDP 读到表', { ...goodHostGpu, posted: null }, null, '没能从 CDP 读到宿主设备表'],
    ['表没送到服务端', { ...goodHostGpu, post: null }, null, '没有送到服务端'],
    ['表被服务端拒收（把服务端给的理由带出来）',
      { ...goodHostGpu, post: { status: 400, body: { error: '宿主设备表的 source 应为 "CDP SystemInfo.getInfo"' } } },
      null, 'source 应为'],
    ['盘上没有 host-gpu.json（adapter.json 那句话成了空话）',
      { ...goodHostGpu, onDisk: null }, null, `盘上没有 ${HOST_GPU_FILE_ON_RECORD}`],
    ['盘上那份表的来源与驱动读到的不一致',
      { ...goodHostGpu, onDisk: { ...goodHostGpu.onDisk, source: 'CUDA' } }, null, '的来源是'],
    ['盘上那份表少了驱动读到的卡',
      { ...goodHostGpu, onDisk: { ...goodHostGpu.onDisk, devices: hostDevices.slice(0, 1) } },
      null, '里少了驱动读到的卡'],
    ['服务端说用的卡不在驱动读到的表里',
      { ...goodHostGpu, onDisk: { ...goodHostGpu.onDisk,
        resolved: { ...goodHostGpu.onDisk.resolved, device_id: 9999, device: 'NVIDIA GeForce RTX 5090' } } },
      null, '表里没有这块卡'],
    ['判定里的卡与盘上那份文件不是同一块',
      goodHostGpu,
      { host_gpu: { ...goodVerdict.host_gpu, device_id: 9999 } },
      '同一个问题两个答案'],
    ['判定全过却说不出卡身份（判定没说，就不算说过）',
      goodHostGpu, { host_gpu: null }, '判定里没有 host_gpu 这一栏'],
  ];
  for (const [name, hostGpu, verdictPatch, keyword] of hostGpuCases) {
    const result = auditRun({
      serverVerdict: { ...goodVerdict, ...(verdictPatch ?? {}) },
      pageState: goodPage,
      framesOnDisk: 80,
      hostGpu,
      // 这一节只测卡身份那一条：字节数那一环给全绿灯的值，免得"红在别处"混进来。
      onDiskBytes: goodOnDiskBytes,
    });
    const reasons = result.hard.join(' | ');
    check(`卡身份：${name}（且理由是"${keyword}"）`, result.ok === false && reasons.includes(keyword));
  }

  // ---- 宿主设备表：整理到哪一步为止 ---------------------------------------
  const payload = hostGpuPayload({ devices: hostDevices });
  check('设备表：原样搬过来（"哪块卡"要写进记录的那几栏一栏不丢）',
    payload.devices?.length === 3 && payload.devices[0].deviceString === 'NVIDIA GeForce RTX 4070'
    && payload.devices[0].driverVersion === '32.0.16.1074'
    && Object.hasOwn(payload.devices[0], 'vendorString'));
  check('设备表：来源写着（换来源等于换判据）', payload.source === HOST_GPU_SOURCE);
  check('设备表：空的 gpu 段不编一个表出来',
    hostGpuPayload(undefined).error !== undefined && hostGpuPayload({ devices: [] }).error !== undefined);
  check('设备表：devices 不是数组也不编一个表出来',
    hostGpuPayload({ devices: 'GPU 0' }).error !== undefined);
  check('设备表：拿不到表时**不**返回一个空表（空表会被服务端当成"表里没这块卡"）',
    hostGpuPayload(undefined).devices === undefined);

  // ---- 摘掉 ?autorun：这一轮由驱动决定什么时候开跑 ------------------------
  const suppressed = suppressAutorun('http://127.0.0.1:1/corpus.html?leg=m2&frames=0..16&autorun=1&expect=71ec');
  check('摘 autorun：摘掉了，且别的参数一个没动',
    suppressed.removed === true && !suppressed.url.includes('autorun')
    && suppressed.url.includes('leg=m2') && suppressed.url.includes('frames=0..16')
    && suppressed.url.includes('expect=71ec'));
  check('摘 autorun：本来就没有时原样返回（不是错误）',
    suppressAutorun('http://x/corpus.html?leg=m2').removed === false
    && suppressAutorun('http://x/corpus.html?leg=m2').url === 'http://x/corpus.html?leg=m2');
  check('摘 autorun：URL 不合法时说清楚，而不是抛出去',
    suppressAutorun('not a url').error !== undefined);

  // ---- 浏览器参数：URL 必须还在最后 --------------------------------------
  const args = buildBrowserArgs({
    profileDir: 'P', url: 'http://x/corpus.html?autorun=1', width: 1600, height: 1200, headless: true, devtoolsPort: 9222,
  });
  const withExtra = [...args.slice(0, -1), '--force_low_power_gpu', args.at(-1)];
  check('追加的浏览器参数插在 URL 之前', withExtra.at(-2) === '--force_low_power_gpu' && withExtra.at(-1).startsWith('http'));
  check('追加参数后仍然只有一个 target', withExtra.filter((a) => a === 'about:blank' || a.startsWith('http')).length === 1);

  // ---- 旁证里那一栏卡名 ---------------------------------------------------
  // 真记录上踩过：Chrome 报 `adapter.name === ''`，`??` 只认 null/undefined，于是空串被
  // 原样写进旁证，成了"这块卡的名字是空"。这里把三条路都钉住。
  check('卡名：页面报空串时写 null（空串不是名字，三份记录同一种写法）',
    legAdapterName({ adapter: { name: '' } }, { adapter_name: null }) === null
    && legAdapterName({ adapter: { name: '' } }, { adapter_name: '' }) === null
    && legAdapterName({}, null) === null
    && legAdapterName({ adapter: {} }, undefined) === null);
  check('卡名：页面报得出真名字时就用它（不回退成 null）',
    legAdapterName({ adapter: { name: 'AMD Radeon(TM) Graphics' } }, { adapter_name: null }) === 'AMD Radeon(TM) Graphics');
  check('卡名：页面答不出、服务端结算出了名字时用服务端那份（不丢已成的事实）',
    legAdapterName({ adapter: { name: '' } }, { adapter_name: 'NVIDIA GeForce RTX 4070' }) === 'NVIDIA GeForce RTX 4070');

  const failed = cases.filter((item) => !item.ok);
  for (const item of cases) console.log(`  ${item.ok ? '✓' : '✗'} ${item.name}`);
  console.log(`\n${cases.length - failed.length}/${cases.length} 项通过`);
  return failed.length === 0 ? 0 : 1;
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const out = {
    out: 'records/m2/browser',
    leg: 'm2',
    native: 'records/m1/dx12',
    expect: null,
    chromeArgs: [],
    timeout: 300,
    width: 1600,
    height: 1200,
    headed: false,
    keepProfile: false,
    selfTest: false,
    help: false,
  };
  const args = [...argv];
  while (args.length > 0) {
    const arg = args.shift();
    switch (arg) {
      case '--out': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') {
          return { error: '--out 后面要跟一个目录' };
        }
        out.out = value.trim().replace(/\\/g, '/');
        break;
      }
      case '--leg': {
        const value = args.shift();
        if (value === undefined || !/^[a-z0-9-]{1,64}$/.test(value) || value.startsWith('-') || value.endsWith('-')) {
          return { error: `--leg 只认 [a-z0-9-] 且长度 1..=64（它是目录名与 adapter.json 的 backend_slug），收到 ${JSON.stringify(value)}` };
        }
        out.leg = value;
        break;
      }
      case '--native': {
        const value = args.shift();
        if (value === undefined || value.trim() === '') return { error: '--native 后面要跟一个目录，或 none' };
        out.native = value.trim() === 'none' ? null : value.trim().replace(/\\/g, '/');
        break;
      }
      case '--expect': {
        const value = args.shift();
        if (value === undefined || !/^[0-9a-f]{16}$/.test(value)) {
          return { error: `--expect 要 16 位十六进制摘要，收到 ${JSON.stringify(value)}` };
        }
        out.expect = value;
        break;
      }
      case '--chrome-arg': {
        const value = args.shift();
        if (value === undefined || !value.startsWith('-')) {
          return { error: `--chrome-arg 后面要跟一条以 - 开头的浏览器参数，收到 ${JSON.stringify(value)}` };
        }
        out.chromeArgs.push(value);
        break;
      }
      case '--timeout': {
        const value = args.shift();
        if (value === undefined || !/^\d+$/.test(value) || Number(value) < 30) {
          return { error: `--timeout 要 ≥ 30 的整数（秒），收到 ${JSON.stringify(value)}` };
        }
        out.timeout = Number(value);
        break;
      }
      case '--width':
      case '--height': {
        const value = args.shift();
        if (value === undefined || !/^\d+$/.test(value) || Number(value) < 320) {
          return { error: `${arg} 后面要跟一个不小于 320 的整数` };
        }
        out[arg === '--width' ? 'width' : 'height'] = Number(value);
        break;
      }
      case '--headed':
        out.headed = true;
        break;
      case '--keep-profile':
        out.keepProfile = true;
        break;
      case '--self-test':
        out.selfTest = true;
        break;
      case '-h':
      case '--help':
        out.help = true;
        break;
      default:
        // 静默忽略参数是"看起来全绿"最常见的来源。
        return { error: `不认识的参数：${arg}\n\n${USAGE}` };
    }
  }
  if (out.expect && out.native) {
    return { error: '--expect 与 --native 二选一：两个都给的话，"期望值到底是谁的"就没有唯一答案' };
  }
  return out;
}

/** 产物目录里 frames/*.png 的张数。**驱动自己数**，不转述服务端的说法。 */
function countFramesOnDisk(outDir) {
  const framesDir = join(outDir, 'frames');
  if (!existsSync(framesDir)) return 0;
  return readdirSync(framesDir).filter((name) => name.endsWith('.png')).length;
}

/**
 * 连一条 DevTools WebSocket。浏览器级与页面级都走这里。
 *
 * 单独抽出来是因为这里的错误信息要**说清连的是哪一条**：浏览器级连不上（`/json/version`
 * 那条）与页面级连不上（`/json/list` 里那个 target）长得一样，但成因完全不同——前者说明
 * 调试端口被别的进程占了，后者说明页面 target 还没建出来。混成一句"连 DevTools 失败"
 * 会把人引向错方向。
 */
async function connectDevtools(url, what) {
  const socket = new WebSocket(url);
  await withTimeout(
    new Promise((resolvePromise, rejectPromise) => {
      socket.addEventListener('open', () => resolvePromise(undefined), { once: true });
      socket.addEventListener('error', () => rejectPromise(new Error(`连 ${what} DevTools WebSocket 失败`)), { once: true });
    }),
    15000,
    `连 ${what} DevTools`,
  );
  return socket;
}

/** POST 一个 JSON，并**不管成败都取回响应体**：拒收的理由就在里面，正是要收进记录的东西。 */
async function postJson(url, payload, what) {
  const response = await fetch(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(payload),
  });
  let body = null;
  try {
    body = await response.json();
  } catch {
    body = { ok: false, error: `服务端返回的不是 JSON（HTTP ${response.status}）` };
  }
  return { status: response.status, body, what };
}

async function run(args) {
  const browser = findBrowser();
  if (!browser) {
    console.error('✗ 找不到 Chrome / Edge。用 DHAMPIR_CHROME=<exe> 指定一个。');
    return 2;
  }
  const outDir = resolve(REPO_ROOT, args.out);
  if (!existsSync(join(WWW_ROOT, 'corpus.html'))) {
    console.error(`✗ ${join(WWW_ROOT, 'corpus.html')} 不存在`);
    return 2;
  }

  // DevTools 端口：**不能**用服务端那个。服务端用 `--port 0` 让系统挑，这里挑之前
  // 先把服务端叫起来拿到它，再挑一个不同的（本仓踩过：两个 freePort() 撞号）。
  const serverArgs = ['scripts/serve-corpus-harness.mjs', '--port', '0', '--out', args.out, '--leg', args.leg];
  if (args.native) serverArgs.push('--native', args.native);
  if (args.expect) serverArgs.push('--expect', args.expect);

  const server = spawn(process.execPath, serverArgs, {
    cwd: REPO_ROOT,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let serverOut = '';
  let serverExited = null;
  server.stdout.on('data', (chunk) => {
    serverOut += chunk.toString('utf8');
    process.stdout.write(chunk);
  });
  server.stderr.on('data', (chunk) => {
    serverOut += chunk.toString('utf8');
    process.stderr.write(chunk);
  });
  server.on('exit', (code) => {
    serverExited = code ?? 0;
  });

  const profileDir = mkdtempSync(join(REPO_ROOT, 'target', 'corpus-profile-'));
  let child = null;
  let ws = null;
  let browserWs = null;
  let chromeErr = '';
  try {
    // 等横幅。等不到时**必须区分两种情况**：服务端自己退了（参数/环境问题，它的解释
    // 就在输出里），还是只是慢。混在一起报"没等到 PORT"会把人引向错方向。
    const bannerDeadline = Date.now() + 20000;
    let banner = null;
    while (banner === null && Date.now() < bannerDeadline) {
      banner = parseServerBanner(serverOut);
      if (banner === null && serverExited !== null) break;
      await sleep(100);
    }
    if (banner === null) {
      console.error(serverExited !== null
        ? `✗ 本地服务自己退出了（退出码 ${serverExited}）。它上面的输出就是原因。`
        : '✗ 本地服务 20 s 内没有打印 PORT 横幅。');
      return 2;
    }

    let devtoolsPort = await freePort();
    while (devtoolsPort === banner.port) devtoolsPort = await freePort();

    console.log(`宿主  ${browser.kind}  ${browser.path}${args.headed ? '（有头）' : '（无头）'}`);
    if (args.chromeArgs.length > 0) console.log(`追加的浏览器参数  ${args.chromeArgs.join(' ')}`);

    // URL 里的 `?autorun` 摘掉：这一轮什么时候开跑由**驱动**决定（宿主设备表得先到），
    // 不是页面自己一加载就冲出去。没有它页面会停在"等待运行"，正是这里要的状态。
    const page = suppressAutorun(banner.url);
    if (page.error) {
      console.error(`✗ ${page.error}`);
      return 2;
    }
    console.log(`页面  ${page.url}${page.removed ? '（已摘掉 ?autorun：这一轮由本脚本触发）' : ''}`);

    // 追加的参数必须插在 URL **之前**：URL 是唯一带 `&` 的一项，放最后最不容易被吃掉。
    const browserArgs = buildBrowserArgs({
      profileDir,
      url: page.url,
      width: args.width,
      height: args.height,
      headless: !args.headed,
      devtoolsPort,
    });
    const finalArgs = [
      ...browserArgs.slice(0, -1),
      ...args.chromeArgs,
      browserArgs.at(-1),
    ];
    child = spawn(browser.path, finalArgs, { stdio: ['ignore', 'ignore', 'pipe'] });
    child.stderr.on('data', (chunk) => {
      chromeErr += chunk.toString('utf8');
    });

    await withTimeout(waitForOccupiedPort(devtoolsPort, 30000), 32000, `浏览器 的 DevTools 端口 ${devtoolsPort}`);

    // ---- 宿主的设备表：**先送到服务端，再让页面开跑** -----------------------
    //
    // 为什么非先不可：服务端解卡身份要用页面报的 `in_page.vendor` 当匹配起点，而页面一被
    // 触发就会往外送 adapter.json。让这两件事去抢时间，"偶尔红"会变成常态。
    //
    // `SystemInfo` 是 **browser 域**：在页面级那条 WS 上发它会报 method not found，
    // 所以这里单开一条 browser 级连接（`/json/version` 的 `webSocketDebuggerUrl`），读完就关。
    const browserVersion = await fetch(`http://127.0.0.1:${devtoolsPort}/json/version`).then((r) => r.json());
    browserWs = await connectDevtools(browserVersion.webSocketDebuggerUrl, '浏览器级');
    const systemInfo = await new Cdp(browserWs).send('SystemInfo.getInfo');
    browserWs.close();
    browserWs = null;

    const host = hostGpuPayload(systemInfo.gpu);
    if (host.error) {
      console.error(`✗ ${host.error}`);
      console.error('  这台机器/这个浏览器上取不到宿主设备表，"用了哪块卡"就答不出来——'
        + '不是换个参数能绕过去的事（见文件头第 5 条）。');
      return 2;
    }
    const table = host.devices.map((device) => `${device.deviceString}`
      + `（vendor ${device.vendorId} / device ${device.deviceId}）`).join('、');
    console.log(`宿主设备表  ${host.devices.length} 块卡：${table}`);
    const hostRes = await postJson(`http://127.0.0.1:${banner.port}/__corpus/host`, host, '宿主设备表');
    if (hostRes.status !== 200 || hostRes.body?.ok !== true) {
      console.error(`✗ 宿主设备表被服务端拒收（HTTP ${hostRes.status}）：${hostRes.body?.error ?? '（没给原因）'}`);
      return 1;
    }
    // 此刻服务端**应当**还没下结论：它要等页面报的 vendor（那是匹配的起点）。这一口要是
    // 现在就说"解出了哪块卡"，那它必是拿别的什么东西猜的——顺序错了要在这里就看得见。
    console.log(hostRes.body.written === true
      ? `  ✓ 服务端收下并当场解出了卡身份：${hostRes.body.host_gpu?.device}`
      : '  ✓ 服务端收下（此刻还看不到页面报的 vendor，等 adapter.json 到了再对账落盘）');

    const target = await fetchPageTarget(devtoolsPort, `http://127.0.0.1:${banner.port}/`);
    ws = await connectDevtools(target.webSocketDebuggerUrl, '页面级');
    const cdp = new Cdp(ws);
    await cdp.send('Page.enable');
    await cdp.send('Runtime.enable');
    const version = await cdp.send('Browser.getVersion').catch(() => ({}));
    await cdp.send('Emulation.setDeviceMetricsOverride', {
      width: args.width,
      height: args.height,
      deviceScaleFactor: 1,
      mobile: false,
    });

    // 等页面脚本求值完（pkg/ 都在、`dhampirCorpusHarness` 挂上来了）。
    const harnessDeadline = Date.now() + 60000;
    for (;;) {
      const ready = await cdp.evaluate('typeof globalThis.dhampirCorpusHarness?.run === "function"');
      if (ready === true) break;
      if (Date.now() > harnessDeadline) {
        const status = await cdp.evaluate('document.getElementById("status")?.textContent ?? ""');
        throw new Error(`取证页没把 dhampirCorpusHarness 挂上来。状态栏：${status}`);
      }
      await sleep(250);
    }
    // 这一轮由**本脚本**触发（URL 里的 autorun 已摘掉，宿主设备表也已送到）。触发前先确认
    // 页面还是闲着的：`done` 不是 null 说明已经有人在跑这一轮了——那多半是 autorun 没摘干净，
    // 硬等下去只会得到"两轮结果混进同一批文件名"这种最难查的错。
    const triggered = await cdp.evaluate(`(() => {
      const harness = globalThis.dhampirCorpusHarness;
      if (harness.done !== null) return false;
      const pending = harness.run();
      harness.done = pending;
      pending.then((state) => { harness.state = state; });
      return true;
    })()`);
    if (triggered !== true) {
      throw new Error('页面已经在自己跑这一轮了（harness.done 不是 null）——'
        + 'URL 里的 ?autorun 没有被摘掉，而这一轮必须由驱动在宿主设备表送到之后触发');
    }

    console.log(`等这一轮跑完（上限 ${args.timeout} s）…`);
    const pageState = await withTimeout(
      cdp.evaluate('globalThis.dhampirCorpusHarness.done', { awaitPromise: true }),
      args.timeout * 1000,
      '页面跑完这一轮',
    );

    // 服务端的判定行在页面 POST /__corpus/run 时就打出来了，但管道送达有延迟；
    // 页面的 `done` 只保证 POST 的响应收到了，不保证 stdout 已经进了这个变量。
    const verdictDeadline = Date.now() + 10000;
    let serverVerdict = parseVerdict(serverOut);
    while (serverVerdict === null && Date.now() < verdictDeadline && serverExited === null) {
      await sleep(200);
      serverVerdict = parseVerdict(serverOut);
    }

    const framesOnDisk = countFramesOnDisk(outDir);
    // 盘上那份卡身份文件：**驱动自己去读**（不转述服务端的回话）。服务端说解出了哪块卡，
    // 与盘上真写着哪块卡，是两件事——对不上时这份记录就答不出"用了哪块卡"，哪怕别的都全绿。
    const hostGpuPath = join(outDir, HOST_GPU_FILE_ON_RECORD);
    let hostGpuOnDisk = null;
    try {
      hostGpuOnDisk = JSON.parse(readFileSync(hostGpuPath, 'utf8'));
    } catch {
      hostGpuOnDisk = null;
    }
    // 三个文本产物在盘上的字节数：**驱动自己去 `stat`**，不转述谁的说法。
    const sizeOnDisk = (name) => {
      const file = join(outDir, name);
      return existsSync(file) ? statSync(file).size : null;
    };
    const onDiskBytes = {
      adapter: sizeOnDisk('adapter.json'),
      readings: sizeOnDisk('readings.txt'),
      runJson: sizeOnDisk('run.json'),
    };
    const audit = auditRun({
      serverVerdict,
      pageState,
      framesOnDisk,
      onDiskBytes,
      hostGpu: { posted: host, post: hostRes, onDisk: hostGpuOnDisk },
    });

    const digest = serverVerdict?.frames_digest ?? pageState?.frames_digest ?? null;
    const expected = serverVerdict?.expected_frames_digest ?? pageState?.expected_frames_digest ?? null;
    console.log('');
    console.log(`页面  帧 ${pageState?.frames_written ?? '?'} 帧已落盘；整轮 ${pageState?.elapsed_ms ?? '?'} ms；`
      + `渲染 ${pageState?.render_ms ?? '?'} ms`);
    console.log(`盘上  ${framesOnDisk} 张 PNG`);
    console.log(`摘要  ${digest ?? '(未知)'}${expected ? `（native ${expected}）` : ''}`);
    console.log(`账本  ${JSON.stringify(serverVerdict?.counts ?? pageState?.counts ?? null)}`);
    console.log(`卡身份  ${hostGpuOnDisk?.resolved?.device ?? '(盘上没有 host-gpu.json)'}`
      + `（device ${hostGpuOnDisk?.resolved?.device_id ?? '?'}，依据 ${hostGpuOnDisk?.resolved?.match_reason ?? '?'}）`);

    if (!audit.ok) {
      console.error('\n✗ 硬条件没过，**不写截图**（宁可没有图，也不要一张看着全绿的图）：');
      for (const reason of audit.hard) console.error(`  - ${reason}`);
      console.error('  页面结论：');
      console.error(`  ${JSON.stringify(pageState, null, 2).split('\n').join('\n  ')}`);
      if (chromeErr.trim()) console.error(`  浏览器 stderr：\n${chromeErr.trim()}`);
      return 1;
    }

    const metrics = await cdp.send('Page.getLayoutMetrics');
    const content = metrics.cssContentSize ?? metrics.contentSize;
    const shot = await cdp.send('Page.captureScreenshot', {
      format: 'png',
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width: Math.ceil(content.width), height: Math.ceil(content.height), scale: 1 },
    });
    const png = Buffer.from(shot.data, 'base64');
    if (!png.subarray(0, 8).equals(Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))) {
      throw new Error('CDP 返回的截图不是 PNG（签名不对）');
    }
    const shotName = 'screenshot-browser-corpus.png';
    writeFileSync(join(outDir, shotName), png);

    const adapter = pageState?.adapter ?? {};
    const sidecar = {
      schema: 1,
      milestone: 'M2',
      purpose: 'M2 浏览器腿（wasm + 浏览器 WebGPU）那一轮 corpus 取证的整页截图，以及这次运行的硬结论——'
        + '用于人工复核截图里的绿是不是真的。逐帧 SSIM 判定在 T2.4（framediff）另有一份。',
      captured_at: new Date().toISOString(),
      page_url: banner.url,
      leg: {
        slug: args.leg,
        out_dir: args.out,
        requested_backends: adapter.requested_backends ?? null,
        backend_slug: adapter.backend_slug ?? null,
        adapter_name: legAdapterName(adapter, serverVerdict),
        build_profile: adapter.build_profile ?? null,
        wgpu_version: adapter.wgpu_version ?? null,
        naga_version: adapter.naga_version ?? null,
      },
      browser: {
        kind: browser.kind,
        path: browser.path,
        product: version.product ?? null,
        revision: version.revision ?? null,
        user_agent: version.userAgent ?? null,
        headless: !args.headed,
        extra_args: args.chromeArgs,
      },
      viewport: {
        width: args.width,
        height: args.height,
        captured_content: { width: content.width, height: content.height },
      },
      round: {
        scene: pageState?.scene ?? null,
        frames_arg: pageState?.frames_arg ?? null,
        frames_written: pageState?.frames_written ?? null,
        frames_verified: serverVerdict?.frames_verified ?? null,
        frames_on_disk: framesOnDisk,
        png_bytes_total: serverVerdict?.png_bytes_total ?? pageState?.png_bytes_total ?? null,
        frames_digest: digest,
        expected_frames_digest: expected,
        digest_matches_expect: serverVerdict?.digest_matches_expect ?? pageState?.digest_matches_native ?? null,
        counts: serverVerdict?.counts ?? pageState?.counts ?? null,
        // 文本产物的字节数：`*_bytes` 是服务端实测（写了多少），`*_bytes_on_disk` 是驱动
        // `stat` 出来的。两栏并排放着，是因为它们得相等——不相等时这一轮根本不会写这张图。
        readings_bytes: pageState?.readings_bytes ?? null,
        run_json_bytes: pageState?.run_json_bytes ?? null,
        adapter_bytes: pageState?.adapter_bytes ?? null,
        readings_bytes_on_disk: onDiskBytes.readings,
        run_json_bytes_on_disk: onDiskBytes.runJson,
        adapter_bytes_on_disk: onDiskBytes.adapter,
        elapsed_ms: pageState?.elapsed_ms ?? null,
      },
      server_checks: serverVerdict?.checks ?? [],
      server_findings: serverVerdict?.findings ?? [],
      // 卡身份这一环：三份物证都留在旁证里（驱动读到的表、服务端的回话、盘上那份文件）。
      // 半年后有人问"这块卡是怎么认出来的"，这一节能把整条链答完，不必去翻脚本。
      host_gpu: {
        source: host.source,
        read_from: `CDP SystemInfo.getInfo（浏览器级 WS：${browserVersion.Browser ?? '?'}）`,
        devices: host.devices,
        post: { status: hostRes.status, body: hostRes.body ?? null },
        on_disk: hostGpuOnDisk,
      },
      trigger: {
        autorun_suppressed: page.removed,
        by: 'scripts/run-browser-corpus.mjs（摘下 ?autorun 后自己调 dhampirCorpusHarness.run）',
        why: '宿主设备表必须先于页面的 adapter.json 到达服务端：解卡身份要用页面报的 in_page.vendor 当匹配起点',
      },
      screenshot: {
        file: shotName,
        bytes: png.length,
        sha256: createHash('sha256').update(png).digest('hex'),
      },
      audit: { ok: audit.ok, hard: audit.hard, findings: audit.findings },
      notes: [
        '截图是 CDP Page.captureScreenshot 的整页结果，不是人按的 PrintScreen——同一条命令能再拍一张。',
        '服务端在写 run.json **之前**逐帧复算了路径/字节数/FNV-1a 摘要；本脚本另外自己数了一遍盘上的 PNG。',
        '`digest_matches_expect` 只是"逐字节相同吗"这一栏的答案；不同**不是**错误——T2.4 的逐帧 SSIM 才是定量判定。',
        '判定在服务端与页面各有一份，两边都得同意才写这张图。',
        '`host_gpu.devices` 是驱动从 CDP 原样搬来的宿主设备表；`on_disk` 是服务端把页面报的 vendor '
          + '对到其中**唯一**一块卡之后写出的 host-gpu.json——"用了哪块卡"这句话的出处就是这里。',
        '`round.*_bytes` 是服务端实测"这份文件写了多少字节"（页面的那一栏抄的就是它）；'
          + '`round.*_bytes_on_disk` 是驱动 `stat` 盘上文件得到的。两栏必须相等，否则不写这张图——'
          + '页面的 `text.length` 是 UTF-16 码元数，拿它当字节会在记录里留下一个查不出来的假数。',
      ],
    };
    writeFileSync(join(outDir, 'screenshot-browser-corpus.json'), `${JSON.stringify(sidecar, null, 2)}\n`, 'utf8');

    console.log(`\n✓ 完整性三方一致（页面 ${pageState?.frames_written} / 服务端 ${serverVerdict?.frames_verified} / 盘上 ${framesOnDisk}）`);
    for (const finding of audit.findings) {
      console.log(`  ${finding.ok === null ? '·' : finding.ok ? '✓' : '!'} ${finding.detail}`);
    }
    console.log(`\n  截图 ${png.length} 字节 → ${args.out}/${shotName}`);
    console.log(`  旁证 → ${args.out}/screenshot-browser-corpus.json`);
    console.log(`  这一轮的产物 → ${args.out}/（adapter.json、run.json、readings.txt、frames/*.png）`);
    return 0;
  } catch (error) {
    if (chromeErr.trim()) console.error(`--- 浏览器 stderr ---\n${chromeErr.trim().slice(-4000)}`);
    if (serverOut.trim()) console.error(`--- 本地服务输出 ---\n${serverOut.trim().slice(-4000)}`);
    throw error;
  } finally {
    try {
      ws?.close();
      browserWs?.close();
    } catch {
      /* 关不掉就算了，下面照样杀进程 */
    }
    if (child && child.pid) {
      // Chrome 会开出子进程；只杀父进程会留下一个还在跑 GPU 的孙子。
      spawn('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
    }
    server.kill();
    await sleep(400);
    if (!args.keepProfile) {
      try {
        rmSync(profileDir, { recursive: true, force: true, maxRetries: 5 });
      } catch {
        // profile 目录删不掉不该让整条记录失败——它在 target/ 下，本来就没人管。
      }
    } else {
      console.log(`  保留 profile：${profileDir}`);
    }
  }
}

if (import.meta.url === `file://${process.argv[1].replace(/\\/g, '/')}` || process.argv[1].endsWith('run-browser-corpus.mjs')) {
  const args = parseArgs(process.argv.slice(2));
  if (args.error) {
    console.error(`✗ ${args.error}`);
    process.exitCode = 2;
  } else if (args.help) {
    console.log(USAGE);
  } else if (args.selfTest) {
    process.exitCode = selfTest();
  } else {
    // 只设 exitCode，不调 process.exit()：理由同别的守卫脚本（Windows + Node 的 libuv 断言）。
    run(args).then(
      (code) => {
        process.exitCode = code;
      },
      (error) => {
        console.error(`✗ 取证失败：${error.message}`);
        process.exitCode = 1;
      },
    );
  }
}
