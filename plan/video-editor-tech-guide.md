# dhampir · 视频编辑器 + 渲染器技术指导

> **目标产物**：一个**视频剪辑软件**——网页剪辑，服务器渲染导出
> 引擎名：**`dhampir`**（半吸血鬼）｜ 底座：**Rust + wgpu 双运行时**
> 调研日期：2026-09-22 ｜ 文中时效性数据以此日期为准

---

## 命名与项目定义（接手前先读）

这一节写给接手的人 / agent：名字怎么来的、边界在哪、代码里该用什么标识符。技术内容从 §0 开始。

### 为什么是 dhampir

**dhampir**（半吸血鬼）——巴尔干民俗中吸血鬼与人类所生之子：能在日光下行走，血统里同时流着两边的血。

这个语义直接映射本项目的架构支点：**同一份 `dhampir-core` 编译到两个运行时**（`wasm32-unknown-unknown` 与 native x86_64）。双重血统 = 双编译目标；白天（浏览器预览）与黑夜（服务端导出）都活动，靠的是同一套基因。

名字与目标的关系只有一条：**双运行时**（浏览器一侧、服务器一侧）对应「双重血统」——
是架构支点，不是人设。名字本身不影响任何技术决策。

### crate 家族命名

```
dhampir-timeline   时间线数据模型 + serde 契约 + 校验     纯数据，零 GPU 依赖
dhampir-media      解码 / 编码抽象 trait                 纯契约，两端各自实现
dhampir-core       渲染图 + WGSL + 特效注册表            零 #[cfg]，最贵资产
dhampir-wasm       浏览器宿主：wasm-bindgen + WebCodecs   仅 wasm32
dhampir-worker     服务端 headless worker                仅 native
```

依赖方向单向、无环：

```
dhampir-timeline   无内部依赖
dhampir-media      依赖 timeline   ← VideoInfo 的时间基必须与时间轴同源
dhampir-core       依赖 timeline   ← **不依赖 media**：渲染图不该知道 MP4 长什么样
dhampir-wasm       依赖 core + media
dhampir-worker     依赖 core + media

实测输出与守卫口径见 [`foundation-architecture.md`](./foundation-architecture.md) §3。
```

`dhampir-wasm` 与 `dhampir-worker` **互不依赖**——它们是同一份 core 的两个宿主，这正是"双血统"的字面含义。

引擎命名空间只有 `dhampir` 一个前缀。应用层若要另起名字，属于使用方的事，不在本仓库范围。

### 项目背景

起因是做**切片精修**（录像的片段裁剪 + 精修输出）——目标产物就是一个能完成这件事的**视频剪辑软件**。
功能范围按这个目标收：场景需求直接决定特效集与默认参数的取舍，不做「通用引擎」式的无限泛化。

> **命名范围**：无待办——只做本地处理，不发布包、不建远端仓库、不查域名。

---

## 0. 一页摘要

| 层 | 选定 | 关键理由 |
|---|---|---|
| **渲染（唯一核心资产）** | `wgpu`，双编译（wasm + native） | 同一份 Rust 代码与 WGSL 同时服务浏览器预览和服务端导出 |
| 浏览器渲染 API | WebGPU（2026-01 已进 Baseline） | 唯一能与服务端 wgpu 共享 shader 的浏览器图形 API |
| 浏览器解码/编码 | WebCodecs，只解 H.264 proxy | 硬解、零拷贝进 GPU 纹理；不做多格式兼容 |
| 服务端解码/编码 | FFmpeg（libav 绑定） | 媒体 I/O 必须复用 C 生态，不自行实现 |
| 服务端音频 | FFmpeg filtergraph | 音频不值得重造轮子 |
| 前端 UI | React + TypeScript | 浏览器侧生态压倒性优势；Rust UI 框架在此无位置 |
| 时间线模型 | 自研（对齐 OTIO 概念） | 前后端契约、帧精确的基础 |
| 素材策略 | 服务端统一转 proxy | 让浏览器侧解码路径极简 |
| 一致性 | SSIM 逐帧比对 + 影子环境 | 跨 GPU/驱动/编译器的不确定性必须可验证 |

**唯一不可替代的技术决策**：服务端也用 wgpu 渲染（而非 FFmpeg filtergraph）。这决定了特效是"写一遍"还是"写两遍并长期互相追赶"。

