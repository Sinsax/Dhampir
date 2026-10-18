# 调用面（API）

承诺边界与入口地图：形状的真值 CLI 看 `dhampir --help`，wasm 看 [host-api.md](host-api.md) 与 [api-surface.md](api-surface.md)（后者是**生成**的，守卫钉着不许漂）；动图（GIF / 动画 WebP）宿主接入见 [host-animation-integration.md](host-animation-integration.md)。

## 一、承诺等级
| 面 | 承诺 | 变更规则 |
|---|---|---|
| **CLI 退出码 0 / 2 / 1** | **承诺**。脚本可以依赖 | 破坏性变更要改文档并升契约 |
| **CLI stdout 输出形状** | **承诺**（`probe`/`info` 是 JSON，`render` 是 NDJSON） | 加键可以不升；**改键名/删键要升** |
| **wasm 底座 API**（timeline_host / demux） | **承诺兼容** | 形状变化要 `HOST_API_VERSION` +1 |
| wasm 宿主内部（cache） | 可用，**不构成契约** | 随时可改 |
| wasm 取证工具（web / corpus） | **不承诺**，给本仓库验收用 | 随时可改 |
| `preview.rs` 那三个导出 | **已弃用**，别在新代码里用 | 会被删 |

## 二、CLI
二进制名 **`dhampir`**，14 个子命令，分三类。

### 2.1 只读（不写盘、不改工程）
| 子命令 | 要什么 | 出什么 |
|---|---|---|
| `probe --project <文件>` | 无（不要 GPU、不要 ffmpeg） | 校验结果 JSON（DocIssues） |
| `info --asset <文件>` | ffprobe | 尺寸 / 帧数 / 时间基 / GOP 长度 |
| `gop --asset <文件>` | ffprobe | GOP 切片表（顺序解码的定位依据） |
| `library --project <文件>` | 无 | 每个资产被引用了几次 |

### 2.2 出图与出片
| 子命令 | 说明 |
|---|---|
| `frame --project P --frame N --out <目录>` | 出**单帧** PNG |
| `frame --project P --from N --to N --out <目录>` | 出**一段** PNG。**与 `--frame` 互斥**，同时给退 2；一个都不给也退 2 |
| `render --project P --from N --to N --out <文件.mp4>` | 出片。stdout 是 **NDJSON 进度流** |

`frame` 缺省**刻意不对称**：只给 `--from 3` 是「第 3 帧到结尾」，只给 `--to 5` 是「第 0 帧到第 5 帧」。

| 开关 | 做什么 | 要什么 |
|---|---|---|
| `--font-file <ttf/ttc/otf>` | 字幕**烧进画面** | 字体 + GPU；工程有字幕轨时少给就**判失败** |
| `--subtitle-out <文件>` | 字幕另存**侧挂文件** | 什么都不要，**只有 render 认** |
| `--no-audio` | 出片**不要声音** | — |

### 2.3 改工程（全部支持干跑）
`dhampir edit --project P --op '{"op":"split","layer":"c","at":75}'` 是**干跑**（不落盘），加 `--write` 才真写；六个 op：`insert` / `trim` / `split` / `move` / `remove` / `set_sequence`。具名子命令 `clip` / `sequence` / `undo` / `redo` / `batch` 是同一实现的糖（同一个 `apply`、同一条落盘路径），由 `check-cli` 钉在产物字节 + stdout 上。

### 2.4 stdout / stderr 的分工
stdout 只给机器读（JSON / NDJSON），stderr 给人读（诊断、警告、被忽略的东西）；判定**一律看退出码**，别 grep 日志里的 `ok`。

## 三、wasm（浏览器宿主）
入口 `crates/dhampir-wasm/src/`；逐模块定位见 [api-surface.md](api-surface.md)。

### 3.1 调用顺序契约
约束收在 `crates/dhampir-wasm/src/timeline_host.rs` 的模块头（`//!`，第 22–112 行）；错序的症状**都是画面看起来完全正常**。

```text
启动    open(json) -> attach(canvas)
尺寸    resize(w, h) 先于 draw
每帧    sources_for(frame) -> [JS 逐个 seek <video> 并等 seeked]
        -> clear_bitmaps -> set_bitmap(src, bmp) -> draw(frame)
文字    text_frame(frame) -> [JS 栅格化] -> set_text_bitmap / set_danmaku_bitmap -> draw
编辑    edit(op) -> undo/redo -> doc()   （别自己改 JS 副本）
```

## 四、HTTP（本机 / 分离模式的后端）
`scripts/dhampir-local.mjs`：**HTTP 在 Node、渲染在 Rust** 的一层翻译，不是第二套实现；素材位置由 id 经资产索引解释（工程 `assets` 优先，兜底表补缺），**不是**按 id 找同名文件。

| 方法 | 路径 | 背后 |
|---|---|---|
| GET | `/health` | — |
| GET | `/capabilities` | 从 Rust 源码推导（时间线版本、特效清单、上限） |
| GET | `/projects/:id` | 读工程文件 |
| POST | `/validate` | `dhampir probe` |
| GET | `/assets/:id/(info\|gop)` | `dhampir info` / `dhampir gop` |
| GET | `/assets/:id/media` | 素材字节（支持 Range） |
| POST | `/assets` | `dhampir import` |
| GET | `/projects/:id/library` | `dhampir library` |
| POST | `/export` | `dhampir render`（提交 -> 轮询 -> 下载） |
| POST | `/frame` | `dhampir frame`（同步出一帧 PNG） |

### 4.1 `POST /frame`：出一帧真实出片帧
请求体与 `/export` 同形（**工程在请求体里**），多一个帧号：`{ "project": { ...工程... }, "frame": 12 }`；响应 `200`、`content-type: image/png`、body 是 PNG 字节，`x-dhampir-frame` 回帧号，尺寸**不由请求决定**（取工程的 `render_hints` / `sequence_size`，与 `render` 同源）。出错时是 JSON：400 `bad_request`（体不是 JSON）、400 `bad_frame`（帧号不是非负整数）、500 `frame_failed`（CLI 非 0 退出，message 是它的 stderr）、500 `frame_no_output`（报成功但拿不到产物）、503 `cli_missing`（找不到可执行文件）。

**不判逐像素一致**：两端色彩矩阵不同源（台账 D14，wontfix），含解码的双端比对按 D15 是 **unmeasurable**；实测（`probe-layer-a`）4x4 块均值差 4.58、逐像素全等 0.02%，带变换（`probe-layer-d`）20.99、复合（样本工程第 12 帧）14.22。判别：`node scripts/web-check.mjs --verdict realframe --local --project probe-layer-a.doc`。已知**不覆盖**「帧错了」：拿第 0 帧冒充第 12 帧，均值 18.07 vs 18.69。
