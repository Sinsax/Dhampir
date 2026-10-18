# dhampir 使用说明
| 我想 | 去 |
|---|---|
| 五分钟跑通 | [quickstart.md](quickstart.md) |
| 承诺边界、入口地图 | [api.md](api.md) |
| 为什么这样设计 | [../plan/video-editor-plan.md](../plan/video-editor-plan.md) |
| 现在做到哪了 | [../plan/next-steps.md](../plan/next-steps.md) |

`dhampir --help` 永远比这份文档新。
## 一、先决条件
| 需要 | 用途 | 没有它会怎样 |
|---|---|---|
| **Rust 1.97.0** | 全部构建 | `rust-toolchain.toml` 自动切换 |
| **ffmpeg / ffprobe** | 素材探测、字幕栅格化、出片编码 | `info` / `gop` / `render` 直接失败 |
| **GPU（Vulkan/DX12/Metal）** | `frame` / `render` 的合成 | `probe` 能跑，出图不能 |
| **字体文件** | 烧进画面的字幕 | 有字幕轨时 `frame` / `render` **判失败**（`subtitle_font_missing`） |
| Chrome / Edge | 浏览器预览 | 只有预览用得到 |

本仓不内嵌字体、也不猜系统字体：要烧字就给 `--font-file`。
## 二、我想看看工程对不对（不要 GPU、不要 ffmpeg）
```bash
dhampir probe --project fixtures/sample-project.doc.json
dhampir info --asset clip.mp4        # 尺寸 / 帧数 / 时间基 / GOP 长度
dhampir gop  --asset clip.mp4        # GOP 切片表（顺序解码的定位依据）
dhampir library --project P.json     # 每个资产被引用了几次
```

打印 DocIssues。**退出码就是结论**：`0` 过 / `2` 有 error（用户能改）。
## 三、我想出一帧或一段图
```bash
# 单帧
dhampir frame --project P.json --frame 42 --out ./out
# 一段（一张一帧）
dhampir frame --project P.json --from 0 --to 29 --out ./out
```

**`--frame` 与 `--from/--to` 互斥**：同时给退 2。只给一侧时 `--from 3` 是第 3 帧**到结尾**、`--to 5` 是第 0 帧**到第 5 帧**；都不给退 2 —— **不替你猜**。
## 四、我想出一份 mp4
```bash
dhampir render --project P.json --from 0 --to 89 --out out.mp4
```

**stdout 是 NDJSON 进度流**（`start` → 若干 `progress` → `done`），诊断走 stderr，取结论可以：

