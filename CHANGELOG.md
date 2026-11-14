# 变更记录

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本号遵循语义化版本。

⚠️ **契约版本与产品版本是两条独立的线**：产品版本（本文件）是发布节奏，
`project_schema` / `host_api` 是**兼容性承诺**。一次只有后者变化的发布，产品版本仍要动 ——
因为下游钉固的是产物 zip 的字节，不是契约号。

---

## [未发布]

### 构建 / 发布

- **Linux 产物容器化**：新增 `docker/Dockerfile.linux-x64`（ubuntu:24.04，glibc 2.39 基线，
  Rust 钉 1.97.0）与 `scripts/release-local.sh`（Linux 容器内编译 + Windows cargo-xwin 交叉编译）。
  起因与下游宿主同一条：在滚动发行版（Arch，glibc 2.44）上直编的 ELF 要求 `GLIBC_2.43`，
  在我们要支持的所有发行版上都起不来，而构建期看不出任何问题。
  ⚠️ 容器里用**独立 target 目录**（`target-docker/`）：宿主与本容器 rustc 同版本时指纹可复用，
  共享 target 会让容器"编译零动作"、把宿主链接的二进制原样留下。
- **`scripts/package.mjs` 加 glibc 门禁**：linux-x64 产物实测最高 `GLIBC_x.y`，超过 2.39
  （`MAX_GLIBC`，与下游宿主 xtask 同一条线）拒收 —— 读的是真出来的二进制，纯字节扫描，
  不依赖 objdump / readelf。逃生口 `--allow-host-glibc` 显式放行并留痕（只许"产物仅本机跑"）。
- **`scripts/package.mjs` 支持预构建二进制与目标平台**：`--bin <路径>`（容器 / cargo-xwin
  交叉编译产物，跳过本机 cargo build；wasm 半仍在宿主机出）与 `--platform`
  （VERSION 的 `platform=`、zip 稳定名、可执行位自检跟着**目标**平台走，交叉编译时必须显式给）。
  顺手补了 `--help`（以前任何参数都会被当成"真跑"，对一个全量构建脚本是最坏的帮助方式）。
- ⚠️ **产物目录（`dist/dhampir-<版本>+<sha>/`）是"最后一次构建"的暂存区**：双端先后构建时，
  目录里的 `bin/` 反映最后一次；分发以**稳定名 zip** 为准（`dhampir-<版本>-linux-x64.zip` 与
  `dhampir-<版本>-windows-x64.zip` 各一份，各自留 sha256 侧车）。下游宿主打包时要么在同一轮
  平台构建后立刻取目录（宿主 release-local.sh 的顺序就是"先构建底座、再打包宿主"），要么
  显式 `--dhampir-exe` 指定平台正确的二进制。

---

## [0.1.0] — 首个发布版本

第一个正式发布的版本。此前所有开发都在 `0.0.1` 这个占位号下进行，**从未发布过**。

### 发布物

- `dhampir-0.1.0-win32-x64.zip`（+ `.sha256.txt`）—— 下游宿主钉固这一个
- `dhampir-0.1.0-linux-x64.zip`（+ `.sha256.txt`）—— **2026-10-04 补**（见下「Linux 产物」）
- 内含：`bin/dhampir(.exe)`（出片）、`preview/`（浏览器预览）、`VERSION`、许可件

### 契约版本

- `project_schema = 1` —— 工程文件格式
- `host_api = 6` —— wasm 导出面

### 这一版有什么

- **同一份 `dhampir-core` 编译到两个宿主**：native（headless wgpu 出片）与 wasm32（浏览器 WebGPU 预览），
  两条腿跑出**逐字节相同**的 golden 报告（M0 起就是硬判据）。
- **动图（GIF / 动画 WebP）原生解码**：宿主交 **GIF 原始字节**即可，引擎自己解、自己常驻显存、
  自己按帧号供帧 —— 不再需要宿主逐帧栅格化后喂位图。预览与出片**用同一个解码器**，
  所以贴纸相位不会各算各的。接入见 [docs/host-animation-integration.md](docs/host-animation-integration.md)。
- **Apache-2.0 单许可**（2026-09-30 由 `MIT OR Apache-2.0` 收窄；收窄前未发布过任何版本）。
  自带专利授权，允许商用与闭源集成。

### 已知限制

