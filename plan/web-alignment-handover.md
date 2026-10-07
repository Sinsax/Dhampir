# 网页动画对齐：**交接报告**（快照）

> 这是一份**快照**，写于本会话第 71 轮。凡"活数"都以生成器为准（下面逐条写明谁是谁的活源）：
> 手写的数字会过期 —— 本会话已经因为"手写"栽过好几次，所以这里把活源都指出来。

## 一、要的是什么、拿到了什么

**目标**：按 `plan/web-animation-parity.md` 与 `plan/web-animation-criteria.md` 推进"网页动画对齐"——
网页侧用 WAAPI/CSS 写动画，本仓引擎出片；两端效果要对得上，且**预览与渲染共用同一个文档坐标系**。

| 阶段 | 状态 | 说明 |
|---|---|---|
| **1 缓动解析** | ✅ | CSS **全部**缓动形式：关键字、`cubic-bezier()`、`steps()`（四种跳跃）、**`linear()` 断点表**（本仓最后一条"明确不做"的形式，第 37 轮补上） |
| **2 anim2doc 转译器** | ✅ | 通道 / 缓动（含 `linear()` 镜像）/ 方向（normal/reverse/alternate/alternate-reverse）/ 滤镜（`blur`/`contrast`/`saturate`/`brightness`/`hue-rotate` 的精确或带理由的映射）/ 混合 / 圆角 / 裁剪 / 掩码（含渐变）/ 关键帧参数；**每条既有能力都有判据** |
| **3 HTML 宿主** | 🟡 | 数值一致 ✅ + 形状对表 ✅ + 能力边界写进代码（不能画的**响亮报错**而不是静默画错）✅；**剩一件事要人做：打开页面看一眼**（见 §五 第 3 条） |

## 二、这一路给引擎加的能力（登记表里现在是 `supported` 的 15 条）

- `transform`
- `opacity`
- `keyframes`
- `easing.linear-cubic-steps`
- `easing.legacy-quad`
- `filter.blur`
- `filter.contrast`
- `filter.saturation`
- `filter.brightness`
- `filter.hue-rotate`
- `filter.drop-shadow`
- `backdrop-filter`
- `geometry.corner-radius`
- `geometry.clip-path`
- `geometry.clip-path-polygon`

（计数 {"supported":15,"partial":9,"explicitly-not-doing":3,"needs-primitive":1} —— 活源：`node scripts/check-capabilities.mjs` 的输出）

## 三、判据体系（这是本会话最该被接手的地方）

- **26 条守卫**，**全部**带反向自检；合计约 **428** 条断言/变异（活源：`node scripts/guards-inventory.mjs`）。
- 三条铁规矩现在都有判据盯着：
  1. **老工程逐字节不变**：每轮改动后都重测 `frame(plain)` 的 PNG sha256 = `8b7d9e18a7320ee7`（从第 0 轮到第 70 轮未变）；
  2. **守卫会红**：`scripts/guards-inventory.mjs` 报每条的自检数量；本会话新加的判据都先"故意让它红一次"再收口；
  3. **证据归档**：`plan/waapi-stage1/2/3-evidence.md` 逐轮记录（含失败与回退 —— 那些才是最有用的部分）。
- **两件由台账生成、不会腐烂的工具**：
  - `node scripts/web-parity-pending.mjs` —— 你还要做什么 + **具体怎么做**（从差异台账生成，并**双向查同步**）；
  - `node scripts/guards-inventory.mjs` —— 判据家底（从守卫自己的输出读，不手写）；
  - `node scripts/guard-coverage.mjs` —— **改了实现该跑哪条守卫 / 该改哪份文档**（从守卫自己的题头与它绑定的仓库路径生成；**只读文件不 spawn**，所以在没有管道的环境里也能跑）。

## 四、三本台账（都带判据）

| 台账 | 条数 | 管什么 | 判据 |
|---|---|---|---|
| `scripts/dom-parity-differences.toml` | 6 条 | HTML 宿主与引擎之间**允许的差异**（明确接受 3 条写清理由；未实测的**不许写数**） | `check-dom-differences.mjs`（R1–R7 + 8 个变异） |
| `plan/defects.md` | **26** 条（D16 + A10） | 已知缺陷与架构缺失（每条必须被 roadmap 引用） | `check-defects.mjs`（19 条断言） |
| `plan/web-engine-measurements.md` | 量过 11 行 / GPU 用例索引 29 条 | **正确性**实测（与量性能的 `plan/measurements.md` 分工）+ 复现清单 + **机器口径** | `check-capabilities.mjs` 的索引双向一致规矩 |

