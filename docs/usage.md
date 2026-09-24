# dhampir 使用说明

这份文件回答一个问题：**怎么用它**。
它不解释为什么这样设计（那在 [../plan/video-editor-plan.md](../plan/video-editor-plan.md) 与
[../plan/video-editor-tech-guide.md](../plan/video-editor-tech-guide.md)），
也不讲当前做到哪了（那在 [../README.md](../README.md) 与 [../plan/next-steps.md](../plan/next-steps.md)）。

---

## 一、先决条件

| 需要 | 用途 | 没有它会怎样 |
|---|---|---|
| **Rust 1.97.0** | 全部构建 | 由 `rust-toolchain.toml` 自动切换，装好 rustup 即可 |
| **ffmpeg / ffprobe** | 素材探测、字幕栅格化、出片编码 | `info` / `gop` / `render` 直接失败 |
| **GPU（Vulkan/DX12/Metal）** | `frame` / `render` 的合成 | `probe` 能跑，出图不能 |
| **字体文件** | 烧进画面的字幕 | 工程里有字幕轨时 `frame` / `render` **判失败**（问题码 `subtitle_font_missing`） |
| Chrome / Edge | 浏览器预览 | 只有预览与双端比对用得到 |

**本仓不内嵌字体、也不猜系统字体**——要烧字就自己给 `--font-file`。
这是刻意的：静默出一份没有字幕的片子比报错难查得多。

---

## 二、最快的一次「跑通」

不需要工程文件，先确认工具链是活的：

```bash
# 1. 双编译
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能用 --workspace —— dhampir-worker 是 native-only

# 2. 测试
cargo test --workspace                     # native 全量，约 547 条
node scripts/run-wasm-tests.mjs            # wasm32 运行时，15 条

# 3. 命令行入口活着吗
cargo run -q -p dhampir-worker --bin dhampir -- --help
```

想看一个真工程怎么走，见下一节。

---

## 三、命令行（CLI）

二进制名 `dhampir`。**退出码固定三种**：`0` 成功 / `2` 用法或校验错 / `1` 运行期失败。
`--help` 是真相来源，本文只是导览——**子命令的帮助永远比这份文档新**。

### 3.1 读工程

```bash
# 解析并校验，打印 DocIssues（不渲染、不要 GPU）
dhampir probe --project project.json

# 素材信息：尺寸 / 帧数 / 时间基 / GOP 长度
dhampir info --asset clip.mp4

# GOP 切片表（顺序解码的定位依据）
dhampir gop --asset clip.mp4

# 素材库：每个资产被引用了几次
dhampir library --project project.json
```

### 3.2 出图与出片

```bash
# 单帧 PNG
dhampir frame --project project.json --frame 42 --out ./out

# 出片：stdout 是 NDJSON 进度流，不是给人读的日志
dhampir render --project project.json --from 0 --to 89 --out out.mp4
```

出片时两条可选的路，**互相独立，可以只要一条**：

| 开关 | 做什么 | 要什么 |
|---|---|---|
| `--font-file <ttf/ttc/otf>` | 把字幕**烧进画面** | 字体 + GPU。工程里有字幕轨时**少给就判失败** |
| `--subtitle-out <文件>` | 字幕另存**侧挂文件** | 什么都不要。**只有 render 认**这个开关 |
| `--no-audio` | 出片**不要声音** | — |

关于侧挂文件的两条边界：

- 它的时间是**相对这一趟产物**从 0 起算的毫秒，不是源里的绝对时间；
- 内容只有文本与时间——**源里的加粗/斜体/颜色不进侧挂**；
- `--format` 不给就看扩展名（`.ass`/`.ssa` → ASS，其余 → SRT）；
  **看不出来不猜**，直接报错。

`--no-audio` 存在的原因是它**与引入音频之前逐字节相同**——
排查声音问题时这是该拿来对照的那一份。

### 3.3 改工程

形状是一个带 `op` 字段的 JSON 对象，**六个操作**：

| op | 形状 |
|---|---|
| `insert` | `{"op":"insert", ...}` |
| `trim` | `{"op":"trim", ...}` |
| `split` | `{"op":"split", "layer":"c", "at":75}` |
| `move` | `{"op":"move", ...}` |
| `remove` | `{"op":"remove", ...}` |
| `set_sequence` | `{"op":"set_sequence", ...}` |

> **不给 `--write` 就是干跑**：只在内存里做一遍并打印结果，一个字节都不落盘。
> 先干跑看清楚，再决定写不写——这是默认值，别把它当成"没生效"。

