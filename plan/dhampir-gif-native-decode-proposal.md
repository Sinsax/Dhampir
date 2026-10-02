# 提案：底座原生解码 GIF（贴纸动图直通）

> 来源：下游宿主的实测需求（提出时那个宿主是 V-Trim，精修预览 + 出片）。目的：把贴纸动图
> 从「宿主逐帧喂位图」改成「底座引擎自己解码、自己按时间取帧」，消灭逐帧 HTTP 与宿主侧整条取帧管线。
> 状态：**讨论稿** —— API 命名与内部实现由底座定，本文只钉契约语义与验收判据。

---

## 1. 现状（宿主侧，2026-10 在 V-Trim 上实测的数字）

贴纸是全链路**唯一**还在逐帧走网络的资产：视频是宿主侧 `<video>` 直供，字幕/弹幕在 wasm 里解析。
贴纸现状：

```
进页面     V-Trim 服务端预热：整段解码 GIF → PNG 帧表（进程内缓存，键 = mtime+len）
           冷解码单张 3~9s（66 帧 GIF，debug 构建）
播放中     宿主按帧调 POST /api/polish/sticker-frame → 服务端缓存取帧 → base64 PNG（~390KB/帧）
           回程 14~25ms/帧；宿主 createImageBitmap → 常驻 canvas → setSourceBitmap
```

宿主侧整条管线（`webui/src/components/PolishPreview.vue`，约 100 行）：
按素材时基把「工程帧 → 秒 → GIF 帧号」换算、相位锚点、在飞去重、30ms 超时、PNG 解码中转。

痛点：① 逐帧 HTTP（seek 后首帧晚 ~20ms、偶发一帧抖动）；② base64 膨胀 1.33×；
③ 宿主自己换算帧号 —— 相位/延迟语义两处实现（服务端出片 vs 前端预览），一致性靠纪律守。

## 2. V-Trim 已经提供给底座的数据（工程 JSON 里，无需新增）

转译器（`tools/polish-to-dhampir.mjs`）为每个贴纸资产写的字段，**底座可以直接消费**：

| 字段 | 语义 | 备注 |
|---|---|---|
| `kind: "image_sequence"` | 动图资产 | 语义不变；底座可继续按 kind 分派 |
| `uri` | **相对 asset-root（片段目录）** 的 GIF 原文件路径 | 如 `assets/stickers/生气.gif` |
| `frame_count` | GIF 帧数（探测所得） | |
| `timebase` | GIF **实测平均帧率**（如 100/3 ≈ 33.3fps） | ⚠️ 曾写死 10fps → 动图慢 3.33 倍，别回退 |
| `frame_delays_ms` | **逐帧延迟表，仅非匀速 GIF 才写**（匀速缺省 = 用 timebase） | JSON 里 `undefined` 会被丢弃，即「缺省 = 匀速」 |
| `width` / `height` | GIF 原生尺寸 | |

**时间的唯一权威是「秒」**：引擎现在对每个源算 `seconds`（含 `source.source_in` 偏移），
GIF 帧号应当由底座从秒映射：**匀速** `frame = floor(sec × fps)`；**非匀速**按
`frame_delays_ms` 前缀和查表。宿主不再自己算帧号。

## 3. 提议的契约变更（HOST_API_VERSION 5 → 6）

新增（命名示意，底座定稿）：

```rust
// 宿主把 GIF 原始字节一次性交给底座（由宿主负责取文件：
// 浏览器 = fetch 媒体 URL；原生 = 按 asset-root 读盘 —— 与视频源同一分工）。
// 返回 {frame_count, width, height}，底座内部建延迟表与帧缓存。
dhampir_asset_load_gif(asset_id: &str, bytes: &[u8]) -> Result<GifInfo, Issues>

// 可选：宿主查询（诊断/进度用），不参与逐帧渲染。
dhampir_asset_gif_info(asset_id: &str) -> Option<GifInfo>
```

删除/免掉的宿主职责：逐帧 `set_bitmap`、帧号换算、相位锚点、取帧 HTTP。
**不变**：`sources_for(f)`、`seek → prepare → draw` 单趟流水线、`clear_bitmaps` 时机、
`textToken` 单飞语义（底座内部解码后，宿主那次 30ms 赛跑与在飞去重整个消失）。

兼容策略建议：版本号硬门禁（现有机制：宿主 `DHAMPIR_HOST_API` 与底座
`dhampir_host_api_version()` 不等就当场报错），**两边同一次提交里一起 bump**。
V-Trim 侧有跨仓门禁 `scripts/check-dhampir-wiring.mjs` 把这个数钉住（现在 = 5）。

