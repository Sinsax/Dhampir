# 本机模式（--local）：收敛结论与证据

> 这份文件原先记的是「**本机模式的浏览器端到端不可用**」以及一个卡点推断。
> **现在它可用了**，而原来那个推断是**错的** —— 那个卡点根本不存在，
> 它是「观测缺失」造出来的假象。下面按时间顺序写清楚，免得下一个人
> 从错误的结论出发。

## 现状

| 面 | 状态 | 怎么自证 |
|---|---|---|
| 降级模式（前端 + 同源静态资源） | ✅ 可用 | `node scripts/web-check.mjs` |
| 本机后端的 HTTP 层 | ✅ 可用 | `node scripts/check-local-backend.mjs`（20 条判据） |
| 本机后端的素材与工程解析（info / gop） | ✅ **已接** | 同上；调 dhampir CLI，不再 501 |
| 本机模式的**浏览器端到端** | ✅ **可用** | `node scripts/web-check.mjs --local` |
| 分离模式的**代码路径** | ✅ 可用（跨源 + 宿主给 URL） | `node scripts/web-check.mjs --remote` |

实测产物：90 帧 / 640x360 / 30fps / **3.000000 秒**，且 `pngFramesReceived: 0` ——
走的是「提交工程给后端出片」这条**产品路径**，不是 PNG 序列。

## 原来的卡点推断是错的

旧的结论写着：

    卡在 endFrame() 返回之后、renderTimeline/renderInspector/renderIssues 之前。

真实情况是：**那三个都是同步 DOM 函数，它们早就跑完了**，
而它们后面那句 `mark("三个面板已渲染")` 被写进了 **renderInspector 内部的 apply 闭包**里 ——
于是那条 beacon 在启动时**永远不会发出**。

「停在两个函数之间」不是页面停住了，是**我在那里没有脚印**。
当时写下的一句怀疑（「更可能是观测本身不可靠」）方向是对的，
但推断出的位置是错的 —— 而错的推断会把后来的人送到错误的地方去查。

## 真正的三个原因（都修了）

### 1. 跨源 video 没有声明 crossorigin —— WebGPU 拒绝被污染的 video

本机模式下素材来自 `http://127.0.0.1:<后端端口>/assets/a.mp4/media`，
而页面在另一个端口上 —— **它们跨源**。

不带 `crossorigin` 的 video 是「被污染的」，WebGPU 的
`copyExternalImageToTexture` **拒绝它**。表现不是报错，而是 wasm 里一个
`unreachable`（wgpu 的校验失败变成了 panic），页面上只有「启动失败：unreachable」。

修法：`video.crossOrigin = "anonymous"`（后端本来就给了 `access-control-allow-origin: *`）。

### 2. 后端不回答 CORS 预检 —— 浏览器说 "Failed to fetch"，而 curl 是通的

`POST /export` 带 `content-type: application/json`，属于**非简单请求**，
浏览器会先发一个 **OPTIONS 预检**。后端没回答它，于是页面拿到 "Failed to fetch"。

**这条值得单独记**：旧的记录写着「HTTP 层可用，逐条 curl 验过」——
那句话是真的，但它**不能推出浏览器能用**。curl 不做预检。
同一份接口，浏览器与 curl 走的是两套规则。

### 3. 观测本身不可靠（见上一节）

## 观测现在不依赖页面主动上报

`scripts/web-check.mjs` 起了 Chrome 的调试端口，超时/结束时直接
`Runtime.evaluate` 读页面里的 `window.__dhampirMarks`，打成一行「页面卡点」：

    页面卡点: [ready] main 进入 -> wasm 已加载 -> ... -> 启动完成

**为什么必须这样**：beacon 是页面主动发的，页面若在某个点之后不再有网络活动，
外面就只能看到「什么都没发生」。而「什么都没发生」是不可诊断的状态 ——
只能靠猜，而猜出来的位置可能离真相很远（这次就是）。
卡住比失败难查，所以观测要先做对。

另外：超时时把浏览器的**全量** stderr 落盘到 `target/p6/web-check-browser-stderr.txt`，
只打尾部会把 panic 的头几行（真正的原因）截掉 —— 这次就截掉了。

## 已经排除的原因（都不是原因，别重查）

| 假设 | 怎么排除的 |
|---|---|
| 驱动自己不退出 | 真原因之一（更早的一轮）。已修：跑完显式 kill + unref |
| 端口被占（EADDRINUSE） | 真原因之一（残留进程）。已清，并加就绪等待 |
| 后端没就绪就开页面 | 已加 /health 等待 |
| 页面抛 JS 错 | window.onerror + unhandledrejection 出口，回报「无」 |
| 跨源被 CORS 拒 | **部分是真原因**：预检没答（见原因 2）。但那时只加了 allow-origin，不够 |
| 素材路由不支持 Range | 真缺陷，已修（206 + Content-Range）；不是本卡点的原因 |
| seek 无限等 | 排除：seekVideo 本来就有 3000ms 兜底 |
| 三个面板渲染函数卡住 | **排除：它们是同步函数，不会挂起**。而且真实失败点比它们晚（首帧 draw） |

## 规律

1. **卡住比失败难查**：先建观测，再动代码。
2. **本地 curl 通了不等于浏览器能用**：跨源、预检、被污染的媒体，都是浏览器独有的规则。
3. **一个错的卡点推断比没有推断更坏**：它会让人去错的地方查。
   写下推断时，要把「哪些是观测、哪些是猜测」分开标。