**本仓库的交付边界**：上表是整个**产品形态**的选型调研。本仓库只交付其中**渲染与预览的共用底座**
（`dhampir-core` + 两个宿主 + 时间线契约）；服务端媒体 I/O、前端 UI、素材管线、部署与发布**属下游工程**。

---

## 1. 形态判定

### 1.1 三种形态对比

| | A 纯前端 | B 纯服务端 | C 混合（**选定**） |
|---|---|---|---|
| 形态 | 浏览器里剪，素材不离开本机 | 提交时间线 JSON，服务端出片 | 浏览器实时预览 + 服务器导出 |
| 编辑体验 | 完整 | 无（模板 / API 化） | 完整 |
| 导出速度 | 慢一个数量级 | 秒级 | 秒级 |
| 导出成本 | 用户 CPU | 你的 GPU | 你的 GPU |
| 素材隐私 | 最好 | 需上传 | 需上传 |
| 参考 | MASterSelects | 云转码服务 | 腾讯云剪辑 |

### 1.2 为什么排除 A 和 B

**A 的硬伤在导出**：FFmpeg WASM 即便开 SIMD，1080p MOV 转码也只有 **0.3x 实时**。靠起 10 个 worker、取元信息后分布式转码再转封装、音频单独取出，上限也只有 **3–4x**（1 分钟视频十几秒）。纯前端的导出优化天花板是"可接受"，永远到不了"快"——这决定了它做不了专业场景。

**B 的硬伤是没有编辑器**：它本质是模板化 / API 化出片，不是编辑工具。

**C 是专业编辑器的实际答案**：腾讯云剪辑走的就是这条路，前端实时预览、服务端渲染导出。

---

## 2. 总体架构

```
┌─ 浏览器（编辑端）───────────────────────────────┐
│  React + TS         时间线 / 属性面板 / 交互     │
│  WebGPU (wgpu→wasm) 预览渲染  ← 与下面共享 WGSL  │
│  WebCodecs          解 proxy 的 H.264           │
│  Web Audio          AudioWorklet 实时处理       │
└──────────────────┬─────────────────────────────┘
                   │  时间线 JSON（唯一契约）
┌──────────────────▼─────────────────────────────┐
│  API 网关          Rust / axum                  │
│  任务队列          渲染 DAG + 分片               │
├────────────────────────────────────────────────┤
│  渲染 worker       Rust + wgpu headless ← 同上  │
│  解码/编码         FFmpeg (libav)               │
│  音频              FFmpeg filtergraph           │
│  一致性            SSIM 逐帧比对 + 影子环境      │
├────────────────────────────────────────────────┤
│  对象存储          原片 / proxy / 封面图 / 成片  │
└────────────────────────────────────────────────┘
```

**本仓库只实现底座**：`dhampir-core` + 两个宿主（preview / render）。
图中 `API 网关` / `任务队列` / `对象存储` 属**下游工程形态**，不在本仓库交付——
下游可接成客户端-服务端分离，也可接成本地预览 + 渲染的合并处理。

**数据流**（下游形态的完整链路，按需要取用）：

1. 用户上传素材 → probe → 分流（可直编 / 需转 proxy）
2. 生成 proxy（720p、低码率、关键帧密集）+ 封面图 + 雪碧图
3. 浏览器拉 proxy，WebCodecs 硬解，WebGPU 实时预览
4. 编辑操作改的是时间线数据（内存态），不上传素材
5. 用户点导出 → 时间线 JSON 提交服务端
6. 服务端 DAG 编排、按 GOP 分片、wgpu 渲染、FFmpeg 编码
7. merge + package → 成片回对象存储

---

## 3. 分层选型

### 3.1 渲染层：wgpu（不可替代）

wgpu 是编辑器场景唯一现实解：

- **跨平台**：Vulkan / Metal / DX12 / WebGPU
- **Rust 原生**：无需 FFI 进 C++ 图形栈
- **同一套 shader 同时服务预览与导出**
- **同一份渲染图代码可编译到 wasm 和 native**

**预览与导出共用渲染图，只是 sink 不同**（浏览器是 canvas surface，服务端是离屏 texture）。这是整个架构的支点。