## 4. 底座实现要点（已知坑，全部实测过）

1. **GIF 解码器**：纯 Rust `gif` crate（MIT/Apache，无 GPL 传染）即可；
   disposal method（restore-to-bg / restore-to-previous）、局部调色板、交错存储都要走对 ——
   V-Trim 服务端解码器（`decode_sticker_frames_png`）是出片权威，两边语义对齐。
2. **内存预算（最要紧）**：解码 RGBA 帧很大 —— 500×500×4B ≈ 1MB/帧，66 帧 ≈ **66MB/张**，
   四张贴纸就是 264MB，不能全量常驻。建议二选一：
   - **按目标尺寸解码**：渲染目标尺寸 = 工程缩放后的盒子（`280 × min(W,H)/1080` 那套），
     解码时直接缩到目标 → 每帧 ~0.3MB，66 帧 ~20MB/张，可全量常驻；
   - 或 **LRU**（保留活跃 span ±N 帧），上限 ~16 帧/张。
3. **非匀速延迟**：`frame_delays_ms` 缺省 = 匀速（timebase）；给了就按前缀和查表，
   **不要**把首帧延迟算进相位（第 0 帧从层起点起播）。
4. **多实例**：同一 GIF 可能被多层/多区间引用（asset_id 相同）—— 解码缓存按 asset_id 建
   一份，层只持引用。
5. **失败形态**：GIF 损坏/超限（如 >64MB）时**保留该层但报 issue**（走 `open()` 的 issues 通道），
   不要 panic —— 出片逐帧路径里任何 panic 都是整块 chunk 失败。
6. **原生/浏览器同源**：出片（native）与预览（wasm）必须用**同一个解码实现** ——
   这是本提案的核心收益：贴纸帧的一致性从「两份实现对齐」变成「同一份实现」。

## 5. V-Trim 侧将配套做的事（底座就绪后一次落地）

- `DHAMPIR_HOST_API` 5 → 6（含 `scripts/check-dhampir-wiring.mjs` 的跨仓钉子）。
- 宿主删整条逐帧管线：`/polish/sticker-frame`、`/polish/sticker-prewarm` 的**预览消费**、
  `dhampirStickersFrom` 的帧号/相位/在飞去重逻辑（保留层区间重建，供诊断条）。
  服务端解码器保留 —— 出片腿（V-Trim 服务端 PNG 帧）在底座出片也切 GIF 前仍是权威。
- 帧循环里改为：每贴纸 fetch 一次 GIF（`clipMediaUrl`，带缓存）→
  `dhampir_asset_load_gif(assetId, bytes)` → 完事。
- 门禁更新：`check-dhampir-wiring` 的位图探针换成 GIF 探针；parity 对拍
  （`scripts/vtrim-compare.mjs --golden`）重跑三样本。

## 6. 验收判据（两边共用）

1. **逐帧一致**：同一工程、同一帧号，预览贴纸像素与出片一致
   （V-Trim 侧黄金帧对拍：基线中位像素差 ≤ 现行阈值 20/255，贴纸窗口重点抽查）。
2. **相位正确**：GIF 从层起点起播、`source_in` 偏移生效；非匀速表按延迟走
   （构造一个 [30,30,…,200,…] 的测试 GIF 钉住）。
3. **性能**：页面加载每贴纸一次传输（GIF 原文件，通常 0.1~2MB）；播放期**零**贴纸请求；
   seek 到任意时刻贴纸首帧即时（<1 帧间隔）；内存 ≤ §4.2 预算。
4. **版本门禁**：版本不匹配当场报错（不是静默回退近似预览）。

## 7. 不建议的替代路线（为何不是它们）

- **前端 JS 解码 GIF**：目标浏览器无 `ImageDecoder`（实测）；且换解码器 = 预览/出片
  逐帧一致性失保（disposal/透明处理各家有微差）。
- **服务端批量预取 PNG**：内存峰值高（~100MB 级）、解码总量不变，只是把请求藏起来。
- **维持逐帧 + 小批量预取**：能解 seek 首帧延迟，但「两份帧号实现」的结构性成本一直在。

---

## 8. 底座侧核实与补充（评审记录）

> 以下由底座（Dhampir）仓对照源码逐条核实后补记。原文 untouched，行号均指底座仓。

### 8.1 方案声明 vs 底座现状