```bash
dhampir render --project P.json --from 0 --to 89 --out out.mp4 | jq -c 'select(.event=="done")'
```
### 4.1 声音：默认有，除非你明确说不要
工程里有音轨就**默认出声音**；`--no-audio` 明确不要 —— 它的输出与引入音频之前**逐字节相同**，排查声音问题时用这一份做对照。
### 4.2 字幕有**两种口径**，互相独立
| 开关 | 做什么 | 要什么 |
|---|---|---|
| `--font-file <ttf/ttc/otf>` | **烧进画面** | 字体 + GPU。工程有字幕轨时少给就**判失败** |
| `--font-bold-file <ttf/ttc/otf>` | 上面那份的**粗体**（`font_weight >= 600` 时用） | 可选。不给就退回 `--font-file` |
| `--font-dir <目录>` | **按字体族名找字体**的目录 | 可选。工程里写了 `font_family` 时才需要 |
| `--subtitle-out <文件>` | 另存**侧挂文件** | 什么都不要。**只有 render 认**这个开关 |
#### 字体怎么定位：两条来源、一个名字（契约）
按优先级：**① `--font-dir` + 工程里的 `font_family`**（按族名在那个目录里找文件）→ **② `--font-file`**（点名一份，兜底）。`font_family` 是工程契约字段（`SubtitleStyle::font_family` / `DanmakuSpec::font_family`，见 `schema/*.json`），语义就是"从你给我的目录里按这个名字找"：libass 只认家族名、不认文件路径，从文件名推的名字它认不出来，会**静默回退**到系统默认字体（实测 `ArialMT`，无中文字形），而 `lines_failed` 仍是 0、`issues` 仍是空。证据见 [plan/glyph-fallback-evidence.md](../plan/glyph-fallback-evidence.md)。
#### 字体定位的边界
`--font-dir` 指不到目录 → **退出码 2**；目录里一个字体都没有且没给 `--font-file` → **退出码 2**；工程点名了 `font_family` 但目录里没有 → 出片**判失败**（`subtitle_font_missing`，消息点明哪个名字、哪个目录）；两者都没给而工程有字幕 → **判失败**，且**在出片前**就出声。匹配是**归一化后包含**（小写、去非字母数字）：`"Noto Sans SC"` 能对上 `NotoSansSC-Regular.ttf`，不同源时找不到就报错、不静默换。
#### 回退：缺字会借别的字体，跨机可能不同
缺字形时 libass **逐字形回退**到别的字体补出来（`drawtext` 只画 `.notdef` 方框，退出码 0）。落到哪份取决于机器上装了什么（本机实测 `MicrosoftYaHeiUI`）：同一台机器、同一个 `--font-dir` 两次一致，换机器可能换字形；回退集合由下游宿主提供（本仓没有字体栈），见 [upgrade-0.1.0-to-0.2.0.md](upgrade-0.1.0-to-0.2.0.md) 第 4 节。
#### 字幕装不下时：缩字，不是丢行
| 字段 | 默认 | 作用 |
|---|---|---|
| `safe_width_ratio` | `0.0` | 换行安全宽（0 = 沿用 `bottom_margin`，凑出约 88%）；参照实现写死 87.5% |
| `shrink_min_scale` | `0.0` | 装不下时整体缩字号的下限（0 = 不缩，超出就丢行） |
| `keep_all_lines` | `false` | 不截断，折多少行画多少行 |

参照公式 `if (tw > maxLineW + EPS) scale = max(0.7, min(1, maxLineW * 3 / tw));`：`3` 不是截断阈值，只保证缩到恰好三行装得下；贪心折行会折出 4 行，所以**必须同时 `keep_all_lines`**（实测 140 字、`max_lines = 3`、`shrink_min_scale = 0.7`：不加折 4 行丢 1，加上丢 0）。缩字号时描边跟着缩（`swEff = sw * wrapped.scale`）。
#### 高亮词：`<span class="hl">`
字幕文本可写 `<span class="hl">…</span>`，那几段用 `highlight_color` 填色、其余用正文色，**描边仍用 `stroke_color`**；`highlight_color = None` 即不做高亮（既有工程逐字节不变），没有标记时一行只栅格化一次。**两端各有一处已知残差**（段间**水平推进量**）：浏览器用 `ctx.measureText`，量字与画字同一个引擎所以偏移是准的；CLI 的 `drawtext` 的 `text_w` 不能跨过滤器引用，偏移改用**布局的逻辑字宽**累加，全角字准、中英混排的英文半角差几像素。
#### 文字阴影：`shadow_color`
| 字段 | 默认 | 作用 |
|---|---|---|
| `shadow_color` | `None` | 阴影颜色（RGBA，alpha 生效）。`None` = 不画阴影 |
| `shadow_dx_px` | `0.0` | 水平偏移（**文档像素**，与 `transform.x/y` 同一坐标系） |
| `shadow_dy_px` | `0.0` | 垂直偏移（文档像素，**正数向下**）；参照的 `0 2px` 写 `2.0` |
| `shadow_blur_ratio` | `0.0` | 模糊半径（占**文档高**的比，0 = 硬阴影）；`12/1080` 就是参照的 12px |

