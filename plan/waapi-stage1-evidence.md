# 阶段 1（CSS 缓动）证据

> 上游：[plan/web-animation-parity.md](./web-animation-parity.md) 阶段 1；
> 口径：[plan/web-animation-criteria.md](./web-animation-criteria.md) 的 D2。
> 本文件存**原始读数**，不随代码走 —— 想知道今天绿不绿，按下面的命令重跑。

## 1. 做了什么

| # | 改动 | 文件 |
|---|---|---|
| 1 | 新增 CSS 缓动解析器与求值：关键字 / cubic-bezier() / steps()（四个跳跃位置）、零依赖、纯数学 | crates/dhampir-timeline/src/easing.rs（新） |
| 2 | Easing 从单元枚举改成能承载任意 CSS 缓动串（单元变体保留 + Css(String) 兜底），**手写 serde**：既有拼写原样写回 | schema.rs |
| 3 | 求值改走 crate::easing::parse(..).apply(t) —— 公式只有一份 | schema.rs / curve.rs（调用点不变） |
| 4 | 校验层新增 unknown_easing（两条路都加：文档路与 v1 路） | project.rs / schema.rs |
| 5 | 生成物重生成（easing 的 schema 形状从 oneOf 变成 string） | schema/*.{schema.json,d.ts} |
| 6 | 浏览器参考数据的采集工具（真值来源，不是本仓算的） | web/easing-probe.html、scripts/easing-reference.mjs |

## 2. 读数（本机 Windows / MSVC）

```
cargo test -p dhampir-timeline --lib          -> exit 0（289 passed / 0 failed）
cargo test --workspace --no-fail-fast         -> 只有 2 条既有红（见下）
node scripts/timeline-contract.mjs            -> exit 0
  ✓ 时间线契约与派生物一致（3 种形态 × schema + TS 类型）：doc-v1 / timeline-v4 / timeline-v1
```

**既有红（与本阶段无关，改动前就在）**：dhampir-worker --bin dhampir 两条 ——
绝对_uri_按书写形态判而不按平台判、素材表里的绝对_uri_在_linux_上也不挂到_asset_root_。
根因（已定位，未修）：is_absolute_uri（dhampir-worker/src/bin/dhampir.rs:986）的第 1 条规则是
raw.is_absolute()，而 **Windows 上 Path::is_absolute( 斜杠开头的路径 ) 是 false** ——
也就是按书写形态判这条承诺在 POSIX 根这一格上没落实到 Windows。
修它要同时改 scripts/dhampir-local.mjs 的 isAbsoluteUri（注释写着不许只改一边）。

## 3. 新增判据（都在 easing.rs 的测试里）

| 判据 | 钉什么 |
|---|---|
| 下划线与连字符是两条曲线_不许被归一化 | ease_in（二次）≠ ease-in（cubic-bezier）—— 手滑改名就是换曲线 |
| 恒等曲线与线性一致 / css_ease_在中点的量级正确 | 贝塞尔求解器解对 |
| steps_的内部点是闭式算出来的 | 四个跳跃位置的公式 |
| 过冲的缓动真的会冲过头 | **输出不许被夹**（回弹的全部意义） |
| 缓动的取值原样往返_老工程一个字节都不变 | D2 的核心承诺：既有拼写读进来写出去**同一个串** |
| 认不出来的缓动在契约层是可判定的 | unknown_easing 可被校验层报出；热路径退回线性只为不 panic |
| 与浏览器原生缓动逐值对得上（**ignored**） | 阶段 1-B：需要浏览器采的 target/easing-reference.json |

## 4. 本机 agent 会话怎么跑 cargo（给下一个人）

本会话的 bash / pwsh 工具**不可用**（子进程管道被拒：Win32 error 5 / EPERM）。可行的路子（已实测）：

1. 用 run_code 里的 Node spawnSync，**把 stdout/stderr 重定向到文件**（在 target/ 下），不要用管道；
2. process.env 是**空的**，且真实 PATH 里没有 MSVC / Windows SDK ⇒ 要显式拼出 PATH、INCLUDE、LIB
   （MSVC 14.44.35207/bin/Hostx64/x64 与 Windows Kits/10/bin/10.0.26100.0/x64 等）；
3. **TEMP/TMP 必须指到工作区里**（否则链接器报 LNK1104 打不开自己的临时文件）；
4. 加 windowsHide: true（不在桌面上弹控制台窗口）。

这一坨封装在 target/agent-build.mjs（**target/ 不进 git**，所以这里记一笔）。
生成 schema/ 派生物时同样要绕：scripts/timeline-contract.mjs 用 execFileSync(…pipe…) 起 cargo，
在本会话必 EPERM —— 做法是复制一份到 target/，把捕获方式换成重定向到文件再读回来，
产出先落 target/（程序只能写 target/），再由文件工具搬进 schema/。

## 5. 这一阶段**没做**的

1. **浏览器逐值对照没有跑过**（target/easing-reference.json 尚未采集）—— 所以目前只有**本仓内部**的
   证据（公式对表），没有真值对表。命令：node scripts/easing-reference.mjs，然后
   cargo test -p dhampir-timeline --lib -- --ignored 与浏览器。
2. **TS 侧的字面量联合类型退化**：schema/*.d.ts 里 easing?: Easing 变成了 easing?: string，
   五个既有字面量在 TS 侧没了（JSON Schema 现在是裸 type:string）。取舍与建议见 D2「派生物的代价」。
3. unknown_easing 要不要进 scripts/check-web-invariants.mjs 的 FORBIDDEN_IN_APP（未决）。
