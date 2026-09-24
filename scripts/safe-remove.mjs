// 删临时目录 / 临时文件：**删不掉不算失败**。
//
// # 为什么需要它
//
// 守卫里有大量"先把临时目录清掉再建"与"跑完收拾干净"的删除。这些删除**与判据无关**：
// 一条守卫的结论不该取决于清理动作成不成功。可是清理真的会失败 ——
//
// 本机 agent 会话实测：WorkBuddy CLI 给 `fs.rmSync` 套了一层安全删除
// （`node-safe-delete-shim.cjs`），它是**按 turn 累计计数**的，超过阈值直接抛：
//
//     Error: [safe-delete][SAFE_DELETE_BULK_CONFIRM_REQUIRED]
//            {"count":549,"threshold":50,"scope":"turn","targets":[...]}
//         at wrappedRmSync (node-safe-delete-shim.cjs)
//         at collect (check-cli.mjs:154)
//
// 而跑一遍守卫套件本身就要删掉几百个临时路径 —— 于是**越靠后的守卫越容易炸**。
// 报告上它长得像"这条契约不成立"，实际上是被"清理"绊倒的，能白白耗掉半天排查。
// （这条是踩出来的：`check-cli` 一度从 `27/30` 变成直接崩在 `collect` 的第一行。）
//
// # 规矩
//
//   * 删除失败 → **降级成一行警告**，不动退出码；
//   * 但**不静默** —— 留不下痕迹的失败，下一个人只会看到"判据莫名其妙少了几条"。
//
// # 一条必须记住的边界
//
// 清不掉的时候，**上一轮的残留文件可能还在**。所以**不要拿"文件在不在"当判据**：
// 要判就判内容 / 摘要，或者只认**这一轮自己刚写出来的字节**。
// `scripts/check-local-backend.mjs` 的下载产物就是这么处理的（按 `wrote` 判，不按 `existsSync`）。

import { rmSync } from 'node:fs';

/**
 * 尽力删。删不掉返回 false（每个失败的路径打一行警告），**不影响退出码**。
 *
 * @param {string|string[]} target 一个路径，或者一组路径。
 * @returns {boolean} 是否全部删干净。
 */
export function tryRemove(target) {
  const list = Array.isArray(target) ? target : [target];
  let allGone = true;
  for (const path of list) {
    try {
      rmSync(path, { recursive: true, force: true });
    } catch (error) {
      allGone = false;
      const reason = error !== null && error !== undefined && error.code ? error.code : String(error);
      console.warn('清理失败（与判据无关，但上一轮的残留可能还在）：' + path + '：' + reason);
    }
  }
  return allGone;
}
