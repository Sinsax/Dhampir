# records/m1 —— M1 里程碑记录

这一目录里全是**产物**，不是感想。每份文件都能被重新算一遍：命令写在第一行、
原始 stdout/stderr 一字未改、退出码单独记。human 能读，机器也能读。

M1 = 「服务端 headless wgpu 基线」。它要回答的只有一件事：**headless wgpu 能不能
在目标环境离屏出图，并且出得稳定**——同一台机器、同一个后端、同一条命令跑第二遍，
80 张 PNG 要逐字节相同；两个后端之间也要逐字节相同。

---

## 一句话结论

`acceptance.json` → `green: true`、`exit_code: 0`，9 条判据全绿（最终重跑时间见
`acceptance.json` 的 `generated_at`）。

撑起 M1 的是三个数：

1. **跨进程**：第二次运行与第一次运行，两条腿各 80 帧，逐帧**像素摘要与文件摘要两样全同**，
   整表摘要都是 `71ecc80cade3d73d`（`dx12/compare.json`、`vulkan/compare.json` 的
   `identical: true`、`matched_frames: 80`）。
2. **跨后端**：DX12 与 Vulkan 的 80 张同名 PNG **逐字节相同**（直接逐字节比对，80/80）。
3. **预算**：1080p 单帧最慢一趟「渲染 + 读回」往返 DX12 `2.289 ms` / Vulkan `2.217 ms`，
   预算 `10 ms`，`verdict: true`。

**缺口**：Linux 那两条腿（Linux/GPU、Linux/lavapipe）还没跑——本机没有 docker、
WSL 里没有发行版，**⏳ 待补**。退出标准要求四种环境全部记录，所以这一条现在只是 2/4。

---

## 现场（复核这些文件的人需要知道的前提）

| 项 | 值 |
|---|---|
| OS | Windows 10 build 19045（x64） |
| CPU | AMD Ryzen 7 9700X 8-Core |
| GPU | NVIDIA GeForce RTX 4070（DX12 驱动 `32.0.16.1074`；Vulkan 驱动 `NVIDIA` / `610.74`） |
| rustc / cargo | 1.97.0（`rust-toolchain.toml` 钉住；edition 2024，MSRV 1.87） |
| Node | v25.5.0 |
| wgpu / naga | 30.0.1 / 30.0.1（从 `Cargo.lock` 读；后端 feature 只在宿主开） |
| 构建 | `cargo build --release -p dhampir-worker --bin dhampir-render` → `7710208` 字节 |
| 跑 corpus 的可执行文件 | `target/release/dhampir-render.exe` |
| 本机没有的东西 | docker、WSL 发行版（所以 Linux 两条腿 ⏳） |

---

## 文件清单

### ① 两条腿：`dx12/` 与 `vulkan/`（各 5 份文件 + 80 张 PNG）

两个后端各写进自己的目录（`backend_slug`），文件名完全相同——**同样的名字放在不同的
目录里**，比给文件加后缀更容易被人按腿遍历。

| 文件 | dx12 | vulkan | 里面是什么 |
|---|---|---|---|
| `run.json` | 305100 B | 305102 B | corpus 的机器可读全量：`scenes[]` 注册表 + 80 帧 × 采样点 + `counts` + `frames_digest` |
| `readings.txt` | 112215 B | 112215 B | 人读的逐点读数（531 行，80 个帧标题 + 368 行读数） |
| `timing.json` | 3181 B | 3190 B | 1080p 计时（每场景 24 次）+ 行对齐探针；**每次都变**的那一半 |
| `adapter.json` | 884 B | 890 B | adapter / wgpu 版本 / 构建 profile / 探针摘要；**几乎不变**的那一半 |
| `compare.json` | 894 B | 896 B | 第二个进程写的跨进程比对结论 |
| `frames/*.png` | 80 张 | 80 张 | `frames/{scene}-f{frame:03}.png`，256×256 `Rgba8UnormSrgb` |