阴影只画一次、取**填充**轮廓，不参与描边宽度；偏移是文档像素、模糊是比例（求值层乘目标高换成像素）；`None` 与 alpha = 0 都是不画，也不影响弹幕（`DanmakuSpec` 没有阴影字段）。两端只保证观感近似：浏览器用 canvas 原生 `shadowColor` / `shadowOffsetX` / `shadowOffsetY` / `shadowBlur`；CLI 用 `drawtext`（只有白字填充、不带描边）+ `gblur=sigma=模糊半径/2`（÷2 是对齐的那一步）。见 [plan/text-shadow-design.md](../plan/text-shadow-design.md) §2；阴影**不进墨迹报告**。
#### 弹幕的滚动时长：逐条，不是轨道级
`\move(x1,y1,x2,y2)` 跨的是 **cue 窗口**（起止由 `Start`/`End` 定）；`\move(x1,y1,x2,y2,t1,t2)` **显式**给出起止时刻（毫秒，相对 cue 起点），"滚多快"就是 `t2 - t1`。**读不到才回退**到 `DanmakuSpec::duration_ms`（轨道级只能取平均：实测 15.52 / 10.40 / 12.11 / 9.76 秒，平均 11.95 秒，后果是长句滚得太快、短句滚得太慢）。**已知残差**：`\pos(x,y)` 那种静态 cue 没有 `\move` 可读，仍走轨道级。
### 4.3 分块并行（默认关）
```bash
dhampir render --project P.json --from 0 --to 5811 --out out.mp4 --chunk-workers 8
#   --chunk-workers 0 = 自动（min(核数, 4)）；默认 1（关）
```

把区间切成 N 段、各起一套 wgpu 上下文并行渲，最后 `-c copy` 拼起来。收益取决于用哪条腿：

| | workers=1 | auto(4) | 8 |
|---|---|---|---|
| **GPU**（RTX 4070） | 0.0206 秒/帧 | **0.0185**（最好） | 0.0231（变慢） |
| **纯 CPU**（WARP，16 逻辑核） | 0.274 秒/帧 | 0.080 | **0.054**（最好） |

内存按 worker 数涨（各一套 ffmpeg 解码器 + 编码器）：1 → 主进程 ~200 MB + 2 个 ffmpeg ~940 MB ≈ **1.14 GB**；8 → ~878 MB + 17 个 ~3823 MB ≈ **4.7 GB**。**分块会改变字节**（边界处 x264 码率控制不同，实测帧差均值 0.0000–1.9），要逐字节可比就别开。侧挂文件：时间是**相对这一趟产物**从 0 起算的毫秒，内容只有文本与时间（源里的加粗/斜体/颜色不进侧挂），`--format` 不给就看扩展名（`.ass`/`.ssa` → ASS，其余 → SRT），**看不出来不猜**；没给 `--font-file` 时画面一个字都没有，**侧挂文件照写**。
## 五、我想改工程（CLI）
形状是带 `op` 字段的 JSON 对象，**六个操作**：`insert` / `trim` / `split` / `move` / `remove` / `set_sequence`。

```bash
dhampir edit --project P.json --op '{"op":"split","layer":"c","at":75}'   # 干跑
dhampir edit --project P.json --op '...' --write                          # 真写
```

**不给 `--write` 就是干跑**：只在内存里做一遍并打印结果，一个字节都不落盘。
### 5.1 具名子命令（同一实现的糖）
`clip` / `sequence` / `undo` / `redo` / `batch` 与 `edit` **走同一个 `apply`、同一条落盘路径**，行为没有第二份（`check-cli` 钉在产物字节 + stdout 上）。

```bash
dhampir clip insert --project P.json --track v1 --asset a1 --at 0 --source-in 0 --length 30
dhampir clip trim   --project P.json --layer c --edge in --to 10
dhampir clip split  --project P.json --layer c --at 75
dhampir clip move   --project P.json --layer c --to 20 --track v2
dhampir clip remove --project P.json --layer c --ripple
dhampir sequence set --project P.json --timebase 30000/1001 --width 1920 --height 1080 --write
dhampir undo --project P.json --history h.json --write
dhampir redo --project P.json --history h.json --write
```

**每个动作要哪些、认哪些开关都在参数这一关判死 —— 多给一个也报错。** 改帧率会**按时间重算所有序列帧号**，不给 `--timebase` / `--width` / `--height` 的那一项不动。
### 5.2 撤销 / 重做
**历史存哪必须由你说**（`--history <文件>`）。历史文件不在 = 从空历史开始，**不是错误**；没有可撤销的步骤时退出码 `2` 并明说。
### 5.3 批处理
```bash
dhampir batch --project P.json --script ops.txt --history h.json --write
```