### 3.2 浏览器端媒体 I/O：WebCodecs

**覆盖率约 95.5%**（Chrome/Edge/Opera 94+、Firefox 133+、Safari 26.1+ 全支持，Safari 16.6+ 部分）。

**零拷贝关键**：`device.importExternalTexture({ source: videoFrame })` —— WebCodecs 的 VideoFrame 直接变成 WebGPU 纹理，不经过 CPU。

**能力边界（必须自己补）**：

| 缺口 | 补法 |
|---|---|
| 不做 demux / mux | MP4 用 mp4box.js，多格式用 FFmpeg WASM |
| 无多轨时间线 / 图层 / 转场 / 合成引擎 | 自研 `dhampir-core` |
| 音频 | Web Audio API + AudioWorklet |
| AV1 硬件编码覆盖有限 | 导出默认 H.264/H.265，AV1 作可选 |

**性能依据**（1080p30 灰度滤镜实测）：

| 方案 | 每帧耗时 | 帧率 | CPU |
|---|---|---|---|
| Canvas2D `getImageData` | ~45 ms | ~8 fps | 85% |
| WebGL 片元着色器 | ~4 ms | ~60 fps | 15% |
| **WebGPU compute** | **~2 ms** | **~60 fps** | **8%** |

特效越复杂，差距越大。

### 3.3 服务端媒体 I/O：FFmpeg（libav 绑定）

媒体 I/O **必须复用 C 生态**，不自行实现编解码。Rust 侧的绑定层选择与桌面端一致（见附录 A）。

### 3.4 前端 UI：React + TS

浏览器侧 Rust UI 框架没有生态可言。时间线交互的难点不在框架，而在**视图中控**（详见 §6.2）。

### 3.5 音频

- **浏览器预览**：Web Audio API，AudioWorklet 做实时处理
- **服务端**：FFmpeg filtergraph，与视频渲染结果最后 mux

**取舍说明**：音频 DSP（音量包络、淡入淡出）要在前端和服务端各实现一次，但音频对浮点误差的容忍度远高于图形，不需要逐样本比对。在音频上重造轮子不划算。

### 3.6 时间线数据模型：自研

参考 OTIO / GES / MLT 的概念（Stack / Track / Clip / Gap / Transition / Effect），保留 OTIO 或 FCPXML 导出适配器——用户迟早要"导入 pr 工程"。

---

## 4. wgpu 双运行时机制

### 4.1 两条调用链

```
前端 (wasm32-unknown-unknown)
  你的 dhampir-core              ← 同一份源码
    ↓ 调用 wgpu::Device / Queue / RenderPipeline …
  wgpu (统一 API 层)             ← 同一个 crate
    ↓ 分发到
  wgpu-hal 的 WebGpu backend     ← 编译期选定
    ↓ wasm-bindgen FFI
  navigator.gpu (JS WebGPU)
    ↓ 浏览器自己的实现
  Chrome/Edge: Dawn(C++)  |  Firefox: wgpu(Rust，非 wasm)  |  Safari: WebKit 自研
    ↓
  D3D12 / Metal / Vulkan → 驱动 → GPU

服务端 (x86_64 native)
  你的 dhampir-core              ← 同一份源码
    ↓ 调用 wgpu::Device / Queue / RenderPipeline …
  wgpu (统一 API 层)             ← 同一个 crate
    ↓ 分发到
  wgpu-hal 的 Vulkan / Dx12 / Metal backend
    ↓ 直接生成原生调用
  驱动 → GPU
```

两条链在 `wgpu-hal` 这一层分叉，**这以上全部共享**：渲染图、资源管理、bind group 布局、command buffer 录制逻辑，一个字都不用改。

> 有意思的事实：Firefox 的 WebGPU 实现本身就是 wgpu（Rust 代码直接跑在浏览器进程里，不是 wasm）。服务端代码与 Firefox 的浏览器实现其实是同一个引擎。

### 4.2 "对应"发生在三层

**① API 层**：`wgpu::Device`、`wgpu::Texture`、`wgpu::ComputePipeline` 等类型在 wasm 和 native 上是同一套 Rust API、同一套语义。代码没有"浏览器版"和"服务端版"之分。