```bash
dhampir edit --project project.json --op '{"op":"split","layer":"c","at":75}'
dhampir edit --project project.json --op '...' --write
```

### 3.4 撤销 / 重做

```bash
dhampir edit --project p.json --undo --history h.json --write
dhampir edit --project p.json --redo --history h.json --write
```

**历史存哪必须由你说**（不替你往工程旁边写文件）。历史文件不在 = 从空历史开始，**不是错误**。
没有可撤销的步骤时退出码 `2` 并明说——静默什么都不做更难查。

### 3.5 具名子命令：同一实现的糖

`clip` / `sequence` / `undo` / `redo` / `batch` 与 `edit` **走同一个 `apply`、同一条落盘路径**。
它们不是另一套实现，所以行为不可能有第二份——这一点由 `check-cli` 钉在**产物字节 + stdout** 上，
不是"看起来一样"。

```bash
dhampir clip insert --project p.json --track v1 --asset a1 --at 0 --source-in 0 --length 30
dhampir clip trim   --project p.json --layer c --edge in --to 10
dhampir clip split  --project p.json --layer c --at 75
dhampir clip move   --project p.json --layer c --to 20 --track v2
dhampir clip remove --project p.json --layer c --ripple

dhampir sequence set --project p.json --timebase 30000/1001 --width 1920 --height 1080 --write
dhampir undo --project p.json --history h.json --write
dhampir redo --project p.json --history h.json --write
```

**每个动作「要哪些开关、认哪些开关」都在参数这一关判死——多给一个也报错。**
多给比少给更隐蔽：那个开关会被静静丢掉，而用户以为它生效了。

改帧率会**按时间重算所有序列帧号**；不给 `--timebase` / `--width` / `--height` 的那一项就不动它。

### 3.6 批处理

```bash
dhampir batch --project p.json --script ops.txt --history h.json --write
```

脚本是**一行一个 op 的 JSON**（空行与 `#` 开头是注释）。

- **一次写、一条历史**——不是一个"循环调 N 次"的宏；
- 中途有一步不成立就**整份不落盘**，并明说**卡在第几行**；
- **空脚本不算成功**（它会打印 ok 却什么都没做，与"脚本路径写错了"分不开）。

### 3.7 公共选项

| 选项 | 作用 |
|---|---|
| `--asset-root <目录>` | 工程里 `asset.uri` 的相对根（默认 `target/s3`） |
| `--asset-map <文件>` | 兜底资产表（`assets.<id>.file`）。**只补工程没登记的 id**，工程里的位置永远优先 |
| `--width` / `--height` | 输出尺寸（默认取 `render_hints`） |
| `--font-file` | 画字幕的字体（见 3.2） |

---

## 四、浏览器预览

**这里没有打包器**（不用 Vite / webpack / pnpm）。`wasm-pack --target web` 的产物本身就是 ES module，
浏览器能直接 `import`。少一层构建就少一层"改了没生效"。

### 4.1 只起服务，人工看

```bash
node scripts/web-check.mjs --serve
```

### 4.2 程序化验收（四种跑法）

```bash
node scripts/web-check.mjs --probe       # W0：工程帧能不能上 canvas
node scripts/web-check.mjs               # app 验收（**降级模式**）：逐帧导出 -> FFmpeg 编码
node scripts/web-check.mjs --local       # 本机模式：走**产品导出路径**（提交 -> 轮询 -> 下载）
node scripts/web-check.mjs --remote      # 分离模式的**代码路径**：后端在别处，URL 由宿主给
node scripts/web-check.mjs --synthetic N # 合成源导出（双端比对用）
```

另有 `--frames-only`、`--canvas WxH`、`--timeout-ms N`、`--ready-only`（只验页面启动完成）。

**为什么 `--local` 与默认跑法要分开**：默认那条验的是"浏览器渲染的**帧**对不对"；
`--local` 验的是"浏览器把工程交给后端、后端出片、能下载"。两条路终点都是 mp4，
但中间完全不是一回事——混在一起，失败时只说得出"失败了"。

### 4.3 本机后端

```bash
node scripts/dhampir-local.mjs --port 8787
# 打印：本机后端：http://127.0.0.1:8787
```

分工是 **HTTP 在 Node，渲染在 Rust**：这个脚本只管路由、任务生命周期、
把请求落成临时文件、把 Rust 的输出翻成 HTTP。渲染一律调 CLI。

