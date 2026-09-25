# 调用面（API）

这份文件回答两个问题：**这个底座对外承诺什么，以及从哪里进去用。**
每个接口的权威说明在它自己的源码/帮助里，这里给的是地图与承诺边界。

> **形状的真值**：CLI 看 `dhampir --help`；wasm 看 [host-api.md](host-api.md) 与
> [api-surface.md](api-surface.md)（后者是**生成**的，由守卫钉着不许漂）。

---

## 一、承诺等级（读这一节比读名单重要）

| 面 | 承诺 | 变更规则 |
|---|---|---|
| **CLI 退出码 0 / 2 / 1** | **承诺**。脚本可以依赖 | 破坏性变更要改文档并升契约 |
| **CLI stdout 的输出形状** | **承诺**（`probe`/`info` 是 JSON，`render` 是 NDJSON） | 加键可以不升；**改键名/删键要升** |
| **wasm 底座 API**（timeline_host / demux） | **承诺兼容** | 形状变化要 `HOST_API_VERSION` +1 |
| wasm 宿主内部（cache） | 可用，**不构成契约** | 随时可改 |
| wasm 取证工具（web / corpus） | **不承诺**，给本仓库验收用 | 随时可改 |
| `preview.rs` 那三个导出 | **已弃用**，别在新代码里用 | 会被删 |

**为什么要有这张表**：几十个导出长得一样，但其中大半**并不承诺**什么。
不分开就会有人把"内部记账函数"当稳定接口依赖，而它下次就可能换个名字。

---

## 二、CLI

二进制名 **`dhampir`**。14 个子命令，分三类：

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

`frame` 的两个缺省**刻意不对称**：只给 `--from 3` 是"从第 3 帧到结尾"，
只给 `--to 5` 是"从第 0 帧到第 5 帧"。一个都不给**不替你猜**。

出片有两条**互相独立**的可选路：

| 开关 | 做什么 | 要什么 |
|---|---|---|
| `--font-file <ttf/ttc/otf>` | 字幕**烧进画面** | 字体 + GPU。工程有字幕轨时少给就**判失败** |
| `--subtitle-out <文件>` | 字幕另存**侧挂文件** | 什么都不要。**只有 render 认** |
| `--no-audio` | 出片**不要声音** | — |

### 2.3 改工程（全部支持干跑）

```bash
dhampir edit --project P --op '{"op":"split","layer":"c","at":75}'   # 干跑：不落盘
dhampir edit --project P --op '...' --write                          # 真写
```

六个 op：`insert` / `trim` / `split` / `move` / `remove` / `set_sequence`。
**不给 `--write` 就是干跑** —— 这是默认值，不是"没生效"。

具名子命令 `clip` / `sequence` / `undo` / `redo` / `batch` 是**同一实现的糖**：
与 `edit` 走同一个 `apply`、同一条落盘路径。行为不可能有第二份，
这一点由 `check-cli` 钉在**产物字节 + stdout** 上，不是"看起来一样"。

### 2.4 stdout / stderr 的分工

- **stdout 只给机器读**：结构化结果（JSON）或进度（NDJSON）。
- **stderr 给人读**：诊断、警告、被忽略的东西。

这条分工让 `dhampir render ... | jq` 这类用法成立。判定**一律看退出码**，
不要 grep 日志里的 `ok` —— 那是把结论建立在一句话的措辞上。

---

## 三、wasm（浏览器宿主）

入口在 `crates/dhampir-wasm/src/`。**逐模块定位见 [api-surface.md](api-surface.md)**。

### 3.1 调用顺序契约

分散在 `web/engine.js` 六处的约束已经收成一处在
**`crates/dhampir-wasm/src/timeline_host.rs` 的模块头**（`//!` 注释，第 22–112 行）。
下面每一条错了，症状都是**画面看起来完全正常**：

```text
启动    open(json) -> attach(canvas)
尺寸    resize(w, h) 先于 draw
每帧    sources_for(frame) -> [JS 逐个 seek <video> 并等 seeked]
        -> clear_bitmaps -> set_bitmap(src, bmp) -> draw(frame)
文字    text_frame(frame) -> [JS 栅格化] -> set_text_bitmap / set_danmaku_bitmap -> draw
编辑    edit(op) -> undo/redo -> doc()   （别自己改 JS 副本）
```

- `sources_for` 必须在 `draw` **之前** —— 否则画的是上一帧的源位置；
- `clear_bitmaps` 必须在本帧 `set_bitmap` **之前** —— 否则这一帧不再出现的 source
  会**拿着上一帧的位图**被画出来（"慢了半拍"，画面本身正常）；
- `set_bitmap` 必须在 seek **完成之后** —— 否则贴的是上一帧（画面正常、内容是旧的）；
- `text_frame` 必须在 `draw` 之前**且这一帧只调一次** —— 行号按位置编，
  留旧位图会拿**另一条字幕**的像素去贴。

### 3.2 收口记录（62 -> 53 个导出）

删掉的 9 个都是**零调用方**且有证据（全仓 grep + 底层单测仍在）：
`probe_verify`、`corpus_scene_names`、`cache_touch_ram`、`cache_touch_vram`、
`cache_remove_ram`、`cache_ram_budget`、`cache_vram_budget`、
`demux_gop_slices`、`project_clear_text_bitmaps`。

看的人容易误会的一件事：**`web.rs` / `corpus.rs` / `preview.rs` 里的导出没被 `web/` 调用，
不代表它们没用**——前两个是 M0–M2 的**取证工具**（`records/m0`、`records/m2` 的浏览器腿
就是它们跑出来的），`preview.rs` 的三个是 S3.1 实测证据。**删了里程碑记录就不可复现。**

---

## 四、HTTP（本机 / 分离模式的后端）

本机后端 `scripts/dhampir-local.mjs` 是 **HTTP 在 Node、渲染在 Rust** 的一层翻译，
不是第二套实现。路由：

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
| POST | `/export` | `dhampir render`（任务式：提交 -> 轮询 -> 下载） |

**素材位置由 id 经资产索引解释**（工程文件的 `assets` 优先，兜底表补缺），
**不是**按 id 找同名文件 —— 样本工程引用 `a.mp4`，文件却叫 `proxy1080p.mp4`。

---

## 五、想看全部名字

```bash
dhampir --help                          # CLI 的权威说明（永远比文档新）
cat docs/api-surface.md                 # wasm 导出全名单（生成的）
cat docs/host-api.md                    # 返回体形状与版本
```