**② Shader 层**：同一份 WGSL 文本。native 下 naga 编译成 SPIR-V / MSL / HLSL；wasm 下把 WGSL 原文交给浏览器的 `createShaderModule`，由浏览器自己的编译器处理（Dawn 用 Tint，Firefox 用 naga）。

**③ 数据契约层**：时间线 JSON。两端不通信、不共享状态，各自拿同一份 JSON 独立渲染。所谓"一致"是指**输入相同 → 输出足够接近**。

### 4.3 会不一致的四个地方

**① 浏览器专有能力**
`importExternalTexture` 在 native 不存在；native 的 bindless、timestamp query、subgroup 操作在 WebGPU 里也没有。
→ **始终按 WebGPU 的能力下限写**。先按 native 写再想搬 wasm，返工量会很难看。

**② 编译器不同**
同一份 WGSL 被 Tint（浏览器）和 naga（native）分别编译。两个编译器在边界情况、精度处理、优化顺序上可能有差异。这比浮点误差更隐蔽。

**③ 浮点确定性**
同一份 shader 在不同 GPU 上结果不保证逐位相同。用户浏览器可能是 Apple M 系、N 卡、A 卡或 Intel 核显，服务端又是另一家。
→ **SSIM 判据必须设容差，不能要求 1.0**。减轻手法：优先 f32、避免长链累加、避免依赖运算顺序的算法。

**④ 色彩范围**
VideoFrame 带 `colorSpace` 元数据（BT.709 limited range 最常见），转换矩阵配错会画面发灰或过饱和。最容易被忽略又最容易出问题。

**额外成本（诚实说明）**：wasm 里每次调 `wgpu` API 都要跨 wasm↔JS 边界。录制一帧几百个 draw call 时这笔开销真实存在。换来的是渲染代码零重复——对要长期迭代特效的编辑器，这笔账划算。

### 4.4 代码组织

`dhampir-core` 里**不出现任何 `#[cfg]`**。平台差异全部收进两个 trait：

```rust
pub trait FrameSource {
    // wasm:  WebCodecs VideoFrame → importExternalTexture（零拷贝）
    // native: FFmpeg 解码 → 纹理上传 或 DMA-BUF / 共享句柄导入
    fn frame_view(&mut self, device: &wgpu::Device, frame: u64) -> wgpu::TextureView;
}

pub trait FrameSink {
    // wasm:  canvas surface，给用户看
    // native: 离屏 texture，读回交给编码器
    fn acquire(&mut self, device: &wgpu::Device) -> wgpu::TextureView;
    fn finish(&mut self, frame: u64);
}
```

`RenderGraph::render(encoder, target, frame)` 在两个宿主里完全一样——它只认"给我第 N 帧"，不关心帧从 WebCodecs 还是 FFmpeg 来。

`wgpu::Instance` 的创建是唯一需要在入口处分开的地方：

```rust
// wasm
Instance::new(Backends::BROWSER_WEBGPU)
// native
Instance::new(Backends::VULKAN | Backends::DX12 | Backends::METAL)
```

---

## 5. 前后端契约：时间线 JSON

前后端唯一接口，必须先定死。

### 5.1 设计原则

1. **时间用帧号，不用浮点秒**。`start` / `duration` / `source_in` 都是 timebase 下的整数帧数，帧精确是天然成立的，不靠四舍五入。
2. **特效走"类型 + 参数"声明式**，shader 内置在 core crate。
   → 不做可上传 shader：那需要沙箱、uniform 校验、版本管理（腾讯为此写了 VSCode 插件和 Shader Controller 来约束 `#iChannel`、`#iUniform`），成本极高，只有做特效市场时才值得。
3. **帧精确是服务端能按 GOP 边界任意分片、且分片结果与整体渲染逐帧一致的前提。**

### 5.2 Schema