PNG 一共 160 张、259238 字节（单张 823–2194 字节）。文件名三位补零是**有意的**：
字典序 == 帧号序，`ls` 一遍就是时间顺序。

### ② 记录根：探针、验收、守卫、模型交叉核对

| 文件 | 里面是什么 |
|---|---|
| `acceptance.json` | 9 条判据的 `command` / `exit_code` / `ok` / `seconds`；`green` / `exit_code`；跑在哪个提交上（`commit` / `dirty`） |
| `native-check.txt` / `native-tests.txt` / `wasm-check.txt` / `cross-runtime.txt` / `guard-*.txt` | 同名的原始 stdout 与 stderr（分开写） |
| `wasm-tests.json` | `cross-runtime` 的机器可读版（2864 B）：两个 target 的退出码与阶段耗时 |
| `selfcheck-native.txt` | native 侧的 72 行纯逻辑探针报告全文（4061 B）：`fnv1a64 c3f0da6b37577e55`，与 M0 归档的那份**同一个摘要** |
| `model-cross-check.mjs` | 11159 B 的一次性脚本：**独立于 `scene_model.rs` 重写**了一遍五个场景的数值模型，用来交叉核对（不是守卫，不会被自动跑） |

> `selfcheck-native.txt` 单独存在的理由：M1 往 `dhampir-core` 里加了一整个渲染模块，
> 而 M0 的 `cross-runtime` 判据锚在这份报告的字节上。它的摘要没变，就是"探针契约
> 没被 M1 碰坏"的直接证据。

---

## 数字（每个都能被重新算出来）

| 项 | DX12 | Vulkan |
|---|---|---|
| `requested` / `adapter_name` | `DX12` / NVIDIA GeForce RTX 4070 | `VULKAN` / 同一个 GPU |
| `adapter.backend` | `Dx12` | `Vulkan` |
| 帧 / 采样点 | 80 / 368 | 80 / 368 |
| `counts` | `failed 0`、`out_of_range 0`、`unjudged 0`、`clean true` | 同 |
| 同帧两次渲染 | `repeat_mismatches: []` | 同 |
| 整表摘要 `frames_digest` | `71ecc80cade3d73d` | 同 |
| init | `268.345 ms` | `126.412 ms` |
| 每场景往返中位数 | 2.119–2.289 ms | 2.099–2.217 ms |
| 每场景纯 CPU 提交中位数 | 0.103–0.124 ms | 0.058–0.074 ms |
| 预算判决 | `worst_roundtrip_ms 2.289 ≤ 10` → `true` | `2.217 ≤ 10` → `true` |
| 最慢单次往返（只记不判） | blur `3.214 ms` | blur `2.312 ms` |
| 行对齐 | 1366×768：`5464 → 5632` 填充后可比，`1049088` 像素，最差距离 `0`，`exercises_padding: true` | 同 |

场景集（`run.json.scenes[]`，五场景共 23 个采样点 × 16 帧 = 368）：

| 场景 | 趟数 | 采样点 | 随帧变 | 考什么 |
|---|---|---|---|---|
| `gradient` | 1 | 5 | 是 | 插值与 8 位量化精度（含锯齿相位） |
| `checker` | 1 | 7 | 是 | 像素级棋盘：光栅化坐标的精确性 |
| `srgb_linear` | 1 | 4 | 是 | sRGB ↔ linear 往返（8 条竖条） |
| `alpha_stack` | 1 | 2 | 是 | 多层半透明叠加：混合顺序与 alpha 源因子 |
| `blur` | 3 | 5 | **否** | 可分离高斯三趟 + 边缘 clamp 语义 |

**判据是"与 clamp 模型的字节距离 ≤ 1"**（`byte_tolerance: 1`）。这不是"差不多就行"：
记录里每个采样点都带 `measured` / `expected` / `distance` / `detail`，`detail` 里写着
理想字节离舍入边界有多远。缺陷模型的距离是 2 到 78 字节，比容差高一个量级。

