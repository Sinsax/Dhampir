// 请求路径 -> web/ 下的文件路径。**单独一个模块，是为了让守卫能真正测它。**
//
// 为什么不写在 web-check.mjs 里：那个文件**顶层就 server.listen()**，
// 谁 import 它谁就顺手起一个服务 —— 于是"想测这个函数"就变成了"想跑一次验收"。
// 本仓库对这类拆分的说法是「守卫与驱动必须对同一件事给出同一个答案」（见 stale-pkg.mjs），
// 所以判据只留一份实现，放这里。
//
// # 为什么值得有守卫
//
// 这一条的失败方式都不自己冒出来：
//   * 判宽了  -> 读到仓库外面（`/../../.git/config`），而且**页面看起来正常**；
//   * 判窄了  -> 静态文件变 404，表现是"页面白屏"或"样式没生效"，
//                与真正的原因（路由没转 / 没加进名单）毫无关系。
// 两种都不会抛错，所以只能靠反向用例钉住。

import { isAbsolute, relative, resolve } from 'node:path';

/**
 * 把请求路径解析成 `webDir` 下的一个文件路径；越界或形状可疑返回 null。
 *
 * 判据是**解析之后仍在 webDir 里**，不是"路径里有没有 .." —— 后者漏掉
 * 多重斜杠、反斜杠、以及被 URL 解码回来的 %2e%2e（到这里已经是 ..）。
 * 用 relative() 而不是 startsWith(webDir)：`web-evil/` 前缀相同但不是子目录。
 *
 * @param {string} webDir 允许的根目录（绝对或相对都行，内部会 resolve）
 * @param {string} path   以 / 开头的请求路径（已 URL 解码）
 * @returns {string|null}
 */
export function insideWebDir(webDir, path) {
  if (typeof path !== 'string' || path === '') return null;
  // NUL 截断：`/app.js\0.png` 这类在 C 层的文件 API 上会被截到 \0 之前。
  if (path.includes('\0')) return null;
  const root = resolve(webDir);
  // 前缀一个点，保证 `/app.js` 这种绝对路径被当成**相对于 root** 来解析，
  // 而不是被 resolve 当成盘符根（那样它必然越界，全部静态文件都会 404）。
  const resolved = resolve(root, '.' + path);
  const rel = relative(root, resolved);
  if (rel === '' || rel.startsWith('..') || isAbsolute(rel)) return null;
  return resolved;
}