脚本是**一行一个 op 的 JSON**（空行与 `#` 开头是注释）。**一次写、一条历史**；中途有一步不成立就**整份不落盘**并明说**卡在第几行**；**空脚本不算成功**。
### 5.4 公共选项
`--asset-root <目录>`（`asset.uri` 的相对根，默认 `target/s3`）、`--asset-map <文件>`（兜底资产表 `assets.<id>.file`，**只补工程没登记的 id**）、`--width` / `--height`（默认取 `render_hints`）、`--font-file`（见 4.2）。
## 六、我想在浏览器里剪
**这里没有打包器**（不用 Vite / webpack / pnpm）：`wasm-pack --target web` 的产物本身就是 ES module，浏览器能直接 `import`。
### 6.1 完整应用（前后端一起起）
```bash
node scripts/web-check.mjs --serve --local
```

同时起**静态服务**与**本机后端**并打印一个 URL（形如 `http://127.0.0.1:5xxxx/?backend=local&port=8802&project=sample-project.doc`），浏览器打开即可，结束用 **Ctrl+C**。**不要把输出接进 `| Select-Object` / `| head`**（管道提前关闭会让它立刻退出）；改前端代码后**刷新页面**（服务带 `no-store`）。
### 6.2 只起服务，人工看
```bash
node scripts/web-check.mjs --serve
```
### 6.3 程序化验收（五种跑法）
```bash
node scripts/web-check.mjs --probe       # W0：工程帧能不能上 canvas
node scripts/web-check.mjs               # 降级模式：逐帧导出 -> FFmpeg 编码
node scripts/web-check.mjs --local       # 本机模式：产品导出路径（提交 -> 轮询 -> 下载）
node scripts/web-check.mjs --remote      # 分离模式的代码路径
node scripts/web-check.mjs --synthetic N # 合成源导出（双端比对用）
```

另有 `--frames-only`、`--canvas WxH`、`--timeout-ms N`、`--ready-only`。默认跑法验"渲染的**帧**对不对"，`--local` 验"交给后端、出片、能下载"。
### 6.4 判定回传（`--verdict`）
```bash
node scripts/web-check.mjs --verdict playback --local
node scripts/web-check.mjs --verdict trim-drag --local
node scripts/web-check.mjs --verdict ui --local          # 界面骨架（DOM 契约）
node scripts/web-check.mjs --verdict realframe --local   # 预览那一帧 vs 出片那一帧
```

页面自己跑一次验收并**主动 POST 回后端**，驱动只读后端；**拿不到就是没拿到，不会伪装成通过**。`--verdict ui` 逐个点 32 个契约 id，并确认 6 个图标按钮里**真的画出了 svg**；`--verdict realframe` 走 `POST /frame` 取回后端同一帧盖在画布上逐像素比，判据取 **4x4 块均值**（单像素最大值对重采样边缘太敏感）外加粗栏杆（块均值 > 60），**不判逐像素一致**（两端色彩矩阵不同源、含解码的双端比对 unmeasurable），差异数字照实打出，量级与不覆盖的东西见 [api.md](api.md) 4.1。
### 6.5 预览性能：实测数据
测量入口：`node scripts/web-check.mjs --verdict perf --local`（一帧里各段各占多少）、`node scripts/web-check.mjs --verdict playthrough --local`（整条时间线播一遍，丢多少帧）。

一帧花在哪：**解码**。`playthrough` 把一帧拆成 `sourcesFor / clearBitmaps / seek / createImageBitmap / setBitmap / text`，四路工程（90 帧）实测**慢帧里 99% 的时间在 `seek`**：`seek 43.85ms`、`text 0.18ms`，其余各 0.00–0.11ms，合计 44.15ms。单次 seek 中位：720p **22ms**、1080p **45ms**、4K **151ms**；30fps 每帧预算 **33.3ms**，链路本身（宿主求值 + 文字 + 画）只有 1ms 上下。