---

## 五、验证与守卫

### 5.1 跑全部守卫

```bash
node scripts/run-guards.mjs              # 跑全部 19 条
node scripts/run-guards.mjs --list       # 只列清单
node scripts/run-guards.mjs --self-test  # 只跑各自的 --self-test
```

**一共 19 条**，每条都带自己的 `--self-test`，且每条都被**反向验证**过
（临时植入违例，确认它真的会红）。**守卫若不会红，就不是守卫。**

### 5.2 CI 里只跑 3 条

```bash
node scripts/check-core-purity.mjs    # core 里没有 #[cfg] / cfg!
node scripts/check-dep-graph.mjs      # 依赖方向单向无环
node scripts/check-text-hygiene.mjs   # 全仓 LF + 无 BOM + 合法 UTF-8
```

另外 16 条要 ffmpeg / GPU / 真浏览器 / `records/` 里的取证存档，只在开发机上跑。
**这不是"少验一点"**——放进 CI 只会让"环境没装好"长得像"代码回归"。

三条的共同纪律：**不在空文件集上通过**。没有文件可查时退出码是 `2` 不是 `0`——
"没扫到"和"扫过了没问题"是两件事。

守卫若报红，先跑它的 `--self-test`：自检失败说明**守卫自己坏了**，
先修守卫，别信它的结论。

---

## 六、特效与转场（可用清单）

特效是**「类型串 + 参数」的声明式数据**，不是代码。加特效只需要在登记表里加一格；
渲染器按 `pipeline` 派发，不需要改渲染主路径。

### 6.1 已登记的特效

| kind | 参数 | 范围 | 管线 | 说明 |
|---|---|---|---|---|
| `gaussian_blur` | `radius` | 0 – 16 | SeparableBlur | 可分离高斯模糊。上界与着色器展开的抽头数钉在一起，改一边必须改另一边 |
| `brightness` | `amount` | -1 – 1 | ColorAdjust | 加性亮度偏移，**归一化色值**（不是百分比） |
| `contrast` | `amount` | 0 – 4 | ColorAdjust | 绕 0.5 中灰缩放，`1` = 不变 |
| `saturation` | `amount` | 0 – 4 | ColorAdjust | 向亮度插值，`1` = 不变，`0` = 完全灰度 |
| `hue` | `degrees` | -180 – 180 | ColorAdjust | 色相旋转，单位是**度**（不是弧度） |

四个色彩特效**共用一条管线**，可以同时挂；同类叠加是可交换的（亮度相加、其余相乘）。
参数填恒等值（亮度 0 / 对比度 1 / 饱和度 1 / 色调 0）等价于不挂 —— 已实测出片逐字节相同。

### 6.2 执行顺序

调整图层（没有素材、只有 `effects` 的那一层）内部：**先色彩调整，后模糊**。

顺序有实际影响：亮度是加性的，先调整再模糊与先模糊再调整**结果不同**。
定成先调整是因为它更符合直觉 —— 先决定这张图长什么样，再去糊它。
这个顺序必须两端一致，所以它只写在渲染主路径里**一处**。

### 6.3 已登记的转场

| kind | 参数 | 说明 |
|---|---|---|
| `cross_dissolve` | `duration`（帧） | 交叉溶解：把前一个相邻片段淡出的同时把自己淡入 |

转场挂在片段的 `transition_in` 上。校验会拦三种情形：时长非正、时长超过片段本身、
以及**前面没有紧邻片段**（那样「淡出」的那一头不存在，画面会凭空从黑里淡进来）。

**契约版本 v4 起**，转场类型是字符串而不是枚举 —— 加转场与加特效同样不必升版本。
但要注意：渲染侧目前**只读 `duration`**，不认 `kind`。也就是说换了 `kind` 不会改变画面，
只是通过校验的类型串不同。真要有第二种转场，需要在渲染侧认出它。

---

## 七、留取证记录

```bash
node scripts/record-acceptance.mjs --milestone m0   # 也支持 m1 / m2
```

按 `plan/` 里写的退出标准逐条跑，把**原始 stdout/stderr** 与每项的退出码落进 `records/<里程碑>/`，
最后写 `acceptance.json`。**判定以退出码为准，不靠读日志下结论。**

两个宿主各自的产物：

