// 起外部工具（dhampir CLI / ffmpeg / ffprobe / node 脚本）的统一入口。
//
// # 为什么必须显式给 stdin
//
// Node 的 `spawnSync(cmd, args)` / `spawn(cmd, args)` 默认是 `stdio: ['pipe', 'pipe', 'pipe']`，
// 也就是**给子进程开一条 stdin 管道**。对本仓这些工具来说那条管道**从来没人写过** ——
// 但它们照样要创建，而创建本身在有些环境里就是做不到的。
//
// 本机 agent 会话里实测：**只有 stdin 管道创建不了**。
//
//   node 侧：spawnSync 返回 status=null，error.code='EBUSY'
//   Rust 侧：os error 231（ERROR_PIPE_BUSY，"所有的管道范例都在使用中"）
//
// 最小复现（纯标准库、无依赖、与本仓代码无关）：
//
//   // 子进程拿 cmd /C exit 0 就行
//   stdin=null   -> OK
//   stdin=piped  -> ERR os error 231
//   stdout=pipe  -> OK          // 注意：**输出管道是好的**
//
// 所以规矩是：**不喂 stdin 的调用一律把 stdin 设成 'ignore'**。
//
// # 这不是"垫片"
//
// 垫片（比如给子进程 require 一个改 stdio 的 shim）是把环境问题糊过去，
// 副作用是"**真的起不了子进程**"也会跟着变绿 —— 那是更坏的错。
// 这里相反：它如实声明"这次调用不喂 stdin"。真起不来的时候 status 照样是 null、照样红。
//
// # 反过来：要喂 stdin 的地方不许用这个助手
//
// 带 `input:` 的调用（真往子进程 stdin 里写东西的）**必须**保留管道，
// 用这个助手会把 stdin 关掉，喂进去的内容会无声无息地丢掉 —— 比报错更难查。
// 本仓这类地方只有一处：渲染循环往编码器 stdin 写帧（crates/dhampir-worker/src/pipeline.rs）。

import { spawnSync } from 'node:child_process';

/**
 * 不喂 stdin 的三件套。
 *
 * **只有 stdin 是 ignore**：stdout / stderr 照收。
 * 顺手全 ignore 的话守卫就成了睁眼瞎 —— 失败时连现场都带不回来。
 */
export const NO_STDIN = Object.freeze(['ignore', 'pipe', 'pipe']);

/**
 * spawnSync 的统一包装：默认不喂 stdin，其余照传。
 *
 * 返回的仍是 spawnSync 的原样结果（status / stdout / stderr / error），
 * 这样调用方不必改判读方式。
 */
export function runToolSync(command, args, options = {}) {
  return spawnSync(command, args, { stdio: NO_STDIN, encoding: 'utf8', ...options });
}