**中位数会骗人**：同一次测量单帧中位 10ms、均值 38ms、最坏 334ms，丢帧 37/90 —— 贵的帧已被丢掉，四个数必须一起看。

**已证伪，别再试**：设 `<video>` 的 `width`/`height` 降解码分辨率（四组对照 `videoWidth` 全是 3840）；多路 seek 并行（2 路 1.01 / 3 路 0.91 / 1 路 0.95，是位置噪声，探针 `node scripts/seek-parallel-probe.mjs`）；关键帧距离（控住位置后无关）；`createImageBitmap` / `set_bitmap` / 文字栅格化（都是 0.00ms）；页面被限速（`visibility=visible`、`focus=true`、rAF 中位 6.9ms）。

**没查清的**：独立探针页里同一段 seek 序列 **0.1ms**、应用页 **40ms** 上下，引擎加载阶段分开量三段都是 38–41ms，同页对照（引擎绑定的 `<video>` 45.2ms vs 同页新建 45.5ms）说明与引擎无关 —— 40ms 是这个页面环境的现状。**要跑满帧率只有两条路**：低分辨率的**预览代理素材**；换一条不走 `<video>` 的解码通路（WebCodecs）。另：稀疏 GOP 素材对逐帧 seek 特别不友好（`sparse-proxy720p.mp4`：480 帧只有 8 个关键帧），与分辨率是两个独立原因。
### 6.6 播放与丢帧
播放**按序列帧率推进帧号**，不用 `<video>.play()`；跟不上时允许丢帧，但丢了多少会报出来（状态栏，播放按钮右侧），形如 `丢帧 38 帧：跳过 35 帧、卡顿丢弃约 3 帧（93ms）；实际画出 54 帧`。两类分开记：`skipped` 是时间推进一步跨了 n 帧、中间 n-1 帧没画（**确定值**）；`stalled` 是单拍耗时超过 250ms 上限、被砍掉的那段时间（**估算值**，按帧率折算）。
### 6.7 界面
打开页面就是**剪辑台**：左边舞台 + 传输条，下面是时间线，右边是检视器，最上面一条承担工程状态与导出。

- **零构建**：没有 npm、打包器、前端框架；`scripts/check-web-invariants.mjs` 盯着 `web/node_modules` 不许存在、`app.js` 里不许出现业务规则字符串。样式是**一张 `web/app.css`**，图标是**内联 SVG 表**。
- **三态分明**：载入中 / 空（真的没有）/ 读不到（出了事），「读不到」**不许显示成空**。
- **帧是契约单位**：时间码是给眼睛的，播放头永远按整数帧走。

舞台右上角的 **「真实出片帧」** 按钮：点一下，后端用 `dhampir frame` 渲染**当前这一帧**，取回盖在画布上并报出像素差；再点收回，播放头一动自动收回。判据见 6.4。键盘：`空格` 播放/暂停，`←/→` ±1 帧（`Shift` ±1 秒），`Home/End` 首尾，`S` 切开，`Delete` 删除，`Ctrl+Z` / `Ctrl+Shift+Z` 撤销/重做，`M` 静音，`,` / `.` 或 `J` / `L` 微调，`?` 打开快捷键表，`Esc` 关掉它。
### 6.8 本机后端单独起
```bash
node scripts/dhampir-local.mjs --port 8787 --asset-root target/s3
```

分工是 **HTTP 在 Node，渲染在 Rust**：脚本管路由、任务生命周期、把请求落成临时文件，渲染一律调 CLI；路由清单见 [api.md 第四节](api.md)。
## 六之二、我想把网页动画搬到工程里（WAAPI / CSS → 工程文件）
这条路是**转译**，不是"把网页录下来"：网页动画被读成数据，写进工程文件。
### 1. 转译一条
```bash
node scripts/waapi2doc.mjs fixtures/waapi-snapshot.sample.json target/waapi-demo/out.doc.json
target/debug/dhampir.exe probe --project target/waapi-demo/out.doc.json     # 载入校验：期望 0 错 0 警
```