```json
{
  "schema": 1,
  "timebase": { "num": 30000, "den": 1001 },
  "canvas": { "w": 1920, "h": 1080, "fps": { "num": 30000, "den": 1001 } },
  "tracks": [
    {
      "id": "v1",
      "kind": "video",
      "clips": [
        {
          "id": "c1",
          "asset": "asset_7f3a",
          "start": 0,
          "duration": 150,
          "source_in": 0,
          "speed": { "num": 1, "den": 1 },
          "transform": { "pos": [0, 0], "scale": [1, 1], "rot": 0, "opacity": 1 },
          "effects": [
            { "type": "gaussian_blur", "params": { "radius": 8 } }
          ],
          "keyframes": [
            { "prop": "opacity", "at": 0,  "value": 0 },
            { "prop": "opacity", "at": 30, "value": 1, "easing": "ease_out" }
          ]
        }
      ],
      "transitions": [
        { "after": "c1", "type": "cross_dissolve", "duration": 15 }
      ]
    }
  ]
}
```

---

## 6. 素材管线

### 6.1 proxy 策略

浏览器不可能直接编辑源片。上传后第一件事是 probe，然后分流：

| 源素材 | 处理 |
|---|---|
| 可直编（H.264/VP9/AV1 + AAC） | 立即可编辑，同时后台转更小的 proxy |
| 不可直编（ProRes / DNxHD / HEVC 10bit / 专业音频） | 先转 proxy 才让用户进编辑页，原片留服务端供最终渲染 |

**proxy 规格**：720p、低码率、**关键帧密集（每 1 秒一个 I 帧）**。

> 关键帧密度直接决定时间线上拖动的 seek 手感，**比分辨率重要得多**。

同时生成封面图和雪碧图（时间线缩略图带）。

### 6.2 双模式与视图中控（腾讯做法，可直接抄）

**本地 + 云端双模式**：导入即解析判断可否直接编辑，可则走本地工作流（封面图、雪碧图）并后台上传转码，完成后云端化替换，保证换设备数据仍可用。

**视图中控**：点击时提交数据生成拖拽实体、影子元素渲染、松手才真正更新轨道数据。这是时间线性能的关键。

**四类更新**：timer / 缓存 preloader / Clip / 用户行为。

**统一模型**：游戏化父子分层树，所有轨道元素统一称 `Clip`。

### 6.3 前端解码极简化

**只解 H.264，全部走 WebCodecs 硬解，不引入 FFmpeg WASM。**

这是有意的取舍——WASM 那 0.3x 的性能和几十兆的包体积不值得，异构格式交给服务端。

---

## 7. 服务端渲染服务

### 7.1 渲染任务 DAG

```
probe → 按 GOP 边界切 4 秒分片 → 各 rendition 并行编码
      → merge → package(DASH/HLS) → postprocess(缩略图/音频归一化/元数据)
```

- worker 无状态容器，**spot 实例占 80%**
- DAG 状态存 PostgreSQL / DynamoDB，**每 5 秒 checkpoint**
- 心跳丢失即重排
- 规模参考：100 万视频/天，10 分钟视频 P99 < 10 分钟

### 7.2 帧精确与分布式分片

**帧精确 ⇒ 可任意分片且结果完全一致**。这是分布式渲染能成立的唯一前提，也是 §5.1 强调"用帧号不用浮点秒"的原因。

### 7.3 共享内存传帧（腾讯做法）

- FFmpeg 预取帧入共享内存
- 渲染引擎按 handle 取帧
- 结果回共享内存给编码器
- 共享内存分读 / 写模块
- **帧率对齐**：解多少帧就返回等量音视频帧
- 逐帧分析轨道数据，**不需要渲染的内容直接走编码 / 转封装**
- 分片完成后总转封装

> 腾讯的服务端是 Node 进程驱动渲染引擎 + 共享内存 Node 扩展 + 改造 FFmpeg 编解码扩展。Rust 方案能把"渲染引擎"和"编码器"放进同一个进程，省掉一层跨语言边界。

**性能参考**：33 秒原片，解码 → 渲染 → 编码只需 9 秒多。

### 7.4 为什么用 Rust 做 worker

实测优势：

- 无 GC 停顿
- 单 worker 10 并发（Python 3 / Node 2）
- 内存低约 40%
- `ffmpeg-next` 原生模式对比 CLI：快 14–28%，内存省 28–47%

**两个已知坑**：