| 方案声明 | 底座现状 | 结论 |
|---|---|---|
| `HOST_API_VERSION` 5→6，两边同提交 bump | 常量在 `dhampir-timeline/src/host_api.rs:228`；`docs/host-api.md` 的版本行与导出名单由 `scripts/api-surface.mjs` 扫源码钉住 | ✓ 且新增两个导出**必然** bump：v3→v4 先例（`host_api.rs:209-214`）——导出面变了就是破坏性改动，形状不改也要升 |
| `sources_for(f)` 给「源+帧号+秒」 | `SourceView { source, source_frame, seconds }`（`host_api.rs:498-503`）；**`source` 就是 asset_id**（`compose.rs:311-315`）；秒按**素材自己的时间基**算（`timeline_host.rs:2512-2522`） | ✓ |
| `timebase` / `frame_delays_ms` 可直接消费 | 契约字段已在（`project.rs:115,122`），并已接进求值：`AssetTimebases` + `insert_with_delays`（`layer.rs:1080-1091`） | ✓ |
| 「GIF 帧号应当由底座从秒映射：匀速 floor / 非匀速前缀和」 | 实际机制更强：**compose 求值内直接换算**，`source_frame_at_delays`（`layer.rs:831`，逐帧延迟累加、循环取余、`source_in` 为圈入口）与 `source_frame_looped`（`layer.rs:901`），宿主拿到的 `source_frame` 已是最终帧号 | 结论成立，**表述要修正**（见 §8.2-1） |
| 不变：`seek → prepare → draw` 单趟、`clear_bitmaps` 时机、`textToken` 单飞 | 实际每帧管线是 `sources_for → begin_frame → clear_bitmaps → set_bitmap → draw`（`timeline_host.rs:45-63`），另有 `bind_source` / `preroll` / `end_frame`；`begin_frame`（`timeline_host.rs:2214`）带内容标识去重 | 方向对，**清单不完整**（见 §8.2-2） |
| §4.3 「不要把首帧延迟算进相位」 | `source_frame_at_delays` 的现状即如此：`ms` 从 `local_frame=0` 起算，第 0 帧占 `delays[0]`，层起点起播第 0 帧（有单测钉住，`layer.rs:1170-1189`） | ✓ 已是现状，写进验收即可 |

### 8.2 需要修正的表述

1. **「从秒映射」改为「compose 求值直出帧号」**。宿主不需要、也不应该拿 `seconds` 去换算任何东西——`seconds` 只是给 `<video>` seek 用的驱动字段。GIF 的帧号由求值层从「时间线帧 + 延迟表」直接得出，延迟表路径已有回归保护（匀速时与单一时间基换算一致，`layer.rs:1194-1204`）。这比「从秒映射」少一次浮点往返，也是「两份帧号实现」问题的根治点。
2. **「不变」清单补全，其中有一个真要动的点**：`begin_frame` 的内容标识去重目前按宿主申报的源内容盖章。贴纸改引擎内部取帧后，若 GIF 层不进内容标识，预渲染缓存可能在「GIF 帧已变、视频源未变」的两次时间线帧之间错误命中。定稿必须二选一：把 GIF 层的 `(asset_id, 引擎算出的帧号)` 纳入内容标识；或明确 GIF 层不参与预渲染缓存。`sources_for` 可保持现状（继续输出 GIF 条目，宿主仅对 video 条目行动，条目留作诊断）。
3. **失败形态的落点**：渲染期没有 issue 通道——`resolver.texture_for` 返回 `None` 的层被**静默跳过**（`render/timeline.rs:1005-1007`）。§4.5 的「报 issue」要指明位置：**加载/打开阶段**走 `open()` 的 issues 通道；渲染期 native 走出片问题清单，wasm 预览静默缺层。别指望 `draw` 时报 issue。

### 8.3 底座定稿前必须补的决策

1. **解码器住哪个 crate**：建议 `dhampir-core`。它是 wasm 与 native 唯一共同链接的层，「同一份解码实现」字面成立；`dhampir-media` 是零实现纯契约层（`media/lib.rs:6-18`），不破例。依赖侧：`gif` crate（MIT/Apache）合规，`scripts/licenses.mjs --check` 清单同步；`png` 已是 core 直接依赖（`Cargo.toml:58-60`），先例成立。
2. **时间真值归一**：引擎解码出的逐帧延迟表为准，load 时写回 assets 表（`insert_with_delays` 已有；「延迟表为准」的纪律已有先例，`layer.rs:828-830,1079`）。工程 JSON 的 `timebase` / `frame_delays_ms` 退为诊断与交叉校验，不一致报 warning——否则「映射用一张表、像素用另一张表」会把漂移请回来。
3. **「按目标尺寸解码」的目标定义**：建议按**文档坐标系的层盒子**解码——两端渲染同一份文档坐标，解码产物天然逐字节同源，parity 不受预览画布/导出尺寸影响。预算超限才退 LRU；且 GIF 无随机访问关键帧，LRU 缺帧要整段重放，满足不了 §6.3 的「seek 首帧 <1 帧间隔」——建议**按目标尺寸全量常驻为默认，LRU 仅作超限阀**。
4. **API 形状对齐**：`Result<GifInfo, Issues>` 是示意。按 `OpenResult` 先例定 JSON 返回体（`{parsed, ok, info?, issues?/error?}`，成功/失败形态各只出自己的键）；`bytes` 走 `Uint8Array`（`&[u8]`）；`asset_id` 必须是已登记且 `kind = image_sequence` 的资产；重复 load 幂等替换；GIF 缓存生命周期 = `open()` 清空（当前流程资产随 open 静态化，够用）。
5. **非 GIF 动图的边界**：`infer_kind` 也把 apng 标成 `ImageSequence`（`bin/dhampir.rs:1638`）。`load_gif` 收到非 GIF 资产时的行为要定义（报 issue，该层按失败形态处理），别静默。
6. **透明混合约定**：GIF 帧是带 alpha 的 RGBA 直通，须与现有 bitmap 路径同一套预乘/采样/过滤约定，否则 parity 的差会出在混合而不是解码。