快照形状照浏览器里的 `getKeyframes()` + `getTiming()` 裁剪（样本见 [`fixtures/waapi-snapshot.sample.json`](../fixtures/waapi-snapshot.sample.json)）；本仓**没有采集器**，采集留在宿主那一侧。转译器每条动画都给结论：搬了几条、量化成几个断点、**哪几条明确跳过以及为什么**（例：`sparkle` 用 alternate + back_out、`poly` 是 polygon、`wobble` 会改 filter 函数表）。
### 2. 边界在哪（三张表，都是机器判据）
| 我想知道 | 看哪 | 判据 |
|---|---|---|
| 每条能力支持到什么程度 | [`web/capabilities.json`](../web/capabilities.json) | `node scripts/check-capabilities.mjs` |
| 两端允许有哪些差异、量过没有 | [`scripts/dom-parity-differences.toml`](../scripts/dom-parity-differences.toml) | `node scripts/check-dom-differences.mjs` |
| 现在**还需要人做**什么 | `node scripts/web-parity-pending.mjs` | 清单**由台账生成**，不是手写 |
### 3. 在浏览器里看第二个宿主（DOM 预览）
```bash
node scripts/web-check.mjs --serve        # 起静态服务，打印 URL
# 打开 <URL>/dom-host.html：能吃同一份工程，用 CSS 画出来
```

能画的都画；**画不出来的会在页面上报出来**（哪一层、哪条能力、为什么）。
### 4. 别把老工程画变了
```bash
node scripts/check-old-project-baseline.mjs
```

拿**当次构建**与 `dist/` 里那份**已发布产物**渲染同一份工程、同一批帧，逐帧比 PNG 字节。全同才算过；缺工程/缺产物**退 2**（没验 ≠ 通过）。
### 5. 想自己量"预览与成片像不像"
```bash
node scripts/check-preview-path.mjs     # 预览链路（canvas）vs CLI 出图，按跨厂商那一档判
node scripts/measure-dom-parity.mjs     # DOM/CSS 与引擎之间那三条允许的差异（要浏览器）
```

两条都自己起 headless Chrome、自己收尾（不装 puppeteer），读数落在 `target/` 下；档位在 [`scripts/framediff-thresholds.toml`](../scripts/framediff-thresholds.toml) 与 [`scripts/dom-parity-differences.toml`](../scripts/dom-parity-differences.toml)。**缺浏览器 / 缺 wasm pkg 时它们退 2**。
## 七、特效与转场
特效是**「类型串 + 参数」的声明式数据**，不是代码：加特效只需在登记表里加一格。
### 7.1 已登记的特效（13 个 / 4 条管线）
| kind | 参数 | 范围 | 管线 | 说明 |
|---|---|---|---|---|
| `gaussian_blur` | `radius` | 0 – 16 | SeparableBlur | 上界与着色器抽头数钉在一起 |
| `brightness` | `amount` | -1 – 1 | ColorAdjust | 加性亮度偏移，**归一化色值**（不是百分比） |
| `contrast` | `amount` | 0 – 4 | ColorAdjust | 绕 0.5 中灰缩放，`1` = 不变 |
| `saturation` | `amount` | 0 – 4 | ColorAdjust | 向亮度插值，`1` = 不变，`0` = 完全灰度 |
| `hue` | `degrees` | -180 – 180 | ColorAdjust | 色相旋转，单位是**度**（不是弧度） |
| `flash` | `opacity` / `color` | — | ColorMask | 常量色叠加（加性/混合） |
| `vignette` | `strength` | — | ColorMask | 径向暗角 |
| `noise` | `amount` | — | ColorMask | 噪点 |
| `overlay` | `color` / `stops` | — | ColorMask | 覆盖层（纯色 / 渐变） |
| `shake` | `amount` / `frequency` / `seed` | — | Warp | 位移场抖动；`amount` 是**归一化位移比例**，乘回目标高才是像素 |
| `zoom_bounce` | — | — | Warp | 瞬时推近再回弹 |
| `pulse` | — | — | Warp | 瞬时脉冲 |
| `split` | — | — | Warp | 分裂位移 |