- **Windows 子进程管理必须用 Job Object**（`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）+ `BELOW_NORMAL_PRIORITY_CLASS`，否则父进程崩溃会留下僵尸子进程
- **tokio 默认线程池面向网络 IO**，计算密集任务要换掉它

### 7.5 部署代价（提前认，**属下游工程**）

> 本仓库不部署、不容器化。以下代价由下游工程承担，此处只作调研记录。

headless GPU 需要容器注入 Vulkan ICD（NVIDIA Container Toolkit 那条链路）；GPU 实例按小时计费、spot 会中断。若要压成本只能上 CPU 软渲染（lavapipe），性能会掉一个数量级。

---

## 8. 一致性保障体系

### 8.1 三层验证（腾讯做法，直接抄）

1. **测试用例集**先生成预期产物 ← 底座（保留）
2. 迭代时按 **SSIM 逐帧比对** ← 底座（保留，M2 已落地）
3. ~~发布前在影子环境抽样比对~~ ← **下游工程**，移出本仓库
4. 维护 **bad case 库** ← 底座（保留）

### 8.2 SSIM 容差

因 §4.3 的三个不确定性（编译器差异、浮点确定性、跨 GPU），**SSIM 判据必须设容差，不能要求 1.0**。阈值需要按实际素材类型标定。

---

## 9. 落地顺序

### 9.1 验证顺序（按"能否证伪"排序）

前两步不通，整个架构要重做：

| # | 验证项 | 为什么排这个位置 |
|---|---|---|
| 1 | **服务端 headless wgpu 能在目标环境跑起来** | 容器 GPU 注入、Vulkan ICD、离屏渲染出图 |
| 2 | **同一份 WGSL 在浏览器 WebGPU 与服务端 headless wgpu 上渲染同一帧，SSIM 比对** | **整个架构的命门** |
| 3 | 浏览器 WebCodecs 解 proxy → `importExternalTexture` → 渲染进 canvas 的完整链路与延迟 | 前端体验基础 |
| 4 | 时间线 JSON 驱动服务端出片，与前端预览比对 | 契约验证 |
| 5 | 按 GOP 边界分片渲染 → merge，验证分片结果与整体渲染一致 | 分布式前提 |
| 6 | SSIM 比对进闸门 + 影子环境 | 长期保障 |

第 1、2 步花不了几天，但决定了后面半年是"一份代码两个 runtime"还是"两份实现互相追赶"。

### 9.2 执行顺序建议

**先在服务端把 `dhampir-core` 写对，再去搬 wasm。** 服务端能 dump 帧、能单步、能跑 SSIM 比对；浏览器里调 GPU 代码的痛苦程度高一个量级。

**但第一天就把 wasm 空壳跑起来**（`Instance` 创建 + 渲染纯色三角形 + 读回像素）。它的作用不是功能，而是**当场暴露 native-only 的 API 使用**。

之后每一版 `dhampir-core` 都同时出两个产物、跑同一帧、比 SSIM。

### 9.3 仓库结构

```
dhampir/                         ← Cargo workspace 根
  Cargo.toml                     [workspace] members
  rust-toolchain.toml            channel + targets = ["wasm32-unknown-unknown"]
  crates/
    dhampir-timeline/            数据模型 + serde + OTIO 适配
    dhampir-media/               解码 / 编码 trait（纯契约，两端各自实现）
    dhampir-core/                渲染图 + WGSL + 特效节点（**无任何 #[cfg]**）
    dhampir-wasm/                浏览器入口（wasm-bindgen + WebCodecs 实现 media）
    dhampir-worker/              服务端 worker（native + FFmpeg 实现 media）
  gateway/                       axum API + 任务队列   ← 下游工程，不在本仓库
  web/                           React + TS           ← 下游工程，不在本仓库
```

`crates/` 下的五个 crate 就是本仓库的**底座**（交付物）；`gateway/` 与 `web/` 是下游工程的形态示意，
**本仓库不实现**。crate 名与依赖方向见文首「命名与项目定义」。
`dhampir-wasm` 与 `dhampir-worker` **互不依赖**——它们是同一份 core 的两个宿主，这是底座里唯一的"双份"之处。

### 9.4 第一天要落的文件

定名后的第一步：把 workspace 骨架立起来，让**双编译链路当天就暴露问题**。这一步不写任何功能。

**验收标准**（三条全绿才算过）：

1. `cargo check --workspace` 在 native 通过
2. `cargo check -p dhampir-wasm --target wasm32-unknown-unknown` 通过
3. 同一个纯逻辑函数在两端输出一致——建议取 `dhampir-timeline` 的"帧号 → 时间码"换算：native 侧用 `#[test]` 断言，wasm 侧导出一个自检函数由 JS 调用断言