### 8.4 可行性结论与成本

- **结论：可行**。核心收益（预览与出片共用同一解码实现）在架构上成立：wasm 与 native 链接同一份 core；契约数据（`timebase` / `frame_delays_ms` / `frame_count`）已就位，不需要动工程 JSON。
- 成本粗估：core 解码模块 + 单测（含 §6.2 的 `[30,30,…,200]` 测试 GIF）为主，加 host_api v6 形状与键集测试、两端接线、门禁（api-surface / licenses / 依赖图）/ 黄金帧重跑。合计一周级。
- 风险：wasm 加载时同步解码约 100ms/张（66 帧、目标尺寸）——一次性、可接受；wasm 体积 +~50KB（gif+weezl）。
- 过渡期提醒：V-Trim 服务端出片腿未切底座前仍是两个解码器，§6.1 的 20/255 是**过渡期**口径；终态（同解码器、同盒子解码）贴纸窗口应显著低于该值，建议终态另设收紧线（如 ≤2/255）防回归被阈值掩盖。

### 8.5 验收判据补充

1. 非匀速与匀速（JSON 缺省延迟表）两条路径各出一条黄金帧；
2. 同一 GIF 被多层/多区间引用 → 只解码一份（可断言解码计数）；
3. 损坏 GIF → 加载期 issue + 该层不渲染 + 两端都不 panic（native 记入出片问题清单）；
4. `begin_frame` 内容标识含 GIF 帧（换帧不被缓存去重盖掉）；
5. 延迟表真值冲突（解码 vs 工程 JSON）报 warning，且以解码为准。

---

## 9. 动图格式扩展评估：GIF vs 动画 WebP（定版记录）

> 应「下个版本是否连带动画 WebP」的决策请求补记。结论先行：**动画 WebP 无结构性难度，并入下个版本**；同一版本内两步落地，GIF 先切。

### 9.1 事实核查（image-webp 0.2.x，image-rs 官方 crate）

- 纯 Rust 解码器**已支持动画**：README 明确 lossless / lossy / alpha / animation 全支持，速度为 libwebp 的 70~100%；整仓 `cargo geiger` 零 unsafe。
- 动画 API 完备：`is_animated` / `num_frames` / `loop_duration` / `loop_count` / `read_frame(&buf) -> 帧延迟ms` / `reset_animation`（逐帧顺序读 + 回卷，与按前缀和取帧的用法天然匹配）。
- **合成语义在解码器内部完成**（canvas 累积、alpha blending、dispose），集成侧比 GIF 还薄——GIF 的 disposal 合成要自己写，WebP 不用。
- 依赖仅 `byteorder-lite` + `quick-error`，wasm 体积增量可忽略（比 `gif` 还小）。
- 依据：image-rs/image-webp README 与 `src/decoder.rs`（0.2.3，本次评审时查证）。

### 9.2 三维对比

