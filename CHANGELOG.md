# 变更记录

格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；版本号遵循语义化版本。

产品版本（本文件）与契约版本（`project_schema` / `host_api`）是两条独立的线：只有契约变化的发布，产品版本仍要动 —— 下游钉固的是产物 zip 的字节。

## [0.4.1] — 文字边缘的两处真缺陷（白线 / 距离场）

**修的是文字边缘**：描边压填充的白线、距离场换精确 EDT。含 0.4.0（文字纵向锚）一并发布 —— 契约号与 0.4.0 相同（`host_api = 7`、`project_schema = 1`），下游换产物即可，代码不用动。

### Fixed
- **描边压填充的抗锯齿边冲成纯白**：`tint()` 输出直排 RGB（颜色没乘 alpha），`overlay()` 按预乘合成，缺这一步预乘会让部分覆盖的像素相加后冲过 255 被夹。修后定点 `(244,241,240)` → `(127,113,104)`；「亮>232 且紧邻暗描边」像素 3870 → 1212。判据 `描边压填充的抗锯齿边不许冲成白色`。
- **距离场改为精确欧氏距离**（Felzenszwalb–Huttenlocher 抛物线包络，O(n)）：原 chamfer 3-4 的等值线是八边形、带方向性误差，描边/白环半径随方向变。硬跳占比 0.433 → 0.012（r=6）、0.431 → 0.022（r=12）。判据 `到墨迹的距离是精确欧氏距离`、`斜边膨胀后不许出现硬跳`（45° 边膨胀 6px，硬跳占比 < 5%）。

### Added
- `crates/dhampir-worker/examples/jag_ab.rs`：栅格化 A/B 量具，同一文本/字号/字体下分别走 drawtext 与 libass，打印边缘软硬与台阶统计，并落 5× 放大 PNG。

### 已知残留
- 描边/白环边界仍有 ~0.1~0.3px 抖动（膨胀输入是像素格上的墨迹集合）；用覆盖度做亚像素修正非净收益，未采用。

## [0.4.0] — 文字纵向锚（`anchor_bottom`）

`host_api` **6 → 7**（`project_schema` 仍为 1）。下游**先换产物，再改 `DHAMPIR_HOST_API`**；只改一边会报版本不匹配、预览整个不可用。

### Added
- **`SubtitleStyle::anchor_bottom`**：文字块**底边**的归一化 y，`1.0` = 贴底 = 缺省；宿主据此分档（上 / 中 / 下）摆文字。不要用 `bottom_margin` 做纵向 —— 它同时是左右安全宽的输入。

### Changed
- **`safe_width_ratio` 现在真的被 `layout()` 读到**：显式配（`> 0`）就只认它管左右安全宽；不配（缺省 `0`）仍复用 `bottom_margin`。不写这两个字段的既有工程逐字节不变（有单测钉着）。
- **修掉 `crates/dhampir-wasm` 在 `--target wasm32-unknown-unknown` 下编不过**：`0e3209b` 把轨道级样式搬成逐条携带（`track_style`）时漏改 wasm 侧。`cargo build -p dhampir-wasm` 与 `cargo test` 都是绿的，出问题的代码只在 wasm target 才编到（`check-dual-end` 因此一直红）。

### 兼容性

| 面 | 变没变 |
|---|---|
| 工程文件（`project_schema = 1`） | 老工程照读，一个键都不多（`anchor_bottom` 带 `skip_serializing_if`） |
| `dhampir_host_api_version` | **6 → 7** |
| 既有字段 | 全都不变 |

## [0.3.0] — 播放速率（`source.source_rate`）

**契约号未动**（`project_schema = 1`、`host_api = 6`）：加的是可选字段，缺省即旧行为；但产物字节变了，产品版本照动。

```json
{"source": {"asset_id": "clip", "source_in": 60, "source_rate": 2.0}}
```

`end - start` = 输出帧数；`source_rate` = 每输出 1 帧消耗几个源帧（缺省 `1.0` = 原速 ⇒ 老工程逐字节不变）。

- **音轨一起做**：读 `output_samples × rate` 个采样点再 `atempo` 压回来；`rate < 0.5` 自动拆多级（`atempo` 单级限 `[0.5, 100]`），时间伸缩不变调。
- **坏速率不静默**：`rate <= 0` / `NaN` / `inf` 由 `probe` 报 `source_rate_invalid`；`rate > 100` 明确报错。越界检查也按 rate 算。
- **落点逐帧精确**：`rate=2.0`、`source_in=60` 时出片第 i 帧 = 源第 `60+2i` 帧，30/30 帧吻合。
- **量具注意**：解码带 `-vf scale=out_color_matrix=bt709`，源的 **Y（tv 16..235）** 转成 **R（全范围 0..255）**，出片的 `R` ≠ 源的 `Y`。完整更正见 [plan/d5-frame-rate-handoff.md](plan/d5-frame-rate-handoff.md) §10。

## [0.2.0] — 字形回退、渲染能力面、Web 动画对齐

**契约号未动**（`project_schema = 1`、`host_api = 6`），但字幕栅格化器从 `drawtext` 换成 libass，**产物字节变了**。

### ⚠️ 破坏性：老工程的字幕像素会变

契约字段没动，下游不会在任何契约校验里收到提示 —— 做过视觉基线 / 截图回归的**必须重拍基线**（判定口径不用改，图要重拍）。同字号边缘斜坡宽度 0.63~1.43 px → 1.63~3.17 px，每字推进宽度 40 px → 38 px。无字幕工程不受影响（`fixtures/sample-project.doc.json` 第 15 帧 sha256 逐字节相同，**读数、无自动守卫**）。