第 3 条是这一步真正的目的：它证明"同一份源码两个运行时"不是幻灯片上的话。

此时 `dhampir-core` 里只该有一个纯色三角形或单帧渲染，但 `Instance` 的创建已经分叉（wasm 用 `Backends::BROWSER_WEBGPU`，native 用 `Backends::VULKAN | DX12 | METAL`）——这是**唯一允许分叉的地方**。

**这一步明确不引入**：FFmpeg 绑定（先造假帧源）、WebCodecs、任务队列、任何 UI。

---

## 10. 风险与坑清单

| 风险 | 影响 | 对策 |
|---|---|---|
| **Windows 零拷贝路径未验证** | Media Foundation → D3D11 shared handle → D3D12 是唯一标注"待验证"的路径 | 用 `wgpu-external-frame`（0.1.1）或 `wgpu-native-texture-interop`（0.1.1）封装 HAL 层；预览阶段可先接受 CPU 拷贝 |
| VideoFrame 生命周期 | 不及时 `close()` 会耗尽显存，长视频/多轨场景必爆 | core 里用 RAII 包住 |
| 跨源 VideoFrame 受 CORS 限制 | `importExternalTexture` 拿不到跨源帧 | proxy 与编辑器同源或配好 CORS |
| 前端内存 | 4K 素材三层缓存会吃满 | 设上限 + LRU 淘汰；参考 MASterSelects 的 300 张 VRAM 纹理 + 900 帧 RAM |
| Firefox Linux WebGPU 仍在 flag 后 | 目标用户含 Linux 时预览不可用 | `gfx.webgpu.ignore-blocklist`，Mozilla 称 2026 年内发布；或接受降级 |
| 老 Safari（<16.4）无 WebCodecs | 低版本 macOS/iOS 不可用 | 能力检测 + 降级提示 |
| AV1 硬件编码覆盖有限 | 导出格式选择受限 | 默认 H.264/H.265 |
| 4K 以上需显式显存管理 | 易 OOM | 分块渲染 + 显存预算 |
| headless GPU 在云上贵且难搞 | 成本与稳定性 | spot + 优先级路由；CPU 软渲染兜底 |
| WGSL 双编译器差异 | 隐蔽的不一致 | 能力下限写法 + SSIM 容差 + bad case 库 |

---

## 附录 A：桌面端形态选型（原始结论）

若将来要做桌面端，结论如下（与网页形态共用 wgpu 与时间线模型）：

| 层 | 推荐 | 备选 |
|---|---|---|
| 解码 / 编码 | FFmpeg 系绑定（`ffmpeg-next` / 后续 `rsmpeg`） | `gstreamer-rs` |
| 合成 / 渲染 | **wgpu**（唯一现实解） | — |
| UI | egui（快速原型） | iced / Tauri（长期产品） |
| 音频 I/O | cpal + symphonia | rodio |
| 时间线数据模型 | 自研 | — |
| 色彩 / 重采样 | GPU shader / swscale | rubato（音频） |

**绑定层供应链状况**（2026-09 调研）：

- `ffmpeg-next`：128 万下载/月，但**维护者明确 maintenance-only**（原文 "Any PR to improve existing API is unlikely to be merged"），支持 FFmpeg 3.4–8.0，fork 链已三代
- `rsmpeg`：跟版本更紧（0.18 含 FFmpeg 8.0），但约 11 个月无大动作
- `ffmpeg-sidecar`：约 13 万/月，子进程模型，不链 libav
- `ez-ffmpeg`：约 340 star，单人维护，新 API 标 experimental，`cli` 特性只认 FFmpeg 7.1
- `video-rs`：无音频 API 且自述 WIP
- `symphonia` 0.6.0：HE-AAC / Opus 未完成，无 AC-3，不做视频
- `gstreamer-rs`：约 56 万/月活跃，但 **GES 绑定明确标注非线程安全**

**落地路径**：先用 `ffmpeg-sidecar` 跑通架构，再换绑定层（接口形状一致，迁移成本低）。