| 维度 | GIF | 动画 WebP |
|---|---|---|
| **加载解码 CPU** | LZW + 调色板，低；66 帧@目标盒子 ≈100ms（一次性） | 无损 VP8L 与 GIF 相当；**有损 VP8 约高 2~5×**（同为一次性加载成本，播放期无差别） |
| **合成期性能** | 与格式**无关**：两者都是加载时一次解码 → RGBA 帧缓存，播放期只取缓存帧，零逐帧解码 | 同左（性能等同） |
| **内存** | RGBA@目标盒子，同预算 | 相同 |
| **传输** | 0.1~2MB/张 | 通常再小 30~50%（每贴纸一次传输的模型下是小赢） |
| **时间语义** | 延迟为厘秒（10ms 粒度）；存在 0/极短延迟的编码器怪癖，需与 V-Trim 服务端解码器用同一套钳制策略（§4.1 对齐项的具体化） | 24-bit 毫秒粒度，天然干净，无怪癖 |
| **解码器成熟度** | `gif` crate 老牌、久经考验；dispose/blend 合成在集成侧（自写） | image-webp 动画路径较新但测试覆盖完整；合成在解码器内部 |
| **开发增量** | 基线：host_api v6、动图加载管线、解码缓存、`begin_frame` 内容标识、门禁、黄金帧——**全部格式无关** | 仅薄适配层：magic 嗅探（`GIF8` / `RIFF…WEBP`）+ `read_frame` 循环收帧，百行级 |

### 9.3 结论与版本决策

1. **决策：动画 WebP 并入下个版本**（与 GIF 同版本，不另立后续）。依据即 §9.2：合成性能与内存与格式无关，全部基础设施共享，WebP 边际成本是百行级适配。
2. **落地次序（同版本内两步）**：
   - ① 共享动图基建 + **GIF 先切**——它是 V-Trim 的生产阻塞项，且过渡期 golden 对拍有服务端基线可用；
   - ② **WebP 随后并入**，验收用引擎自身双端一致性（同一解码器，预览 = 出片，这正是本提案的核心收益）；**过渡期不做服务端对拍**——V-Trim 服务端解码器不支持 WebP，也不必为过渡期去扩它。
3. **API 命名随之定案**：`dhampir_asset_load_gif` → 语义升级为「加载动图资产」（内部按 magic 嗅探分派），`GifInfo` → `AnimInfo { format, frame_count, width, height, loop_count }`。host_api 仍是一步 5→6。
4. **kind 登记边界**：底座 `infer_kind` 把 webp 归静态图（`bin/dhampir.rs:1639-1645`「webp 两可」注释）。动图 WebP 的登记 kind 可能与事实不符——`load_animation` 以 **magic 为准**并报 warning（登记是猜测、解码是事实，与 §8.3-2「延迟表为准」同一条纪律）；V-Trim 转译器本来就写 `kind: "image_sequence"`，正常路径不受影响。
5. **两个待实测项（不阻塞立项，进任务清单）**：
   - 有损 WebP 在 wasm 的加载解码耗时（超标则约束贴纸用无损 WebP——贴纸多为平涂美术，无损更小也更快）；
   - WebP 黄金帧 fixture 生成：引擎自身出基线，浏览器解码交叉抽查防解码器单边错。
6. **术语校准（「不再分离为帧」的三层含义）**：
   - **宿主/管线层——彻底消失**：GIF/WebP 整个文件一次性进引擎（一次 fetch + 一次 `load_animation`）。逐帧 HTTP、PNG 帧表、base64、宿主逐帧 `set_bitmap`、帧号换算、相位锚点全部免掉；播放期贴纸零请求、宿主零操作。
   - **引擎内部——帧仍存在，但降级为私有缓存**：load 时整段解码成 RGBA 帧序列 + 延迟前缀和表（按目标盒子，全量常驻），此后合成每帧只做 `texture_for(asset_id, frame)` 缓存查找 + 纹理绑定，**播放期零解码，seek 也是缓存命中**。
   - **GPU 层——合成吃的是纹理**：LZW/调色板/VP8 数据 GPU 采样不了，「文件直接参与合成」的准确表述是：**文件是加载单位，帧是引擎内部缓存单位，纹理才是合成采样单位**。「分离为帧」没有消失，而是从「跨网络的逐帧管线」塌缩成「加载期的一次性内部步骤」——性能收益正来自这个塌缩。

---

## 10. 落地进度（底座侧执行记录）

> 任务清单与目标在会话里维护（T1…T7）；这里只记**已完成**的落地事实与验证证据，
> 供下一次接手的人知道从哪继续。

### 10.1 已完成：T1 —— core 动图解码模块

* **新增** `crates/dhampir-core/src/animation.rs`（449 行 + 测试）：
  * `detect_format` 按 **magic** 分派（`GIF87a/89a` 与 `RIFF????WEBP`），不看扩展名；
  * **GIF**：用 `gif` crate 取「增量矩形 + RGBA」，**画布与 disposal 合成由本模块自己做**
    （`Any/Keep` 不动作、`Background` 清前一帧矩形为透明、`Previous` 恢复到**绘制之前**的留底）；
    透明像素（alpha = 0）**不落笔**；延迟按 10ms 单位换算为毫秒且**原样保留 0**；
  * **动画 WebP**：`image-webp` 自己维护画布（`read_frame` 直接给合成好的整张画布 + 该帧时长），
    无 alpha 的动画补成 RGBA8；静态 WebP 明确报 `NotAnimation`；
  * **三道闸**：原始字节 64MB、帧数 4096、解码后 256MB —— 全部**明确失败**，绝不 panic。