### Added
- **字形回退（缺字不再是方框）**：字体缺字形时 libass 逐字形回退；此前 `drawtext` 画 `.notdef` 空心方框且退出码为 0。复现与位证见 [plan/glyph-fallback-evidence.md](plan/glyph-fallback-evidence.md)（乐米波波体缺 `靥 U+9765`）。
- **字体定位正路**：`--font-dir` + 契约的 `font_family`，`--font-file` 退居兜底。见 [docs/usage.md](docs/usage.md)。
- **渲染能力面**：`blend_fn`、`gradient`、`path` / `polygon` 四个渲染模块 + 对应 WGSL；契约侧新增 `easing.rs`、`path.rs`。
- **WAAPI/CSS 动画对齐**：缓动全集、`anim2doc` 转译器、DOM 第二宿主 `web/dom-host.mjs`；`scripts/check-anim-eval.mjs` 逐帧逐通道与 core 对照（容差 1e-4，实测最大偏差 9.37e-6）。演示见 `fixtures/effects-demo.json`、`web/effects-demo.html`（WASM / DOM 两版）与 `web/effects-demo/effects-demo-contact-sheet.png`。

### Changed
- **`drawtext` → libass（`subtitles=`）**，取舍见 `text_raster.rs` 模块头。
- **字体路径含非 ASCII 字符不再失败**：原来 ffmpeg 段错误（退出码 139）且 0 字节输出；现在把字体复制到 ASCII 临时路径再用，用后清理。
- **三条静默路改成响亮失败**：字体目录不存在、目录里没有可用字体、`font_family` 给了却找不到 —— 都退非零退出码并点出是哪个名字、哪个目录。
- **覆盖度口径**：libass 输出到 `rgba` 时 alpha 恒为 0、覆盖度在 RGB，且黑描边与透明底逐字节相同（`(0,0,0,0)`）；故填充与描边都画成白墨，颜色交回 `tint`。

### 已知限制
- **描边颜色当前不受 `stroke_color` 控制**：填充与描边共用一张覆盖掩码，已用判据钉住。
- **字形回退落到哪份字体取决于机器上装了什么字体**（同机同目录两次出片一致）：回退字体集合由下游宿主提供，接入见 [docs/upgrade-0.1.0-to-0.2.0.md](docs/upgrade-0.1.0-to-0.2.0.md)。

### 依赖前置
- ffmpeg 必须带 `--enable-libass`（此前只需 `--enable-libfreetype`）。自检：`ffmpeg -filters | grep subtitles`。

## [0.1.0] — 首个发布版本

首个正式发布版本（此前都在占位号 `0.0.1` 下开发、从未发布）。契约：`project_schema = 1`、`host_api = 6`。

### 发布物
- `dhampir-0.1.0-win32-x64.zip` 与 `dhampir-0.1.0-linux-x64.zip`（各带 `.sha256.txt`，Linux 产物 2026-10-04 补）；内含 `bin/dhampir(.exe)`（出片）、`preview/`（浏览器预览）、`VERSION`、许可件

### 这一版有什么
- **同一份 `dhampir-core` 编到两个宿主**：native（headless wgpu 出片）与 wasm32（浏览器 WebGPU 预览），两条腿跑出逐字节相同的 golden 报告。
- **动图（GIF / 动画 WebP）原生解码**：宿主交 GIF 原始字节，引擎自己解、常驻显存、按帧号供帧；预览与出片同一个解码器。接入见 [docs/host-animation-integration.md](docs/host-animation-integration.md)。
- **Apache-2.0 单许可**（2026-09-30 由 `MIT OR Apache-2.0` 收窄），自带专利授权，允许商用与闭源集成。

### 已知限制
- 本仓只做渲染与预览底座；网关 / 任务队列 / 对象存储 / 部署 / UI 产品化都在下游。
- 预编译产物只覆盖 Windows x64 与 Linux x64，其余平台需自行构建（`cargo build --release` + `wasm-pack`）。
- `check-web-invariants` / `check-dual-end` 要求 `crates/dhampir-wasm/www/pkg` 比源码新：克隆后跑 `wasm-pack build crates/dhampir-wasm --target web --out-dir www/pkg --dev`。

### Linux 产物（2026-10-04 补）
- `scripts/package.mjs` 压包按平台分叉：Windows 走 .NET `ZipFile`，Linux / macOS 走纯 Node（只依赖 `node:zlib`）。
- zip 写 Unix 权限位：`external_attr` 高 16 位 + `create_system = 3`，可执行位进 `verifyZip` 判据。
- 边界：构建机上的现产物，不是交叉编译；只证明「Linux 上能构建、打包、解包能跑」。

### 发布前收口（同日）
- `cargo fmt --all` 848 处 / 65 文件；`cargo clippy --workspace --all-targets -- -D warnings` 原 23 条归零（NaN 判据与 `too_many_arguments` 属误报，用 `#[allow]` 保留原判据）。
- `-- --ignored` 的 9 条要真字体，装 `noto-fonts-cjk` 后 35 passed / 0 failed（原 26/9）。
- 复核：四大 CI 命令 EXIT=0、705 passed / 0 failed、双端 SSIM 1.000000 / MAE 0、`records/m1` 摘要 `71ecc80cade3d73d` 复现。
- 仍红一条：`check-m2-record.mjs` 的 `diff-images`（归档差异图重编码后与盘上字节不同），与本版改动无关，登记为 **D21**。
- 文档去掉「把某个下游宿主当唯一宿主」的写法；`scripts/bench-animation-upload.mjs` 素材目录改为参数必填；补 `LICENSE`（与 `LICENSE-APACHE` 逐字节相同）。
