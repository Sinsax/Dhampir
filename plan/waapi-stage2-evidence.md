# 阶段 2（waapi2doc 转译器）证据

> 上游：[plan/web-animation-parity.md](./web-animation-parity.md) 阶段 2；
> 口径：[plan/web-animation-criteria.md](./web-animation-criteria.md) 的 D1 / D2 / D3。
> 想知道今天绿不绿，按下面命令重跑 —— 本文件存的是**当时的原始读数**。

## 1. 交付物

| 文件 | 是什么 |
|---|---|
| `fixtures/waapi-snapshot.sample.json` | WAAPI 快照样本（形状照 `getKeyframes()` + `getTiming()` 裁剪）。三条动画：两条能转、一条**故意表达不了** |
| `scripts/waapi2doc.mjs` | 转译器：快照 → 工程文件 + 量化报告。研发期放本仓，成熟后整份搬去下游（本仓不存放下游方言） |
| `scripts/check-waapi2doc.mjs` | 判据守卫（已登记进 `run-guards.mjs` 的清单），带 `--self-test` |

## 2. 链路是通的（端到端，有像素证据）

```
node scripts/waapi2doc.mjs fixtures/waapi-snapshot.sample.json target/waapi-demo/out.doc.json -> exit 0
cargo run -q -p dhampir-worker --bin dhampir -- probe --project target/waapi-demo/out.doc.json -> exit 0
  {"errors": [], "warnings": []}
cargo run -q -p dhampir-worker --bin dhampir -- frame --project ... --frame 15 --out ... -> exit 0
  target/waapi-demo/frames/frame-0015.png（1920x1080）
```

**像素读数**（Pillow；样本快照里带了 `background = #101014`）：

```
size (1920, 1080) RGBA
corner (16, 16, 20, 255) / center (16, 16, 20, 255) / bottomright (16, 16, 20, 255)
distinct colors: 1   top: [(2073600, (16, 16, 20, 255))]
```

整帧**只有一种颜色**，而且正是快照里那个 `#101014` —— 说明这条链把数据一路带到了像素：
快照 → 工程文件 → 载入校验 → 渲染 → PNG。（图层这一版还没有素材/特效，所以画面只有底色；
底色的价值在于它是**零图层**的落地方式，正好用来判定 `render_hints` 有没有被吃进去。）

## 3. 判据（守卫）

```
node scripts/check-waapi2doc.mjs --self-test -> exit 0
  ✓ waapi2doc：结构、off-by-one 的缓动、量化留痕、明确跳过、确定性与 --strict 都对
  ✓ 自检：8 个变异全部被抓住（守卫会红）
node scripts/run-guards.mjs --list -> exit 0（对表通过）
```

守卫钉的八条：`schema == 4`、层区间正长度、关键帧 target 在契约的五个通道里、关键帧不落在层外、
**off-by-one 的缓动挂在终点键**（样本的既定答案）、层 id 全局唯一、量化记录三件套齐全、
**表达不了的那条必须出现在报告里**（静默丢弃即红）。另加两条全局判据：同一输入两遍产出**逐字节相同**；
带 `--strict` 跑样本必须退 2。

## 4. 口径落实（对着 criteria 逐条）

| 口径 | 落地 | 样本上的读数 |
|---|---|---|
| D1 一次量化 | offset 先落到层区间再量化，用 BigInt 有理数取整 | 10 个量化点全部留痕，最大误差 **0.0000 帧** |
| D2 缓动字符串原样透传 | 写进 doc 的就是浏览器给的那个串 | `cubic-bezier(0.2, 0.8, 0.4, 1)`、`steps(4, jump-both)` 都原样 |
| D2 off-by-one | 底座第 i 个键取源第 i-1 个 easing | title 的 30 帧键挂 `cubic-bezier(...)`，0 帧键是未使用的 `linear` |
| D3 delay 用层区间吸收 | `start = delay` | note 层 `[9,28)`（delay 300ms @30fps = 9 帧） |
| D3 iterations/direction 超限**明说不支持** | 不展开、不近似 | badge（iterations=3 + alternate）被跳过并写进报告 |
| 一条动画一条轨 | 同轨层不许重叠（probe 的 `layer_overlap`） | 2 条动画 → 2 条轨 |

## 5. 这一阶段**没做**的（写下来，不假装）

1. **HTML 宿主（阶段 3）没开始** —— 现在还没有"第二个宿主"可以对照。
2. `direction` 只支持 `normal`：反向/往返要把缓动**镜像**，而镜像 `cubic-bezier` 与镜像 `steps` 是两回事，
   这一版明说不支持（宁可跳过，也不近似）。
3. `iterations` 只支持 1..64 的整数次（不展开镜像遍）。
4. `iterationStart` 只支持整数（非整数相位要额外一次量化）。
5. `scaleX` / `scaleY` 在契约的五个通道里**表达不了**（只有等比 `scale`）→ 进警告，不静默丢。
6. `fill: none` 表达不了"不进画面"，按 `fill: both` 处理并报警告。
7. 图层这一版没有素材与特效，所以画面只有 `render_hints.background`。

## 6. 环境备注（与阶段 1 那条并列）

本会话**连 stdout 管道都起不来**：`spawnSync(node, ..., { stdio: ['ignore','pipe','pipe'] })` 直接 EPERM。
这与仓库 `scripts/spawn-tool.mjs` 里记的"只有 stdin 管道不行、输出管道是好的"**不是同一台会话的读数**。
所以新守卫 `check-waapi2doc.mjs` 用**文件重定向**捕获子进程输出（`stdio: ['ignore', fd, fd]`）——
那不是垫片：起不来时 `status` 是 null，判据照红（fail-closed）。