---

## M1 退出标准的三条，现在到哪一步

| # | 原文 | 状态 |
|---|---|---|
| ① | 目标环境（含 Linux 容器）能跑出 PNG，且重复运行逐字节一致 | **Win 两条腿达成**；Linux 容器 **⏳ 待补** |
| ② | 四种环境（Win/DX12、Win/Vulkan、Linux/GPU、Linux/lavapipe）的 adapter 与通过情况全部记录 | **2/4**：两条腿的 `adapter.json` 已归档；Linux 两条 **⏳ 待补** |
| ③ | 1080p 单帧渲染 ≤ 10ms（不含读回）——起始值，按实测定档 | **达成**：判的是「渲染 + 读回」往返（含 GPU，故为渲染时间的**高估**），最慢 2.289 ms ≤ 10 ms；预算仍留 10 ms（见下面的坑 3） |

`plan/video-editor-plan.md` §4 里，只有真做完的才勾：Linux 相关的那两条**没有勾**，
旁边写了 ⏳ 与缺口原因。

---

## 怎么重跑

```bash
# 0) release 二进制
cargo build --release -p dhampir-worker --bin dhampir-render

# 1) 第一条腿 → 临时目录（不碰归档）
target/release/dhampir-render.exe --scene all --frames 0..16 --out target/m1-p1

# 2) 第二条腿 → 记录目录，同时逐帧与第一条腿比（结论落进 <out>/<slug>/compare.json）
target/release/dhampir-render.exe --scene all --frames 0..16 --out records/m1 --compare-run target/m1-p1

# 3) 纯逻辑探针报告（selfcheck-native.txt；不碰 GPU）
target/release/dhampir-render.exe --probe-only --out records/m1

# 4) 9 条判据（会覆盖 acceptance.json 与 9 份 *.txt）
node scripts/record-acceptance.mjs --milestone m1

# 5) 只跑记录守卫 / 只跑它自己的自检
node scripts/check-m1-record.mjs --record records/m1
node scripts/check-m1-record.mjs --self-test
```

`--backend` 默认 `all`，所以 1) 与 2) 各自**一个进程**就跑完两个后端。corpus 路径
**必须显式给 `--out`**：默认值 `records/m0` 是已归档的记录，不给就被拦下来。

---

## 读这些记录时会踩的坑

1. **`scenes[].samples[].expected` 是第 0 帧的那一列。** 四个 `uses_frame: true` 的场景
   随帧变，别的帧的 `expected` 与这张表不同**才是对的**。要核某一帧，看
   `backends[0].frames[].points[].expected`，别看注册表。
2. **`frames_digest` 是"像素摘要"的整表摘要，不是文件摘要。** 规则：按帧序拼
   `场景名 + 0x00 + 帧号(LE u32) + 像素摘要(LE u64)` 再 FNV-1a 64。文件摘要另有
   `png_digest`（对文件字节）——两样都在，是因为它们回答两个不同的问题：
   "文件没被改过" vs "这些像素就是那次渲染出来的"。
3. **判据用的是中位数，不是极值。** 每场景 24 次（`TIMING_REPEATS: 24`）取中位数，
   极值照记（DX12 blur 单次最慢 `3.214 ms`）。桌面机上单次采样由合成器与调度主导，
   拿极值判"能不能稳定出帧"会把噪声当结论。本机连单次最慢也在预算内，所以这个选择
   在这里不影响判决——换一台机器要重新看。
4. **两个计时数别混。** `worst_frame_cpu_ms` 量的是 CPU 侧编码 + `submit`（异步，不含 GPU），
   `worst_roundtrip_ms` 量的是渲染 + 读回往返。**判预算的是后者**——前者没有能力否证
   "一帧画完 ≤ 10 ms"。记录里 `budget_metric` / `budget_metric_note` 把这件事写死了，
   守卫会按 `worst_roundtrip_ms` **自己重算一遍** `verdict`，对不上就红。