* **依赖**：`gif = "0.14"`、`image-webp = "0.2"`（纯 Rust、零 unsafe、宽松许可），
  加在 core 并附理由注释（与 `png` 同一条：两端要同一个实现）。
* **测试夹具**：`crates/dhampir-core/tests/fixtures/animated_lossless.webp`
  （取自 `image-webp` 自己的测试资产，出处与许可记在旁边的 `CREDITS.md`）。

**验证证据**（`cargo test -p dhampir-core --lib animation`）：12 项全绿 ——
magic 嗅探、延迟换算与前缀和、零延迟保留、透明不落笔、Background 处置、
Previous 处置（钉住"恢复到绘制之前"而不是"上一帧的样子"）、循环次数读取、
三道闸、截断 GIF 不 panic、WebP 整段解码与文件自报账目一致、静态字节流拒收。
`cargo check --workspace` 通过（wasm 与 worker 两个宿主不受影响）。

### 10.2 已完成：T2 —— core 动图纹理缓存

* **新增** `crates/dhampir-core/src/render/animation.rs`（278 行 + 测试）：
  * `AnimationTextures`：解好的帧**整段上传显存**，按 `(asset_id, 帧号)` 取纹理；
    同一 id 再传即**替换**（幂等，编辑里换素材文件必须生效）；
  * 帧号口径**只有一处实现**（`resolve_frame_index`）：越界停在最后一帧、负号落第零帧 ——
    不循环的层跑到素材末尾之后就靠它；循环层的取模在求值层已经做完；
  * **显存预算闸**：`memory_bytes()` 超过 `budget_bytes`（默认 256 MiB）时**明确拒绝**，
    而且拒绝**不污染已上传的账**；
  * 逐帧像素长度与画布对不上时**报错**（否则传上去是错位的像素，比失败更难查）；
  * `FRAME_FORMAT = Rgba8Unorm`：与 worker 的 `WORK_FORMAT`、wasm 的预览格式一致，
    有测试钉住（跨三个 crate，改一处就得改三处）。
* **尺寸决策（与 §8.3-3 的偏差，已定）**：按**素材原生尺寸**上传，缩放交给 GPU 采样
  （与视频源同路）。理由是**逐帧一致性优先** —— 两端拿到的源像素因此逐字节相同，
  缩放差异被限制在采样器上。代价是显存按原生尺寸算，由上面那道预算闸兜住。

**验证证据**：`cargo test -p dhampir-core --lib animation` 14 项全绿（含 2 项缓存单测）；
另加**真机**集成测试 `crates/dhampir-worker/tests/animation_gpu.rs`（4 项，
`cargo test -p dhampir-worker --test animation_gpu -- --ignored`）——
上传后按帧号取到的**是四张不同颜色的帧**（证明帧号真被用上）、越界停在最后一帧、
负帧号落第零帧、未知资产取不到、超预算明确拒绝且不动已上传的账、帧字节数对不上被拒。

### 10.3 已完成：T3 —— host_api v6

* `HOST_API_VERSION` 5 → **6**，版本注释补 v5→v6 段（理由与 v3→v4 同类：形状没变、**导出面变了**）。
* 三个新形状（都带 `json-schema` feature 派生）：`AnimInfoView`、`AnimLoadResult`、`AnimQueryResult`。
  `AnimInfoView` 里 **`frame_delays_ms` 是解码器读出来的时间真值**（§8.3-2 的落点）。
* **键集测试**钉死三种形态：成功只出 `parsed/ok/info`、失败只出 `parsed/ok/error`、
  查得到出 `loaded/info`、查不到只出 `loaded` —— 少一个 `null` 就少一类对端崩。
  另有一条测试钉住「这一版是 6」。

### 10.4 已完成：T4 —— wasm 宿主接线

* 两条导出：`dhampir_asset_load_animation(asset_id, bytes)` 与 `dhampir_asset_animation_info(asset_id)`。
  **magic 认格式、不看扩展名**；认得出但解不开是 `parsed:true, ok:false`（两件事分开报）。
* **时间真值归一**（§8.3-2）：新增 `ANIMATION_TIMING`（asset_id → 延迟表）与 `assets_with_animation_truth()`，
  **六个求值点全部改走它** —— 原先只有 draw 一处，漏掉任何一处就会出现「同一帧里两个贴纸相位不同」。
