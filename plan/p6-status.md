# P6 状态：双端 + 单机的实际使用

> 范围前提（用户已定）：**排除 Linux 环境**，只在当前 Windows 本机完成实际功能。
> 目标：**双端（浏览器 + 后端渲染）与单机（本机后端）都能实际使用** ——
> 在浏览器里打开一份真实工程文件（含素材表）、编辑、把工程交给后端渲出 mp4。
>
> 起点 `528ac56`，收口 `abe7720`（P6 共 5 个提交）。
> 前四个阶段的记录在 `p1-p5-status.md`。

## 一句话

**它把一个"能证明一致性的底座"推到了"能真的剪、真的出片"。**
上一阶段的判据是"两条管道出的形状一致"；这一阶段的判据是
**在浏览器里打开工程、看到画面、点导出、拿到 mp4**，而这件事两头都成立了。

## 六项都没跳

| # | 阶段 | 交付物 | 判据 |
|---|---|---|---|
| **F1** | 后端真渲染器 | `crates/dhampir-worker/src/pipeline.rs` | 多源顺序解码 + 求值 + 合成 + 读回 + 编码；90 帧工程出 640x360 / 30fps / 3.000000 秒 |
| **F2** | 底座 CLI | `crates/dhampir-worker/src/bin/dhampir.rs` | 五个子命令 probe / info / gop / frame / render；退出码 0/2/1 |
| **F3** | 本机后端真出片 | `scripts/dhampir-local.mjs` + `scripts/check-local-backend.mjs` | 20 条端到端判据全过（校验 / 素材 info+gop / Range / 出片 / 下载 / 取消） |
| **F4** | 浏览器认工程文件 | `crates/dhampir-wasm/src/timeline_host.rs` + `web/` | 三种契约形态都收；素材走资产表；导出按能力声明分路 |
| **F5** | 观测与驱动 | `scripts/web-check.mjs` | CDP 读页面脚印；--local / --remote 走产品路径 |
| **F6** | 收口 | 本文 + `plan/measurements.md` | A1–A9 逐条真跑 |

## 最有分量的三条证据

### 1. 单机模式端到端可用（这条是本轮的核心目标）

    node scripts/web-check.mjs --local
    -> 90 帧 / 640x360 / 30fps / 3.000000 秒 / pngFramesReceived: 0   EXIT=0

`pngFramesReceived: 0` 是关键：**走的是"提交工程 -> 后端出片 -> 下载"这条产品路径**，
不是"浏览器逐帧渲染成 PNG 交给驱动"。只看到"成功"是分不清这两条路的。

### 2. 分离模式的代码路径也通了

    node scripts/web-check.mjs --remote
    -> 同上（后端在不同端口 —— 因此确实是跨源，URL 由宿主给）

**边界（写进输出，不声称验过）**：后端仍在 127.0.0.1 上。这条验的是**代码路径**，
不是真实远端部署 —— 本机没有第二台机器。

### 3. 用户按下导出要等多久，现在有数了

| 用例 | 每帧 ms（中位） | 端到端 |
|---|---|---|
| 640x360 / 90 帧 | 14.56 | 1310 ms |
| 1920x1080 / 30 帧 | 22.50 | 675 ms |

**3 秒成片在本地约 1.3 秒出完。** 量纲限制见 `plan/measurements.md` 第五节。

## 收口基线

| 项 | 值 |
|---|---|
| 全仓测试 | **329 passed / 10 ignored / 0 failed** |
| 守卫 | **8 个，全 EXIT=0** |
| 带 --self-test 的脚本 | 5 个（含两个新增），全 EXIT=0 |
| 警告 | **0 条**（口径：`cargo check --workspace --all-targets`） |
| 降级模式 | `node scripts/web-check.mjs` -> EXIT=0 |
| 本机模式 | `node scripts/web-check.mjs --local` -> EXIT=0 |
| 分离模式代码路径 | `node scripts/web-check.mjs --remote` -> EXIT=0 |
| 本机后端端到端 | `node scripts/check-local-backend.mjs` -> 20/20 |
| 双端一致性 | `node scripts/check-dual-end.mjs` -> 4/4 SSIM=1.000000 |

## 这轮踩到的坑（写给下一个人）

