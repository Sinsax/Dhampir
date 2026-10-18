# 0.1.0 → 0.2.0 下游对接说明

**给下游宿主（V-Trim）**。产物 `dhampir-0.2.0-win32-x64.zip`。

## 0. 一句话

契约号未动（`project_schema = 1`、`host_api = 6`），**但字幕像素变了**：栅格化从 `drawtext` 换成 libass。

## 1. 必须做的三件事

| # | 做什么 | 为什么 |
|---|---|---|
| 1 | 重拍含字幕的视觉基线（截图回归 / SSIM / PSNR） | 字幕每帧像素都变 |
| 2 | ffmpeg 带 `--enable-libass`（**运行时**要求） | 0.2.0 起的硬前置，出片那一刻才失败 |
| 3 | 字体改走 `font_family` + `--font-dir` | 缺字回退要靠它 |

自检 `ffmpeg -filters | grep subtitles`（有输出 = 带了 libass）。不打包 ffmpeg 的形态：在 doctor / 启动自检里跑它，缺了就明确告诉用户装带 libass 的 ffmpeg；随应用打包的形态：打包时确认一次。

## 2. 破坏性变更：字幕像素会变

只影响含文字层（字幕 / 弹幕）的工程；无字幕工程整帧不变。判定口径（SSIM / PSNR / 边缘口径）不用改：边缘抗锯齿斜坡 0.63 ~ 1.43 px → 1.63 ~ 3.17 px（更柔），字距每字推进 40 px → ~38 px（有漂移）。

不用做：改契约校验、改工程文件、改 API 调用。

## 3. 契约面：没动，但是"加了一堆能用"

`host_api = 6` 未动：`dhampir_host_api_version()` 仍返 6，导出面零变化（0 新增 / 0 删除）。`project_schema = 1` 未动，schema 纯新增：

| 能力 | 新字段 |
|---|---|
| 裁剪形状 | `clip`、`kind`（circle / ellipse / inset）、`center`、`radius`、`radius_x`、`radius_y`、`corner_radius`、`left`/`top`/`right`/`bottom` |
| 渐变 | `gradient`、`stops`、`angle_deg`、`offset_x`、`offset_y` |
| 遮罩 / 蒙版 | `mask`、`coverage`、`invert`、`channel` |
| 模糊 / 阴影 / 路径 | `blur_sigma`、`shadow`、`points`、`at`、`data` |
| 背景 | `backdrop_effects`、`background`（**只在 `doc-v1`**，即 `render_hints.background`） |

`blend` 字段没变，可用值 4 → 9（+ `darken` / `lighten` / `overlay` / `soft_light` / `difference`；0.1.0 里这 5 个是枚举占位，`is_implemented()` 返 false）。新增字段数：`timeline-v4` 25、`doc-v1` 26（多 `background`）。`clip` 只支持 circle / ellipse / inset（圆角矩形），**`polygon()` 与 `path()` 明确不做**。

## 4. 字体：这是 0.2.0 最需要你配合的一块

0.1.0 的 `drawtext` 不回退字形（缺字画成 `.notdef` 方框、退出码仍 0）；0.2.0 的 libass **逐字形回退**。

| | 0.1.0 | 0.2.0 |
|---|---|---|
| 主路 | `--font-file <文件>` | 工程 `subtitle.font_family` = **族名** + `--font-dir <目录>` |
| 兜底 | — | `--font-file` |

两个坑都静默：`font_family` 必须是字体内部的真族名（文件名主干 / 英文内部名都会解析成 `ArialMT`，且 `lines_failed: 0`、`issues: []`）；回退落到哪份字体取决于机器上装了什么。回退字体集合**由下游提供**（2026-10-08 已答复）：字体库目录随包分发、传给 `--font-dir`；老工程里 slug 形态的 `font_family` 要转成真族名。

## 5. 你这边的改动清单（建议顺序）

1. 换产物 `dhampir-0.2.0-win32-x64.zip` + `.manifest.json`（+ `.sha256.txt`）；钉固流程读 `.manifest.json`，别抄本文档的数字。
2. 确认 ffmpeg 带 libass（第 1 节）；字体改走 `font_family` + `--font-dir`（真族名）；重拍视觉基线。
3. （可选）放开新 schema 字段（裁剪 / 渐变 / 混合 / 遮罩）到 UI；（已定）回退字体集合由你提供 + slug 转族名。

## 6. 怎么确认你拿到的是哪一版

产物根目录的 `VERSION`：`version=0.2.0`、`project_schema=1`、`host_api=6`、`license=Apache-2.0`、`git=<构建时的 sha>`、`built_at=<构建时间>`、`platform=win32-x64`。`git` / `built_at` 以 `VERSION` 为准；**没有** `dhampir_version()`（wasm 侧只有 `dhampir_host_api_version()`，返 6），运行时只能读 `VERSION` 文件。

## 7. 0.2.0 的其他新增（可能对你有用）

| 能力 | 在哪 |
|---|---|
| 渲染能力面：混合 / 渐变 / 路径 / 多边形 | `dhampir-core` 的 `render/{blend_fn,gradient,path,polygon}.rs` + 对应 WGSL |
| WAAPI / CSS 动画对齐 | `web/anim-eval.mjs`、`web/dom-host.mjs`、`scripts/waapi2doc.mjs` |
| 效果演示 | `fixtures/effects-demo.json` + `web/effects-demo.html` |

特性支持的真值：[web/capabilities.json](../web/capabilities.json)（28 条，`scripts/check-capabilities.mjs` 核对）：supported 15 / partial 9 / explicitly-not-doing 3 / needs-primitive 1。

## 8. 已知边界（如实记，别当已解决）

| 项 | 状态 |
|---|---|
| 描边颜色当前不受 `stroke_color` 控制 | 填充与描边共用一张覆盖掩码；已用判据钉住 |
| 回退字体集合归属 | 已答复（2026-10-08）：由下游字体库提供，slug 要自己转族名 |
| 无字幕帧 sha256 无自动守卫 | 该 hash 不在代码 / 守卫里，是读数不是断言 |
| 非 Windows 平台 | 全部实测都在 Windows + gyan.dev ffmpeg 上做的，没验过 |
| `1.25` / `288` 补偿常数 | 只测过 msyh / 乐米 / NotoSansSC 三个字体族 |

## 9. 本版的验证读数（可复算）

`cargo test --workspace` 766 passed / 0 failed；`cargo check --all-targets` 0 warning；`node scripts/run-guards.mjs` 26 / 26 全绿（含 `--self-test`）；真机测试 9 / 9；`node scripts/licenses.mjs --check` EXIT=0。产物 `dist/dhampir-0.2.0-win32-x64.zip`，zip 的 sha256 / git / built_at 以 `.manifest.json` 与 `.sha256.txt` 为准。