```bash
# native：离屏探针图 + adapter 信息（--probe-only 则完全不碰 GPU）
cargo run -q -p dhampir-worker --bin dhampir-render -- --out records/m0
cargo run -q -p dhampir-worker --bin dhampir-render -- --probe-only --out records/m0

# 浏览器：本地静态服务 + 记录落盘口
node scripts/serve-wasm-harness.mjs --port 8787
#   打开打印出来的 URL（带 ?autorun=1&expect=<native PNG 的 fnv1a64>）
#   页面结论会 POST 回服务，写成 records/m0/browser-harness.json 与 probe-browser-webgpu.png

# 浏览器那一份的整页截图（自己起服务、起无头 Chrome、等页面跑完再拍）
node scripts/capture-harness-screenshot.mjs
#   无头下拿不到 WebGPU 时加 --headed
```

---

## 八、常见问题

### 8.1 `wasm-bindgen` 版本不一致（第一天就红的头号来源）

报 `it looks like the Rust project used to create this wasm file was linked against a
different version of wasm-bindgen` 并拒绝生成胶水代码。**看着像"构建坏了"，其实是版本没对齐。**

- crate 版本由 `Cargo.toml` 的 `[workspace.dependencies]` 钉住；
- CLI 由 `wasm-pack` 下到自己的缓存里，**不在 PATH 上**；
  `scripts/run-wasm-tests.mjs` 会去缓存里找，并**先比对版本再跑**（不一致直接退出 2，不去猜）。

**换机器/换 CI 时若报这句话，先看这两个版本，别先怀疑代码。**

### 7.2 改了 Rust 之后守卫变红（陈旧 wasm pkg）

驱动检测到陈旧 pkg 会**自动重建一次**（`scripts/stale-pkg.mjs`），守卫**只判不修**。
若重建后仍红，那是真的坏了——**不要手工去改 pkg 让它变绿**。

一个已知的坑：**失败的 wasm 构建会毒掉增量缓存**，此后每次重建都 ICE（`rmeta/encoder.rs:2457`）。
遇到时加 `CARGO_INCREMENTAL=0` 冷跑一次。

### 7.3 本机 node 的 `UV_HANDLE_CLOSING` 断言

Windows + Node v25.5.0 上，`wasm-bindgen-test-runner` 会在**测试全部通过之后**
报 `UV_HANDLE_CLOSING` 并把退出码变成 `-1073740791`——与测试内容无关。
`scripts/wasm-test-node-exit-shim.cjs` 接管 `process.exit`、只设 `process.exitCode`，
让 node 自然退出；代价是"挂住"成为新风险，所以调用方设了超时。

**不要改成"忽略退出码、只 grep `test result: ok`"**——那是把守卫改成永远绿。

### 7.4 agent 会话里起不了子进程

某些受限会话里给子进程开 **stdin 管道**会失败（`os error 231`）。
不喂 stdin 的调用走 `scripts/spawn-tool.mjs`（`stdin: 'ignore'`）——这是**如实声明这次调用不喂 stdin**，
不是垫片。渲染必须往编码器 stdin 写帧，那部分**修不掉**，而且**只应为这一个原因红**。

**不要套 `--require` 垫片**：它会让"真的起不了子进程"也变绿，**比假红更坏**。

### 7.5 守卫的清理被"安全删除"绊倒

本机 CLI 给 `fs.rmSync` 套了按 turn 计数的安全删除，超阈值直接抛
`SAFE_DELETE_BULK_CONFIRM_REQUIRED`。跑一遍套件要删几百个临时路径。
与判据无关的清理走 `scripts/safe-remove.mjs`（删不掉只警告、**不动退出码**）。

边界：清不掉时上一轮残留可能还在，所以**别拿"文件在不在"当判据**，
要判就判内容/摘要或"这一轮刚写出来的字节"。

---

## 九、退出码与判据的约定

这套仓库对"怎么算通过"有一套固定纪律，用它的时候会一直遇到：

1. **退出码优先**。判定以退出码为准，不靠读日志、不靠 grep `ok` 字样。
2. **不做软判据**。没有 `|| true`，没有 `continue-on-error`。
   "守卫永远绿"是这套东西最危险的失效模式。
3. **自检与正跑分开报**。自检证明守卫自己没坏，正跑证明仓库没坏——
   两件事分开，才知道是哪一边出了问题。
4. **不猜**。看不出来的东西（比如侧挂格式）直接报错，不选一个默认值糊过去。
5. **假红比没有守卫更坏**。守卫的触发条件做不到精确时，要么改措辞要么在
   "这条守卫不管什么"一节里写明——**不默默放过**。