5. **后端名有两套拼法，这是决定不是笔误。** `records/m0/*.json` 里的 `Backends(DX12)`
   是 wgpu 的 `Debug` 输出，原地冻结、**不追溯改写**；M1 起走 `baseline::backend_label`
   的 `DX12` / `VULKAN`。所以 `adapter.json` 里 `backend: Dx12`（wgpu 枚举的 Debug）
   与 `requested_backends: DX12`（人的标签）会同时出现，`backend_slug: dx12` 是文件名。
6. **`adapter.json` 与 `timing.json` 是刻意拆开的。** 前者"几乎不变"，后者"每次都变"；
   两份共用同一个 `unix_epoch_millis`（守卫会真的比对这两个数），键**刻意不重叠**。
7. **`compare.json` 是第二个进程写的。** 它不属于被比的那一次运行——它比的是
   "本目录的 `run.json`" vs "`--compare-run` 指过去的那份 `run.json`"，两侧摘要都记
   （`frames_digest` / `other_frames_digest`）。只有一份 `run.json` 归档，跨进程那一半
   靠这份比对结论与可重跑命令立住。
8. **`run.json` 声明了 `nondeterministic_fields: []`。** corpus 产物里没有任何时间项；
   时间戳住在 `adapter.json` / `timing.json`（那两份各自声明了自己的非确定项）。
9. **记录守卫不重算第 0 帧以外的着色器模型。** 守卫是 JS，模型在 Rust 里：它对
   `uses_frame: false` 的场景要求"每帧都等于表"，对 `uses_frame: true` 的要求
   "至少有帧偏离表"，并逐点重算 `distance` 与 `passed`——但**不**声称自己验过
   第 1..15 帧的数值正确性。那部分由 `dhampir-core` 的模型测试与 `model-cross-check.mjs`
   负责，别把守卫的绿读成"每帧数值都被独立复算了"。
10. **`readings.txt` 与 `run.json.frames[]` 是同一次运行的两种写法。** 守卫会把两边
    的帧标题与读数列数出来对一遍，但它们不是"两个独立证据"。

---

## 这份记录**不**证明什么

- **不证明 Linux 上跑得通。** 只有两条 Windows 腿；Linux/GPU 与 Linux/lavapipe **⏳ 待补**。
  退出标准的第 ① ② 条因此还没闭合。
- **不证明"单帧纯渲染 ≤ 10 ms"。** 判据是往返（含读回）——这是**高估**，
  高估只漏报不误报；真要测纯 GPU 时间需要 `TIMESTAMP_QUERY`，那不是到处都有的能力。
- **不证明别的 GPU / 别的驱动上的表现。** 这些数来自一块 RTX 4070 与两台驱动；
  `frames_digest` 的跨后端一致性只说明"这两个后端在这块卡上把同一份 WGSL 跑成了
  同样的字节"，不说明换一块卡也这样。
- **不证明性能上限。** 这里只有"一帧的往返时间"，没有任何吞吐/并发/多帧流水线的结论。
- **不证明 Safari / Firefox 上跑得通。** M1 完全不碰浏览器（那是 M2 的事）。
- **不证明 CI 跑得通。** 仓库还没建，`.github/workflows/ci.yml` 要在首发推送时才第一次真跑。

---

## 相关文档

- 计划与决策真相：`plan/video-editor-plan.md`（§4 是 M1 的任务与退出标准）
- 记录格式的上一份：`records/m0/README.md`（M0 的产物与坑）
- 项目总览与构建方式：`README.md`（仓库根）
- 守卫与记录工具：`scripts/`（`record-acceptance.mjs`、`check-m1-record.mjs`、
  `check-core-purity.mjs`、`check-dep-graph.mjs`、`check-text-hygiene.mjs`、
  `run-wasm-tests.mjs`）