- 本仓只做渲染与预览底座：网关 / 任务队列 / 对象存储 / 部署 / UI 产品化都在下游。
- 预编译产物覆盖 **Windows x64 与 Linux x64** 两个平台；其余平台需自行构建
  （`cargo build --release` + `wasm-pack`）。
- `check-web-invariants` / `check-dual-end` 要求 `crates/dhampir-wasm/www/pkg` 比源码新；
  克隆后需跑一次 `wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev`。

### Linux 产物（2026-10-04 补）

0.1.0 首发时只出了 Windows 产物，而 README 的命题是「同一个底座编译到两个宿主」、
服务端出片的目标环境就是 Linux —— 只发一个平台的产物与那句话是矛盾的。
这次把 Linux 那条路补齐，并修掉路上真实存在的两个断点：

1. **`scripts/package.mjs` 的压包只认 PowerShell**（Linux 上通常没有 `powershell`，
   实测 ENOENT）—— 于是流程走到压包就断，Linux 根本出不了产物。
   现在按平台分叉：Windows 仍走 .NET `ZipFile`（原样不动），
   Linux / macOS 走**纯 Node**（只依赖 `node:zlib`，不引入 `zip` 命令行依赖）。
2. **zip 里没写 Unix 权限位**：解压出来 `bin/dhampir` 是 `-rw-r--r--`，**跑不起来**。
   而"zip 打得开、文件都在"这类自检完全发现不了它。现在写 `external_attr` 高 16 位
   （`create_system = 3`），并把**可执行位**加进 `verifyZip` 的判据 —— 这条是反向验过的
   （拿没修的那份 zip 跑，判据会红）。

边界（没做的、别当成做了）：

- 这份产物是**在构建机上现产物**，不是交叉编译。它证明的是「Linux 上能构建、能打包、
  包解开能跑」，不证明别的发行版 / 别的 GPU 上同样如此。

### 发布前收口（同日）

补齐 Linux 产物时顺带发现：CI 的 `check-native`（跑在 ubuntu + windows 两个平台）
有两条判据是红的，不修就发不出去。都已收：

- **`cargo fmt --all`** —— 848 处 / 65 文件，代码从没按 `rustfmt.toml` 排过。现已绿。
- **`cargo clippy --workspace --all-targets -- -D warnings`** —— 原 23 条，逐条处理后归零。
  其中**三处是 lint 误报必须保留原判据**：`!(x > 0.0)` 与 `x <= 0.0` **在 NaN 上不等价**，
  而挡掉 NaN 正是本仓的意图（`text_layout.rs`、`pipeline.rs`）——用 `#[allow]` + 理由，
  **没有**按 lint 去改判据。`too_many_arguments` 三处同理（参数是 wgpu / 着色器 uniform
  的天然形状）。
- **`-- --ignored` 的 9 条**要真字体（6 条候选路径本机一个都不在，只有 `adwaita` /
  `liberation`）。装 `noto-fonts-cjk` 后 **35 passed / 0 failed**（原 26/9）——
  这补上的是**字幕上屏**在 Linux 上的实测，不是单纯消红。

改完的复核：四大 CI 命令全 EXIT=0、705 passed / 0 failed、双端 **SSIM 1.000000 / MAE 0**、
`records/m1` 整表摘要 `71ecc80cade3d73d` 由重算复现、生成文档 md5 未变
—— 即**行为未变**，改动只碰格式与那几处 allow。

**仍有一条守卫自检是红的**（如实写出来，不算绿）：`check-m2-record.mjs` 红在 `diff-images`
—— 归档的 9 张差异图重编码后与盘上不是同一份字节。守卫的规矩是「自检先过才谈结论」，
所以它现在的结论不可信。**已确认与本次改动无关**（把 `crates/` 全部改动 `git stash`
撤下后报错**逐字相同**；`records/` 自 2026-10-01 未动），登记为 **D21**。
它不影响本版产物，但属发布说明该提的一句。

### 本版前的仓库侧整理

- 去掉文档与注释里「把某个下游宿主当成唯一宿主」的写法（引擎本身是宿主中立的）。
- `scripts/bench-animation-upload.mjs` 的素材目录不再写死本机路径，改为**参数必填**。
- 补 `LICENSE`（GitHub 许可证识别器只认标准名；与 `LICENSE-APACHE` 逐字节相同）。