**UI 侧要点**：egui 通过 `egui_wgpu::CallbackTrait` 把视频纹理直接绘制进 UI（lumina-video 已验证的做法）。Tauri v2 打包 2–10MB（vs Electron 80–150MB）、空闲内存 30–50MB（vs 150–300MB）。iced 0.14 响应式渲染默认开、CPU 降 60–80%，但编译慢、Windows 无障碍 / IME 有缺陷。GPUI v0.2 在 Windows 仍实验性。

**核心工程决策（与网页形态共通）**：

- 编辑器场景选 **FFmpeg 绑定层而非 GStreamer pipeline 模型**——需要逐帧控制、精确 seek 和自管缓存，用 GStreamer 会与它的调度器冲突
- **预览与导出共用渲染图，仅 sink 不同**（SurfaceSink / BufferSink）
- **音频作主时钟**（cpal 已播放采样数推算位置），而非视频时钟
- **必备 proxy 文件与帧缓存**
- **硬件编解码尽早验证**（`h264_nvenc` / `h264_qsv` / `h264_amf` 或 Media Foundation）

---

## 附录 B：关键数据依据

**带宽量级**（判断零拷贝必要性的基准）：1080p30 ≈ 240 MB/s，4K60 ≈ 2 GB/s。

**WebGPU 状态**：2026 年 1 月进入 Baseline。Chrome/Edge 113+（2023-05）、Safari 26（2025-09，macOS Tahoe 26 / iOS 26 / iPadOS 26 / visionOS 26）、Firefox 141 Windows（2025-07）、Firefox 145 macOS ARM64。**Firefox Linux 仍在 flag 后**。

**WebCodecs 覆盖**：约 95.5%。

**MASterSelects（开源浏览器编辑器参考实现）**：60fps 播放、1080p 导出、30 个 GPU 特效、2,500 行 WGSL、13 个生产依赖（React 19 + Zustand + mp4box + ONNX Runtime）、三层缓存（300 张 GPU 纹理 VRAM + 逐视频帧缓存 + 900 帧 RAM 预览）、导出直接从 GPU canvas 取帧交 WebCodecs，零 CPU 往返。

**FFmpeg WASM 性能墙**：开 SIMD 后 1080p MOV 转码 0.3x 实时；10 worker 池化 + 分布式转码 + 转封装 + 音频单独取出 → 3–4x。

---

## 附录 C：参考实现与链接

**浏览器 / 网页形态**

- `https://webcodecsfundamentals.org/basics/rendering` —— importExternalTexture 与性能数据
- `https://webgpu.io/security` —— WebGPU 实现状态表
- `https://m.thepaper.cn/detail/23027951` —— 腾讯云剪辑架构演讲全文（**最值得精读**）
- `https://www.algoroq.io/system-design/design-video-transcoding` —— 云转码 DAG 设计

**桌面形态**

- `https://kenichistudio.github.io/blog/desktop_video_editor_tech` —— 专业编辑器技术栈
- `https://deepwiki.com/lumina-video/lumina-video/5.1-zero-copy-overview` —— 零拷贝平台矩阵
- `https://www.cnblogs.com/Yeauty/p/22018803` —— Rust 音视频六条路线实测

**Rust 分布式转码参考**

- `https://github.com/danausx/ffmpeg-cluster` —— server/client/common + WebSocket，自动识别 NVENC/QuickSync/AMF/VideoToolbox/VAAPI，帧精确切分重组，SQLite 记录，REST API
- distributed-video-transcoder —— job server + node server，HTTP 任务 API + rsync/SFTP 传输，心跳与失败重排

**参考项目基线**：Gyroflow（Rust + wgpu + FFmpeg 管线）、lumina-video（零拷贝 + egui 集成范例）、videomti-render（声明式 FrameDescription 模型 + SurfaceSink/BufferSink 双输出）。

---

## 附录 D：许可证注意

- **Slint** 是三许可：专有桌面/移动/Web 免版税、开源 GPLv3、**专有嵌入式需商业许可**
- `ffmpeg-next` 是 WTFPL
- `wgpu-native-texture-interop` 是 MPL-2.0
- `egui` 是 MIT / Apache-2.0
- WebCodecs 的跨来源限制：VideoFrame 来自跨源视频时受限，必须同源或配置 CORS