## 五、**还没做的**（如实列，含"为什么"）

1. **浏览器侧的掩码上传：两端都接上了 ✓**（第 72/73 轮）—— wasm 宿主有 `dhampir_project_set_mask_image(asset_id, bitmap)` + `BoundVideos::mask_texture_for` + 宿主注册表；JS 侧有 `engine.uploadMasks()`（`open()` 之后 await 一次：`fetch` → `createImageBitmap` → 交给宿主）。**没做的只剩"在浏览器里实测"** —— 我这边的验证只到 `cargo check` 与 `node --check`。安全网在：没交进来的掩码会让 `draw` **响亮报错**（不是静默画一张没有掩码的图）。
2. **DOM 侧的数字**：本会话所有实测都是**引擎侧**；DOM 侧那几条（边缘差异 / `plus-lighter` / 掩码插值 / 逐值缓动对照）**都必须有浏览器**。台账里逐条写明了量法与操作步骤。
3. **真渲染确认**：`web/dom-host.html` 我没法打开看。它现在会画：特效 / 混合（9 条）/ 圆角 / 裁剪（含多边形与路径）/ 掩码（含渐变）/ 投影 / 背景滤镜；不能画的会**报出来**。
4. **GPU 用法索引里 17 条用例没有被登记表点名**（第 69 轮查过）：它们在跑、在守（不少是"拒绝路径"的行为判据），只是条目没逐条引用它们。已在台账里写明这个松耦合。
5. **`asset.uri` 那 2 条既有红**（`crates/dhampir-worker` 的测试）：本会话**没有动它**（它会牵到 `scripts/dhampir-local.mjs` 的 `isAbsoluteUri`）。这是**已知的既有缺陷**，不在本计划范围内，等你决定。

## 六、你接手时的第一件事

```
node scripts/web-parity-pending.mjs     # 你还要做什么 + 具体怎么做（含操作手册）
node scripts/run-guards.mjs             # 26 条一次跑完（本会话的沙箱不给管道，只有你那边能跑）
```

跑完第二条之后：**21/26 绿是当前基线**，5 条红全是环境（wasm pkg 需重建 / 三条要管道 / 一条要浏览器）——
按第一条的清单做完，那 5 条应当全部转绿。

## 七、本目标的出口核对（第 77 轮）

目标原文要求每阶段以可判定出口收口：老工程逐字节不变、守卫会红、证据归档。逐条核：

| 出口 | 证据 | 状态 |
|---|---|---|
| 老工程逐字节不变 | frame(plain) 的 PNG sha256 = 8b7d9e18a7320ee7，从第 0 轮到第 77 轮**每次改动后都重测**，一次未变 | 达成 |
| 守卫会红 | 26/26 条守卫**全部**带反向自检（合计 428 条断言/变异）；本会话**每一条新判据都先故意让它红一次**再收口；两处「读不出来」的假结论也是这么被抓出来的 | 达成 |
| 证据归档 | plan/waapi-stage1/2/3-evidence.md 逐轮记录（含**失败与回退**，那些才是最有用的一段）；三本台账各自有判据：差异 6 条（R1–R7）、缺陷 26 条、正确性实测 11 行 + GPU 用例索引 29 条（含机器口径） | 达成 |

**三阶段**：阶段 1 达成（CSS 全部缓动形式 + 逐值判据备好）；阶段 2 达成（转译器，每条能力都有判据）；
阶段 3 **代码与判据达成**（数值一致 + 形状对表 + 能力边界响亮报错 + 素材掩码两半都接），
**只剩「在浏览器里看一眼」这一条验收** —— 它不是「没做」，是「我做不了」（本环境没有浏览器）。

## 八、第 72–76 轮新增（刷新）

- **素材掩码两端接通**：wasm 宿主有 dhampir_project_set_mask_image(asset_id, bitmap) + BoundVideos::mask_texture_for + 宿主注册表；JS 侧有 engine.uploadMasks()（open 之后 await 一次）。没交进来的掩码会让 draw **响亮报错**。
- **签名级判据**：api-surface 现在比对文档标题里的参数名与 Rust 签名的参数名（第 74 轮那处漂移就是这样漏的，现在会被抓；已现场反向验证）。
- **三件生成式工具**：web-parity-pending.mjs（待办 + 操作手册）、guards-inventory.mjs（判据家底）、guard-coverage.mjs（改了实现该跑哪条守卫 —— 这一条只读文件不 spawn，没有管道也能跑）。