`ColorAdjust` 与 `ColorMask` **共用"逐像素"这一档**，可以同时挂；参数填恒等值（亮度 0 / 对比度 1 / 饱和度 1 / 色调 0）等价于不挂 —— **已实测出片逐字节相同**。登记表是这条链路的唯一真相，`scripts/check-effect-registry.mjs` 盯着"登记了的管线都真的实现了"。
### 7.2 执行顺序（三段，不是两段）
| 段 | 含哪些管线 | 做什么 |
|---|---|---|
| `PerPixel` = 0 | ColorAdjust + ColorMask | 逐像素色彩变换与常量色叠加 |
| `Warp` = 1 | Warp | **坐标重映射** —— 改变"哪个像素在哪" |
| `Neighborhood` = 2 | SeparableBlur | 邻域算子（模糊） |

顺序有实际影响：亮度是加性的，先调整再模糊与先模糊再调整**结果不同**；**模糊不能排在 Warp 之前**（位移场按目标坐标算，先糊会把边缘糊成一片）。顺序只写在渲染主路径**一处**（`PassStage::of` 与 `effect_passes`）。
### 7.3 有素材的图层**只执行 SeparableBlur**
> **挂了素材的图层（实拍片段、贴纸）只跑模糊；别的管线挂上去通过校验、进 JSON、被求值层算出来，然后整条丢掉 —— 画面一点不变。**

`shake` / `flash` / `color_shift` 这类要挂在**调整图层**上（没有素材、只有 `effects` 的那一层）。`validate_timeline_v2` 会报 `effect_would_be_ignored` 错误，判据是 `layer.source.is_some() && spec.pipeline != SeparableBlur`。
### 7.4 已登记的转场
转场只有 `cross_dissolve`（参数 `duration`，帧）：把前一个相邻片段淡出的同时把自己淡入。它挂在片段的 `transition_in` 上；校验拦三种情形：时长非正、时长超过片段本身、**前面没有紧邻片段**。**契约 v4 起转场类型是字符串而不是枚举** —— 加转场与加特效同样不必升版本；但渲染侧目前**只读 `duration`**、不认 `kind`，换了 `kind` 不改变画面，只是通过校验的类型串不同。
## 八、验证与守卫
### 8.1 跑全部守卫
```bash
node scripts/run-guards.mjs              # 跑全部 30 条（每条先自检、再正跑）
node scripts/run-guards.mjs --list       # 只列清单
node scripts/run-guards.mjs --self-test  # 只跑各自的 --self-test
```