### 1. 一句"注释里写着但实现没做"的话

`dhampir_project_open` 在工程校验不过时把已载入的工程**清空**了，
而 `engine.js` 的注释一直写着"失败时保留上一份可用工程"。
后果：编辑到一半（工程暂时非法）会让预览直接不再出图。
**注释与实现不一致时，两边都可能"看起来是对的"。**

### 2. 一个"本地 curl 通了"不等于"浏览器能用"的坑

`POST /export` 带 `application/json` 会触发跨源**预检**。
后端不回答 OPTIONS，浏览器就报 "Failed to fetch"，而 curl 打同一个地址是通的。
旧记录写着"HTTP 层可用，逐条 curl 验过"——那句话是真的，
但它**推不出浏览器能用**。

### 3. 一个只有跨源才会出现的坑

不带 `crossorigin` 的 video 是"被污染的"，WebGPU 的
`copyExternalImageToTexture` 拒绝它。表现不是报错，而是 wasm 里一个
`unreachable`（wgpu 校验失败变 panic），页面上只有"启动失败：unreachable"。
同源模式下永远看不到这个问题。

### 4. 一个"推断错了位置"的坑

上一阶段把 `--local` 的卡点收窄到"endFrame 之后、三个面板之前"。
**那三个是同步函数，早就跑完了** —— 真正的原因是那句
`mark("三个面板已渲染")` 被写进了 `renderInspector` 的 apply 闭包里，
于是那条 beacon 在启动时永远不发。所谓"卡住"是**没有脚印**。
错的卡点推断比没有推断更坏：它把人送到错的地方去查。详见 `local-mode-status.md`。

### 5. 一个"实例字段盖住原型方法"的坑

`this.doc = null` 与类方法 `doc()` 同名 ——**实例字段会盖住原型上的方法**，
于是 `engine.doc()` 变成 "engine.doc is not a function"。
字段改名 `projectFile` 解决。

### 6. 两个"错得很安静"的数据问题

- ffprobe 的 JSON 字段类型是混的（pos/size 是字符串，dts/duration 是数字）。
  只认字符串时 width/height/duration 会被静默当成 0 —— CLI 的 info 一开始就报 0x0。
- 真实 MP4 的首包 dts 可以是负的（这份素材是 -512）。契约里 dts 是 u64：
  报错则 info/gop 在真实素材上从来跑不通；夹成 0 则多个包撞同一时间戳。
  改为减去最小 dts，并把 `dts_origin` 一并报出来。

## 未做清单（P6 收口时如实列出）

| # | 未做 | 说明 |
|---|---|---|
| 1 | **真实远端部署** | `--remote` 只验了代码路径（跨源 + 宿主给 URL）。本机没有第二台机器 |
| 2 | **音频** | 出片**不渲染音频**。render 会把它列在 stderr 上，不给"看起来很成功"的哑片 |
| 3 | **含解码的逐像素双端比对** | 两端解码路径不同（浏览器 WebCodecs / 后端 FFmpeg），当前架构下不可测 |
| 4 | **per-mode 混合的真机像素测试** | 只证明了"方程表与 is_implemented 一致"，没证明 multiply 画出来是对的 |
| 5 | **M1 四种环境记录（2/4）** | 需要 Linux —— **用户已明确排除** |
| 6 | **四份不同素材的出片重测** | 样本工程四个 asset 指向同一文件，吞吐数是**下界** |
| 7 | **真实尺寸下的并发重测** | 有单路真实尺寸数了，没有并发的 |
| 8 | **素材导入** | 编辑器能改工程，但不能把一份新素材导入成 asset（那要一个上传/登记流程） |
| 9 | **撤销/重做、拖拽** | 属性面板是数字输入；时间线上还不能拖动 |
| 10 | **色彩矩阵与浏览器的一致性** | 后端已显式声明；浏览器那侧由 WebCodecs 决定，本仓库管不到 |

## 明确划界（不是遗漏）

原生解码器、编码 Rust 绑定、上传管线 / 对象存储 / 账号、任务队列持久化、
ping-pong 类混合模式、嵌套序列 —— 均见 `p1-p5-status.md` 与
`remaining-work.md` 各自的理由；本轮**没有放宽任何一条**。

`records/` 未改动一个字节。
