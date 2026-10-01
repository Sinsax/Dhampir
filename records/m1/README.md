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

**两条 Linux 腿已补**（2026-10-01）：`linux-gpu/`（AMD Radeon 680M / RADV）与
`linux-lavapipe/`（llvmpipe 软渲染）。两条腿各 80 帧，**两次独立运行逐字节一致**
（各自的 `compare.json`：`identical: true`、`matched_frames: 80`），`probe_digest` 与
golden 相同。退出标准要的四种环境因此**齐了（4/4）**。

⚠️ **它们与 Windows 那两条腿的 `frames_digest` 不同**：`679b249510eea426`（RADV）、
`e2291e1bf32ddef6`（llvmpipe），而 Windows 的两条都是 `71ecc80cade3d73d`。**换了 GPU
与驱动，字节不必相同** —— 退出标准①说的是「**重复运行**逐字节一致」，那由每条腿自己的
`compare.json` 给出；跨机器/跨驱动的逐字节一致**不是**这份记录的主张（见下面「不证明什么」）。

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
| 本机没有的东西 | ~~docker、WSL 发行版~~（2026-10-01 起本机有 docker；两条 Linux 腿已补，见下） |

### 补记（2026-10-01）：Linux 两条腿的现场

| 项 | 值 |
|---|---|
| OS | Arch Linux（内核 7.2.3，**裸机**，非虚拟化） |
| CPU | 16 核（x86_64） |
| GPU（`linux-gpu/`） | AMD Radeon 680M `RADV REMBRANDT`（集成显卡，驱动 `radv` / Mesa `26.2.3-arch1.1`） |
| GPU（`linux-lavapipe/`） | `llvmpipe (LLVM 22.1.8, 256 bits)`（CPU 软渲染，驱动 `llvmpipe`） |
| rustc / cargo | **1.98.1**（发行版系统包。本机没有 rustup，**没有**按 `rust-toolchain.toml` 的 1.97.0 跑） |
| Node | v26.10.0 |
| wgpu / naga | 30.0.1 / 30.0.1（从 `Cargo.lock` 读，与 Windows 那条一致） |
| 构建 | `cargo build --release -p dhampir-worker --bin dhampir-render` → `7652728` 字节 |
| 运行环境 | Arch 容器（`--device=/dev/dri` 直通，`VK_ICD_FILENAMES` 选后端）；核外宿主**没有** Mesa/ffmpeg |
| 运行语料 | `--scene all --frames 0..16 --backend vulkan`，两条腿各跑**两次**（第二次带 `--compare-run`） |

**两条腿的读数**（`timing.json` / `run.json`）：

| 腿 | 整表摘要 | 1080p 最慢往返 | 预算 10 ms | 重复运行 |
|---|---|---|---|---|
| `linux-gpu`（RADV） | `679b249510eea426` | **2.421 ms** | `verdict: true` | 逐字节一致（80/80） |
| `linux-lavapipe` | `e2291e1bf32ddef6` | **29.581 ms** | `verdict: false` | 逐字节一致（80/80） |

lavapipe 超预算**是设计使然**：plan 写明它是 CPU 软渲染、「性能掉一个数量级，只用于链路验证」。
数字照记，门槛不套 —— 拿预算判它等于把设计选择当成回归（守卫的 `checkLinuxLegs` 就是这么写的）。

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

### ③ 两条 Linux 腿：`linux-gpu/` 与 `linux-lavapipe/`（各 5 份文件 + 80 张 PNG）

与 ① 的 Windows 两条腿**同形**（同样五个文件 + `frames/`），只是跑在不同的机器与驱动上：

| 腿 | 适配器 | `frames_digest` | `compare.json` | `build_profile` |
|---|---|---|---|---|
| `linux-gpu/` | AMD Radeon 680M（RADV，Mesa 26.2.3） | `679b249510eea426` | `identical: true`（80/80） | `release` |
| `linux-lavapipe/` | llvmpipe（LLVM 22.1.8，CPU） | `e2291e1bf32ddef6` | `identical: true`（80/80） | `release` |

**两条腿的整表摘要与 Windows 那两条不同**（`71ecc80cade3d73d`）—— 换 GPU/驱动不保证逐字节一致，
所以守卫对这两条腿**另有一套口径**（`scripts/check-m1-record.mjs` 的 `checkLinuxLegs`）：不跟 Windows
比字节、不套 10 ms 预算（lavapipe 是 CPU 软渲染），只判形状与自证——帧数与场景集、`probe_digest`
等于 golden、`build_profile` 是 `release`、以及 `compare.json` 必须说两次运行逐字节一致。

归档时用的是 `--backend vulkan`：`--scene all` 在 Linux 上会把不存在的 DX12 腿也跑一遍并因此退 1。

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
| ① | 目标环境（含 Linux 容器）能跑出 PNG，且重复运行逐字节一致 | **达成（4/4）**：Windows 两条腿（见上）+ Linux 两条腿（`linux-gpu/`、`linux-lavapipe/`）各自两次运行逐字节一致 |
| ② | 四种环境（Win/DX12、Win/Vulkan、Linux/GPU、Linux/lavapipe）的 adapter 与通过情况全部记录 | **4/4**：四条腿的 `adapter.json` / `timing.json` / `run.json` / `compare.json` 都已归档 |
| ③ | 1080p 单帧渲染 ≤ 10ms（不含读回）——起始值，按实测定档 | **Windows 达成**（最慢 2.289 ms）；**Linux/GPU 达成**（2.421 ms）；**Linux/lavapipe 超预算**（29.581 ms）—— CPU 软渲染的设计使然，plan 只把它当链路验证 |