* **解析器接入**：`BoundVideos::texture_for` 里动图缓存**排在最前**（按 `source_frame` 定位），
  之后才是 video / 位图那两条路。放最前的理由写进注释：让位图抢先，贴纸会**永远钉在宿主给的那一帧**，
  画面看起来只是「动图不动了」。
* **生命周期挂 `open()`**：新增 `clear_animations()`，与历史、预渲染缓存同时清。
* 上传失败时**同时撤掉时间真值**：要么两条都立着，要么都不立。

**验证证据**：`cargo check -p dhampir-wasm` 与 `--target wasm32-unknown-unknown` 都通过。

### 10.5 已完成：T5 —— native 出片接线

* `DecodingSources` 新增 `animations: AnimationTextures` + `animation_tried`，
  解析器里动图**优先于 ffmpeg**（拿动图去喂 ffmpeg 会得到「能解码但内容不是这一帧」）。
* **惰性加载** `ensure_animation`：先读**头 12 字节**认 magic（视频几百 MB，不能为了判断整个读进来），
  是动图才整段读 + 解码 + 上传；**只试一次**（失败的文件每帧重试会把日志刷成墙）；
  失败记 issue（`animation_decode_failed` / `animation_upload_failed` / `asset_unavailable`）并**退回 ffmpeg 那条路**。
* **时间真值归一（native 侧）**：新增 `asset_timebases_with_animations()`，
  在建 **RenderPlan** 时就按解码器读出的延迟覆盖资产表 —— 求值与渲染必须用同一张表，
  只在渲染侧用解码真值会出现「帧号按工程 JSON 算、像素按解码帧数给」。CLI 两个建计划点都接了。

**验证证据**：`cargo check --workspace --all-targets` 通过。

### 10.6 已完成：T6 —— 门禁与文档

* `node scripts/api-surface.mjs --write`：版本行 5→6、`docs/api-surface.md` 整份重生成。
* `docs/host-api.md`：**新增 v5→v6 一节**（两条导出的契约、`info` 字段表、
  「`frame_delays_ms` 是时间真值」与「这一路不再 `set_bitmap`」两条必须照做的约定、已知边界三条），
  并把两条导出**手工**加进文末名单（守卫只替你补版本行，名单得人写）。
* `node scripts/licenses.mjs --write`：第三方清单 157 → **163** 个 crate（`gif` / `image-webp` 的依赖树）。
* 根 `Cargo.toml` / `crates/dhampir-core/Cargo.toml` 注释已在 T1 写明理由，本项复核无待办。

**验证证据**：`node scripts/run-guards.mjs` —— 20 条守卫全绿（`api-surface` 报
「6 个模块 / 57 个导出；版本 6，`docs/host-api.md` 列了 29 个导出」；`licenses` 报 163 个 crate 一致）。

### 10.7 已完成：T7 —— 全量验证

* `cargo check --workspace --all-targets`：通过。
* `cargo check -p dhampir-wasm --target wasm32-unknown-unknown`：通过。
* `cargo test --workspace`：**除 2 项与本改动无关的既有失败外全绿**。
  那 2 项在 `crates/dhampir-worker/src/bin/dhampir.rs` 的 `is_absolute_uri` 测试里
  （`/abs/a.mp4` 应当按绝对处理）—— **已用 `git stash` 在干净树上复现**，确认是这台 Windows
  机器上的既有环境问题，不是本次改动引入的。
* 新增 `crates/dhampir-worker/tests/animation_gpu.rs`（4 项真机测试）全绿。
* `scripts/check-text-hygiene.mjs` 等全部守卫通过。

### 10.8 真实素材验证（clip-18，2026-10 实测）

用 V-Trim 的真实产物「谁想和我甜旮旯 / clip-18」跑通了**第一次真实素材检验**。
素材位置：`<clip-18>/assets/stickers/`（四个 GIF）与 `data/sticker_frames.json`。

**四个 GIF 的解码事实**（`cargo run -p dhampir-core --example animation_probe --`）：

| 文件 | 文件字节 | 画布 | 帧数 | 总时长 | 显存 |
|---|---|---|---|---|---|
| 叹号.gif | 2.6 MiB | 500x500 | 38 | 1140 ms | 36.2 MiB |
| 思考(认真地).gif | 2.2 MiB | 500x500 | 32 | 960 ms | 30.5 MiB |
| 电风扇2.gif | 1.8 MiB | 500x500 | 32 | 960 ms | 30.5 MiB |
| 笑.gif | 1.9 MiB | 500x500 | 32 | 960 ms | 30.5 MiB |

