# 变更记录

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本号遵循语义化版本。

⚠️ **契约版本与产品版本是两条独立的线**：产品版本（本文件）是发布节奏，
`project_schema` / `host_api` 是**兼容性承诺**。一次只有后者变化的发布，产品版本仍要动 ——
因为下游钉固的是产物 zip 的字节，不是契约号。

---

## [0.1.0] — 首个发布版本

第一个正式发布的版本。此前所有开发都在 `0.0.1` 这个占位号下进行，**从未发布过**。

### 发布物

- `dhampir-0.1.0-win32-x64.zip`（+ `.sha256.txt`）—— 下游宿主钉固这一个
- 内含：`bin/dhampir.exe`（出片）、`preview/`（浏览器预览）、`VERSION`、许可件

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
- 只提供 **Windows x64** 预编译产物；其他平台需自行构建（`cargo build --release` + `wasm-pack`）。
- `check-web-invariants` / `check-dual-end` 要求 `crates/dhampir-wasm/www/pkg` 比源码新；
  克隆后需跑一次 `wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev`。

### 本版前的仓库侧整理

- 去掉文档与注释里「把某个下游宿主当成唯一宿主」的写法（引擎本身是宿主中立的）。
- `scripts/bench-animation-upload.mjs` 的素材目录不再写死本机路径，改为**参数必填**。
- 补 `LICENSE`（GitHub 许可证识别器只认标准名；与 `LICENSE-APACHE` 逐字节相同）。