`plan/video-editor-plan.md` §4 里，Linux 相关的那两条**已勾上**（2026-10-01）。

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

### Linux 两条腿（2026-10-01 补）

```bash
# 0) 容器：要 Vulkan ICD（RADV + lavapipe）与 /dev/dri 直通；挂在**同一绝对路径**上
#    （测试产物里烤着 CARGO_MANIFEST_DIR，挂到别处会让草稿目录变成别的地方而 EACCES）
docker run -d --name dhampir-linux --network host --device=/dev/dri \
  -v "$PWD":"$PWD" -w "$PWD" archlinux:latest sleep infinity
docker exec dhampir-linux pacman -Sy --noconfirm mesa vulkan-radeon vulkan-icd-loader vulkan-swrast

# 1) release 二进制（宿主上编，容器里跑）
cargo build --release -p dhampir-worker --bin dhampir-render

# 2) linux-gpu：第一次 → 临时目录；第二次 → 归档目录，并逐帧比第一次
docker exec -u 1000:1000 -w "$PWD" dhampir-linux \
  target/release/dhampir-render --scene all --frames 0..16 --backend vulkan --out target/m1-linux-gpu-p1
docker exec -u 1000:1000 -w "$PWD" dhampir-linux \
  target/release/dhampir-render --scene all --frames 0..16 --backend vulkan \
  --out target/m1-linux-gpu-p2 --compare-run target/m1-linux-gpu-p1

# 3) linux-lavapipe：同一套流程，只把 ICD 指到软件渲染那条
docker exec -u 1000:1000 -w "$PWD" -e VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
  dhampir-linux target/release/dhampir-render --scene all --frames 0..16 --backend vulkan \
  --out target/m1-linux-lvp-p1
docker exec -u 1000:1000 -w "$PWD" -e VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
  dhampir-linux target/release/dhampir-render --scene all --frames 0..16 --backend vulkan \
  --out target/m1-linux-lvp-p2 --compare-run target/m1-linux-lvp-p1

# 4) 归档：把第二次那份 <out>/vulkan/ 原样拷成腿目录（工具固定写 <out>/<slug>/）
cp -a target/m1-linux-gpu-p2/vulkan/. records/m1/linux-gpu/
cp -a target/m1-linux-lvp-p2/vulkan/. records/m1/linux-lavapipe/

# 5) 复核
node scripts/check-m1-record.mjs --record records/m1   # linux-legs 这条判据会按它们自己的口径查
```

**为什么不用 `--scene all` 的默认（两个后端都跑）**：Linux 上没有 DX12，那条腿会红；
`--backend vulkan` 让这一趟只跑存在的那个后端，退出码才是 0。

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
   两份共用同一个 `unix_epoch_millis`（守卫会真的比对这两个数，并校验
   `unix_epoch_seconds === floor(ms / 1000)`）。但"拆开"**不等于"键不重叠"**——
   实测 18 / 15 个键里有 **10 个同名**，其中 `kind`（`"adapter"` vs `"timing"`）与
   `nondeterministic_fields`（各自的非确定项清单）两键**值不同**，其余 8 个
   （`adapter_name`、`backend_slug`、`build_profile`、`milestone`、`requested_backends`、
   `schema`、`unix_epoch_millis`、`unix_epoch_seconds`）刻意取同值。拆开说的是
   **非确定项各归各**，不是"没有同名键"。
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

- **不证明"换台机器也逐字节一样"。** 四条腿的 `frames_digest` 是两组：Windows 两条都是
  `71ecc80cade3d73d`，Linux 两条分别是 `679b249510eea426`（RADV）与 `e2291e1bf32ddef6`（llvmpipe）。
  这份记录主张的是「**同一环境重复运行一致**」，不是「跨 GPU/驱动一致」。
- **不证明 Linux/lavapipe 的 10 ms 预算。** 它 29.581 ms —— CPU 软渲染本来就只用于链路验证。
- **不证明 Windows 与 Linux 能力对等**：Linux 那两条腿跑在**容器**里（Vulkan ICD 来自容器），
  核外宿主没有 Mesa；而且构建用的是发行版 cargo 1.98.1，不是 `rust-toolchain.toml` 钉的 1.97.0。
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
- 本目录的独立复核（另一位复核者、隔离克隆）：`records/m1/review-independent.md`
  ——判定 **PASS**。它提的 2 条中等问题（记录守卫的诚实性检查可被一个**空壳 `linux/`
  目录**绕过；依赖守卫看不见 `crates/` 下没有清单的目录）已在本轮**补了守卫与反向自检**：
  诚实性只认"**完整**的腿"（5 份必备文件 + `frames/` 里有 PNG），空壳 `linux*` 目录
  直接红，缺腿的 ⏳ 还必须与 Linux **同一行**；`crates/` 下每个目录都必须在 members 里。
  4 条低问题是数字与措辞，已改：`键不重叠`→ 实测 10 键同名（见上面的坑 6）、
  `183 文件`→ 185、`254 行`→ 253（`wc -l` 口径）；`docker` 那条经实测确认本机确实
  没有 docker，原结论在写下时成立；**2026-10-01 起本机有 docker，两条 Linux 腿已补**（见上面的补记）—— 这条的前提已经不在了（可关闭）
- 记录格式的上一份：`records/m0/README.md`（M0 的产物与坑）
- 项目总览与构建方式：`README.md`（仓库根）
- 守卫与记录工具：`scripts/`（`record-acceptance.mjs`、`check-m1-record.mjs`、
  `check-core-purity.mjs`、`check-dep-graph.mjs`、`check-text-hygiene.mjs`、
  `run-wasm-tests.mjs`）
