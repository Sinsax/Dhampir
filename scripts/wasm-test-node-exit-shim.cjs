// 接管 wasm-bindgen 的 node 测试壳对 process.exit 的调用。
//
// ---------------------------------------------------------------------------
// 为什么需要它
//
// 本机（Windows 10 + Node v25.5.0）直接跑 `wasm-bindgen-test-runner <test.wasm>`，
// 会在**测试全部通过之后**报失败：
//
//     test result: ok. 4 passed; 0 failed; 0 ignored; 0 filtered out
//     Assertion failed: !(handle->flags & UV_HANDLE_CLOSING), file src\win\async.c, line 76
//     Error: Node failed with exit_code -1073740791
//
// 退出码于是变成 1，与测试结果毫无关系。这会毁掉唯一不该被放弃的判据：
// 只看日志里有没有 `test result: ok`，会把守卫改成永远绿（上一次的 ok 字样完全
// 可能留在被截断的日志里）。
//
// ---------------------------------------------------------------------------
// 根因（实测排除过，别重复走弯路）
//
// 第一版以为是"stdout 是管道才触发"，于是把 stdout 换成文件句柄——**错的**，
// 当时只跑了一次，恰好没崩。反复实测（每格 12 次，同一二进制）：
//
//   变体                           崩溃    退出 0
//   不给任何垫片                   10/12    2/12
//   推迟一个 tick 再真退           9/12     3/12
//   完全接管退出（本文件）          0/12    12/12
//   接管 + 嵌套 setImmediate 再真退  3/12     9/12
//
// 结论：**只要真的调用了被强制执行的 process.exit()，就可能撞上 libuv 的
// UV_HANDLE_CLOSING 断言**；把它推迟几个 tick 只是降低概率，不能根除。
// 让 node 自己把事件循环跑干、自然退出，才是干净的。
//
// 所以这里**根本不调用真正的 process.exit**，只设 process.exitCode。
//
// ---------------------------------------------------------------------------
// 这不掩盖失败
//
//   - 测试通过 → 测试壳调 process.exit(0) → exitCode=0 → node 自然退出 0
//   - 测试失败 → 测试壳调 process.exit(1) → exitCode=1 → node 自然退出 1
//   - 崩溃     → 异常终止，退出码是负数，仍然是非零
//
// 退出码原样传递，只是换了一条不撞 libuv 的路径。
//
// 代价：如果测试壳留下了活着的句柄，事件循环不会干，node 会**挂住**而不是退出。
// 所以调用方必须给自己设超时（scripts/run-wasm-tests.mjs 里设了），别让"挂住"
// 伪装成"还在跑"。
//
// 用法（由 scripts/run-wasm-tests.mjs 自动设置）：
//   NODE_OPTIONS=--require <本文件绝对路径>
//
// 注意：NODE_OPTIONS 不解析带空格的 --require 路径（反斜杠会被整个吃掉），
// 所以调用方必须保证本文件所在路径不含空白字符。

process.exit = (code) => {
  if (typeof code === 'number') process.exitCode = code;
};