四个都是「30 ms 均匀延迟、无零延迟帧、无限循环、延迟之和 == total_ms」——
**解码器读出的延迟表与 V-Trim 探测的那份逐值相同**（对比 `sticker_frames.json` 的
`delays` 数组：叹气 38 个 30、其余各 32 个 30）。这是 §8.3-2「时间真值归一」
能成立的**实测依据**，不再是推理。

**逐帧方案的代价被量化了**（V-Trim 现状 vs 引擎原生解码）：

| | V-Trim 现状（逐帧 base64 PNG） | 引擎原生解码 |
|---|---|---|
| 帧数 | 134 | 134（一致） |
| 延迟表 | 全 30 ms | 全 30 ms（一致） |
| 传输/落盘数据 | 46.2 MiB（`sticker_frames.json`） | **8.4 MiB**（GIF 原文件） |
| 膨胀 | **4.1x** | 1x |

**端到端「直接进合成」验证**（`cargo run -p dhampir-worker --example animation_compose --`）：

* 后端 **Vulkan**（真机，非软渲染）；
* 四张贴纸解码 + 上传合计 **218 ms**（平均 55 ms/张，与方案预估的 ~100ms/张同量级且更好）；
* 源内帧 0 / 1 / 10 / 37 各自合成出**四个不同的画面摘要**，且每帧都有内容
  （非透明像素 61709 / 61518 / 60768 / 59407 —— 随帧号单调变化，证明内容是活的）；
* 这条链是：**GIF 字节 -> 解码 -> 上传 -> 渲染器按 `(source, source_frame)` 取纹理 -> 合成**,
  中间**没有**"分离为帧再逐张喂宿主"那一步。

**由此发现的真实预算问题（需要决策）**：

四张贴纸合计 **127.8 MiB** 显存，而当前预算是 256 MiB —— **余量只有 2 倍**。
一个引用 8 张以上贴纸的工程就会撞上限。可选对策（尚未实施，按需要再定）：

1. 按**目标显示尺寸**解码而不是原生 500x500（贴纸通常显示得比 500 小，可省数倍显存）；
2. 提高预算上限（如 512 MiB），并让宿主能按机器能力传参；
3. 引入 LRU 驱逐（但 GIF 无关键帧，驱逐后回放要重播整段，会牺牲 seek 延迟承诺）。

方案 §8.3-3 当初选的「按原生尺寸上传」（理由：两端逐帧一致性）正是这个代价的来源。
实测数据出来后，这一条**值得重新权衡**。

### 10.9 仍未覆盖的部分（诚实的边界）

* **浏览器预览侧未跑**：10.9 验证的是 **native/Vulkan** 这一端。
  wasm 侧的接线已编译通过（含 wasm32 目标）且逻辑同构，但**没有在浏览器里跑过真素材** ——
  那需要起 web 宿主、把 GIF 喂给 `dhampir_asset_load_animation`，属于 §5 的配套工作。
* **`sources_for` 是否过滤动图层**未定：当前实现**不过滤**（动图层照旧出现在清单里）。
  这意味着 JS 侧不能"清单里有的源都去 seek"。两种做法各有道理，
  需与 V-Trim 侧一起定（改了会动 `sources_for` 的形状语义）。
* **`begin_frame` 的内容标识与动图帧的关系**（§8.3-5 遗留项）**未动**：
  动图层的纹理由引擎缓存提供，不经过 `set_bitmap` 那条上传路径，因此不受内容标识去重影响 ——
  但「动图层要不要参与预渲染缓存」这件事没有明确收口。
* **`frame.delay == 0` 的钳制策略**（§8.3-6 遗留项）仍是**保留 0**（解码层不猜）。
  **本次四个真实素材没有零延迟帧**，所以这条分歧在实际素材上暂时不显现。
* **LRU 驱逐未实现**（按 §9.3 决策：整段常驻，超预算明确失败）。
* **「用这个项目跑完整出片」未做**：没有走 `dhampir render` 出 mp4，
  也没有与 V-Trim 出的成片做像素对比 —— 10.9 验的是**合成算子的输入输出**这件事本身。

* **`begin_frame` 的内容标识与动图帧的关系**（§8.3-5 遗留项）**未动**：
  动图层的纹理由引擎缓存提供，不经过 `set_bitmap` 那条上传路径，因此不受内容标识去重影响 ——
  但「动图层要不要参与预渲染缓存」这件事没有明确收口，留给 §5 落地时一并定。
* **`frame.delay == 0` 的钳制策略**（§8.3-6 遗留项）仍是**保留 0**（解码层不猜），
  与 V-Trim 服务端解码器的口径对齐需要跨仓库确认。
* **LRU 驱逐未实现**（按 §9.3 决策：整段常驻，超预算明确失败）。