**自检与正跑分开报**：自检证明守卫自己没坏，正跑证明仓库没坏；守卫报红时**先跑它的 `--self-test`**。
### 8.2 CI 跑哪几档
`static` / `records` 两档走 guard job，`cargo` 档走 check-native；`wasm-pkg` / `ffmpeg` / `cli` / `browser` 要本机资产，只在开发机上跑。档位写在 `scripts/run-guards.mjs` 的 `deps` 里，名单以 `--list` 为准；漏档由 `check-ci-guards` 判红。纪律：**不在空文件集上通过** —— 没有文件可查时退出码是 `2` 不是 `0`。
### 8.3 完整命令
```bash
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能带 --workspace —— dhampir-worker 是 native-only
cargo test --workspace
node scripts/run-wasm-tests.mjs
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

本机 DSH 沙箱里 `cargo test` 会因 `%TEMP%` 被拒（`建临时字体目录: PermissionDenied (os error 5)`）：把临时目录指进工作区，`New-Item -ItemType Directory -Force -Path target/tmp | Out-Null` 后设 `$env:TEMP = $env:TMP = (Resolve-Path target/tmp).Path`，再跑。
## 九、留取证记录
```bash
node scripts/record-acceptance.mjs --milestone m0   # 也支持 m1 / m2
```

按 `plan/` 里写的退出标准逐条跑，把**原始 stdout/stderr** 与每项的退出码落进 `records/<里程碑>/`，最后写 `acceptance.json`。**判定以退出码为准。**

```bash
# native：离屏探针图 + adapter 信息（--probe-only 则完全不碰 GPU）
cargo run -q -p dhampir-worker --bin dhampir-render -- --out records/m0
cargo run -q -p dhampir-worker --bin dhampir-render -- --probe-only --out records/m0
# 浏览器：本地静态服务 + 记录落盘口（打开打印的 URL，带 ?autorun=1&expect=<native PNG 的 fnv1a64>）
node scripts/serve-wasm-harness.mjs --port 8787
# 页面结论会 POST 回服务，写成 records/m0/browser-harness.json 与 probe-browser-webgpu.png
node scripts/capture-harness-screenshot.mjs      # 整页截图；无头下拿不到 WebGPU 时加 --headed
```
## 十、常见问题
### 10.1 wasm-bindgen 版本不一致
报 `it looks like the Rust project used to create this wasm file was linked against a different version of wasm-bindgen` 并拒绝生成胶水代码 —— 是版本没对齐，不是构建坏了。crate 版本由 `Cargo.toml` 的 `[workspace.dependencies]` 钉住；CLI 由 `wasm-pack` 下到自己的缓存里（**不在 PATH 上**），`scripts/run-wasm-tests.mjs` 会去缓存里找并**先比对版本再跑**（不一致直接退出 2）。
### 10.2 改了 Rust 之后守卫变红（陈旧 wasm pkg）
驱动检测到陈旧 pkg 会**自动重建一次**（`scripts/stale-pkg.mjs`），守卫**只判不修**；重建后仍红就是真的坏了，**不要手工改 pkg 让它变绿**。已知的坑：**失败的 wasm 构建会毒掉增量缓存**，此后每次重建都 ICE（`rmeta/encoder.rs:2457`），加 `CARGO_INCREMENTAL=0` 冷跑一次。
### 10.3 本机 node 的 UV_HANDLE_CLOSING 断言
Windows + Node 25.5.0 上 `wasm-bindgen-test-runner` 会在**测试全部通过之后**报 `UV_HANDLE_CLOSING`、退出码变成 `-1073740791`（与测试内容无关）。`scripts/wasm-test-node-exit-shim.cjs` 接管 `process.exit`、只设 `process.exitCode`，代价是"挂住"成为新风险，所以调用方设了超时。**不要改成"忽略退出码、只 grep test result: ok"**。
### 10.4 受限会话里起不了子进程
某些受限会话里给子进程开 **stdin 管道**会失败（`os error 231`）。不喂 stdin 的调用走 `scripts/spawn-tool.mjs`（`stdin: 'ignore'`）；渲染必须往编码器 stdin 写帧，那部分**修不掉**，而且**只应为这一个原因红**。**不要套 `--require` 垫片**。
### 10.5 守卫的清理被"安全删除"绊倒
本机 CLI 给 `fs.rmSync` 套了按 turn 计数的安全删除，超阈值抛 `SAFE_DELETE_BULK_CONFIRM_REQUIRED`。与判据无关的清理走 `scripts/safe-remove.mjs`（删不掉只警告、**不动退出码**）；清不掉时上一轮残留可能还在，所以**别拿"文件在不在"当判据**。
### 10.6 EADDRINUSE：端口被占
浏览器里报连不上、或后端起不来并报端口占用，通常是**已经有一个实例在跑**（`--serve` 的窗口没关）。先确认并结束它再重开 —— 两个实例抢同一个端口时，页面连上的是**先起的那个**。
## 十一、约定（这套仓库对"怎么算通过"的纪律）
1. **退出码优先**。判定以退出码为准，不靠读日志、不靠 grep `ok` 字样。
2. **不做软判据**。没有 `|| true`，没有 `continue-on-error`。
3. **自检与正跑分开报**（见 8.1）。
4. **不猜**。看不出来的东西直接报错，不选一个默认值糊过去。
5. **假红比没有守卫更坏**。触发条件做不到精确时，要么改措辞、要么写明"这条守卫不管什么"。
6. **量出来的数才写进文档**。性能结论必须给测量入口与原始数字。
