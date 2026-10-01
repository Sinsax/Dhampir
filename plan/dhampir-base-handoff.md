# Dhampir 底座：待修问题交接单（可直接在 Dhampir 仓独立处理）

> **这份文档是自包含的**：不需要下游仓、不需要它的工程，第 1～2 节的复现都能在 Dhampir 仓里做完。
> 来源：下游侧在"精修效果链路统一 + 类型收敛"（2026-10-01）期间整理；下游默认出片通道就是本底座。
> 每条都给了 **症状 / 影响 / 最小复现 / 已排除项 / 可疑点（带 file:line）/ 验收判据**。
> 行号按本次整理时读到的代码，若已变动请按函数名搜。

## 结论摘要

| # | 项 | 本次核对后的状态 |
|---|---|---|
| **D1** | `overlay` 渐变色罩**不出图** | ✅ **在本仓 HEAD 复现不出来**（`overlay` 出图，且逐像素吻合解析值）；已补**两条真机 GPU 测试**把这一族钉住（含 §5.2 的 `angle=45` 方向）。见 §7.1 |
| **D2** | `--from` 单独给出时**注释与代码不一致** | ✅ **已修（把实现改成与承诺一致）**：只给 `--from` ⇒ **到工程结尾**；`check-cli` 加了第 33 条判据并**变异验证过会红**。见 §7.2 |
| **D3** | `DanmakuSpec.duration_ms` 注释与实现相反 | ✅ **模块头已按实现补全**（逐条 `travel_ms` 优先，读不到才回退轨道级；`End` 始终不参与）。见 §7.3 |
| **B1** | `stage` fit 的 **contain + 背景层** | ⚠️ **已量清，且用现成原语就能表达，底座不需要新概念**：竖屏导出实测 **43.75% 透明**（= contain）；`transform.scale=1.7778` ⇒ **0 透明**（= cover）；模糊底 = 同源层 + 调整层模糊 + 主层三步。**归转译器**。见 §7.4 |
| **B2** | 放大**重采样偏软** | ✅ **不复现，方向相反**：1 像素棋盘放大后 **100% 是插值中间值**（与 ffmpeg `bilinear` 一致，最近邻是 0%）；高频能量比 ffmpeg 双线性 **高 2.59×**。见 §7.5 |
| **B3** | 高亮词**段间水平推进量**不同源 | ⚪ **已知残差，本仓文档早已写明**（`docs/usage.md` 的"两端各有一处已知残差" + `text_overlay.rs` 的"行内偏移"块）。要抹平得让出片那一路也拿到同源度量。见 §7.6 |
| **B4** | 弹幕 **`\pos` 静态 cue** 仍走轨道级 travel | ⚪ **已知残差，且转译侧有现成解**：`docs/usage.md` 已写明；底座已有逐条通道（`\move` 6 参数的 `t2-t1`），转译器给静态 cue 合成一个 `\move(0,0,0,0,0,travel)` 即可逐条。见 §7.7 |
| **B5** | 字幕**文字阴影**在契约里没有字段 | 🔷 **建议暂不扩契约**（观感项、下游已能如实报 DROP、扩了要两端实现并重跑逐字节不变）。要不要扩请底座这边拍。见 §7.8 |
| **D4** | （**本仓新发现**，不在原交接单里）`overlay` 的 `shape=0`（纯色）取的是 **`r2/g2/b2`**，不是 `r/g/b` | ✅ **已修（改实现）**：证据是下游转译器 `solidEffect` 把两组颜色写成一样 ⇒ 修正对现有工程**零影响**。见 §7.9 |

---

## 1. D1（重要）`overlay` 渐变色罩完全不出图

### 症状与影响

任何 `{"kind":"overlay", …}` 的调整层在出片里**整层消失**（片头渐入、色罩、氛围层都属于这一类）。
不报错、不崩溃、退出码 0 —— 下游的默认出片通道就是本底座 ⇒ 用户看到的是"这块效果没了"。
**连带影响**：下游侧 D5 新做的 `overlay.angle`（渐变方向换算）在底座出片里**无法目视验证**。

### 最小复现（自包含，不依赖 下游）

**第 1 步**：把下面这段存成 `/tmp/ov.doc.json`（`clip.mp4` 换成你们自己的测试片段：1920×1080、timebase 60/1、**480 帧**）。
结构 = 一条视频层 + 一条**调整层**（无 `source`、只有 `overlay` 特效，窗口帧 30..126）：

> **⚠️ 2026-10-01 就地修正（本仓实测）**：原文这段 JSON **缺 `timeline.schema`** —— 载入器要它，
> 直接粘贴会报 `工程文件字段不符：missing field schema`（另外还缺 `meta` 与 `frame_count`，
> 且 `end` 写到 720 会撞 `source_range_exceeded`）。下面是**实测能跑**的那一份。

```json
{
  "project_schema": 1,
  "generator": { "app": "dhampir", "version": "0.0.1" },
  "meta": { "title": "D1 复现", "created_at": null, "modified_at": null },
  "assets": [
    { "id": "clip.mp4", "kind": "video", "name": "test", "uri": "clip.mp4",
      "frame_count": 480, "timebase": { "num": 60, "den": 1 },
      "width": 1920, "height": 1080, "content_hash": null, "tags": {}, "note": "" }
  ],
  "timeline": {
    "schema": 3,
    "timebase": { "num": 60, "den": 1 },
    "markers": [],
    "tracks": [
      { "id": "v1", "kind": "video", "layers": [
        { "id": "main-0", "start": 0, "end": 480,
          "source": { "asset_id": "clip.mp4", "source_in": 0 }, "effects": [] } ] },
      { "id": "ov-0", "kind": "video", "layers": [
        { "id": "overlay-0", "start": 30, "end": 126,
          "effects": [ { "kind": "overlay", "params": {
            "amount": 0.16,
            "r": 0.0784, "g": 0.0392, "b": 0.1569,
            "r2": 0.1569, "g2": 0.0784, "b2": 0.2353,
            "shape": 1, "angle": 0 } } ] } ] }
    ]
  },
  "view": { "playhead": 0, "selection": null, "zoom": 1 },
  "render_hints": { "width": 1920, "height": 1080, "format": "mp4" },
  "extensions": {}
}
```

**第 2 步**：出那一帧（60fps 时间基 ⇒ 帧 60 = 1.0s，落在窗口 30..126 内）

```bash
dhampir frame --project /tmp/ov.doc.json --frame 60 --out /tmp/f60/ --asset-root /tmp
```

**第 3 步**：期望 vs 实际

```text
期望：画面叠一层 #140A28(≈0.078/0.039/0.157) → #28143C(0.157/0.078/0.235) 的线性渐变，
      强度 = amount × Effect.opacity 默认 1.0 = 0.16，应该**明显可见**
实际：**与"把 ov-0 那条轨删掉"逐像素相同**（没有任何色罩）
```

### 已核对过（说明不是"调用方传错字段"）

| 项 | 事实 |
|---|---|
| 效果名与参数 | `kind:"overlay"` 与 `amount/r/g/b/r2/g2/b2/shape/angle` **就是底座自己映射代码读的那几个字段**（§「可疑点」第 1 条给了行号），也与它自己的单测形状一致（同文件单测用 `effect("overlay", &[("amount",1.0),("angle",180.0),("shape",1.0)])`） |
| 图层形态 | 调整层：**无 `source`**、只有 `effects`，挂在视频层之后 |
| 窗口 | `start=30, end=126`（覆盖被取的那一帧）✓ |
| 强度 | `Effect.opacity` 缺省 1.0 ⇒ `weight = 0.16 > 0` ✓ |
| 最小化 | 上面的 JSON **已经把"逐帧包络不生效"这个可能性排除了**（这条层没有 `keyframes`，强度只来自 `amount`） |
| 是否只有 overlay 一支 | ⚠️ 未测：`flash` / `vignette` / `noise` 走的是同一族调整层 —— 如果它们**也不出**，那是同一条根因；如果它们**出**，说明只差 overlay 这一支。**建议复现时顺手把 `kind` 换成 `flash` 各出一条对比**（这一步能直接二分掉一半范围） |

### 可疑点（本次只读代码得到的线索，未验证）

1. **调整层的判定链**：`Layer::is_adjustment()` = `source.is_none() && !effects.is_empty()`
   （`crates/dhampir-timeline/src/layer.rs:235`）⇒ `compose.rs:329` 传给渲染层 ⇒
   `plan_steps()` 用它决定产 `Step::Adjust`（`crates/dhampir-core/src/render/timeline.rs:1415`，第 1420 行 `if layer.is_adjustment`）。
   **先确认这条链在你的复现里到底走没走到 `Step::Adjust`**（打一行日志/单测即可）。
2. **没有源=被 `continue` 掉**：`compose_layers` 里
   `let Some((view, size)) = resolver.texture_for(&layer.source, …) else { continue };`
   （同文件 ~1005 行）—— 如果那条调整层被判成 `Draw` 而不是 `Adjust`，它就会**静默消失**在这里。
3. **`Step::Adjust` 分支自己的前置条件**：`Step::Adjust { … } => { let Some(from) = current else { continue }; … }`
   （同文件 771–772 行）—— 如果这一帧上 `current` 还是 `None`（前面没有 Draw 过东西），这一层会被跳过。
4. **空间（space）筛选**：`apply_stage` 里 `color_mask_params(batch, space.target, frame)`
   （同文件 902 行）；`overlay` 的特效登记里 `space` 是 **Document**（`crates/dhampir-core/src/effects.rs` 的 `OVERLAY` 定义），
   而 `space.target` 是按 pass 传进来的 —— 确认 **Document 那一趟有没有跑到这条层**。
   （`vignette` 也是 Document，所以第 1 条那个"换个 kind 对比"同样能验证这一支。）

### 验收判据（不需要 下游）

```bash
# 出同一帧两次：带 overlay 层 / 把它删掉，然后比"平均绝对差"
ffmpeg -v info -i /tmp/f60/with.png -i /tmp/f60/without.png \
  -lavfi "[0][1]psnr" -f null -          # psnr 的摘要行是 info 级，别用 -v error
# 期望：不是 inf（两帧不同）；且差异集中在 overlay 的窗口帧 30..126 内，
#       窗口外（例如帧 10）两次出图应当**逐字节相同**
```

**请不要这样修**：不要在 下游的转译器里"用别的效果凑一个色罩" —— 那会掩盖问题，
底座修好后还会变成第二份实现。转译侧现在已经把它**正确发出**（报告里没有 DROP）。

---

## 2. D2（小）`--from` 单独给出时，注释与代码不一致

**位置**：`crates/dhampir-worker/src/bin/dhampir.rs::frame_range`（~1170 行）。

注释（1186–1192 行）写的是：

```text
* 只给 `--from 10`：“从第 10 帧起，到工程结尾”；
* 只给 `--to 5`：“从第 0 帧到第 5 帧”。
```

而代码（1193–1194 行）是：

```rust
let from = args.from.unwrap_or(0);
let to = args.to.unwrap_or(from);   // ← 只给 --from 10 时 to = 10 ⇒ 只出第 10 帧
```

**影响**：信注释的调用方会拿到"一帧"而不是"到结尾"，**退出码还是 0**（静默出错）。
（下游两头都给 `--from 0 --to N-1`，所以没踩到。）

**⚠️ 修之前注意**：`frame_range(args)` 只拿得到 `args`，而 `cmd_frame` 是**先调它、后 load 工程**
⇒ 想让 `to` 缺省成"工程结尾"，要么把工程加载提前、要么把返回值改成 `Option<Frame>` 由调用点补。
**另一条同样合法的路是改注释**（让文档与实现一致）—— 两条选一条，别只改一半。

---

## 3. D3（小）`DanmakuSpec.duration_ms` 的模块头注释

**位置**：`crates/dhampir-timeline/src/danmaku.rs`（模块头 ~15 行、实现 ~128/142/151/229 行）。

**本次核对**：文件里已经有多处"这条先前写错了…"的更正注释，
实现是 `let travel_ms = cue.travel_ms.unwrap_or(spec.duration_ms);`（151 行）⇒
**"素材里的 `End` 不参与、`\move`/`duration_ms` 说了算"这个口径与实现看起来一致**。
⇒ 判定为**已自行修正**，只需确认模块头那句是否也同步过；若仍觉得矛盾，请以 151 行实现为事实。

---

## 4. B 组：量化出来的不足（这几条来自下游侧的历史读数，需底座侧自测确认）

> 这几条**没有**像 D1 那样的最小复现，因为它们是"观感/精度"类差异。
> 每条给了：现象、下游侧读数、底座侧建议的自测方法。

| # | 现象 | 下游侧读数（历史） | 底座侧建议自测 |
|---|---|---|---|
| **B1** | `stage` fit 的 **contain（摆位）+ 背景层（模糊底）** 没做 | 竖屏画布：只改 `orientation` 一行，基线像素差 **87.98**、位移 −24px；下游转译器补了 cover 的 `f` 之后 → **44.20** / **+2px** | 用 1920×1080 源导出 1080×1920：主画面是否被 **cover 裁切铺满**（无黑边）；底座侧是否已有 `stage`/背景层概念（本次**没查**，只是记成"没做"） |
| **B2** | 放大**重采样偏软** | 手写双线性放大 1.7778x，比浏览器 `drawImage` **软 2.5 倍**（同一帧逐像素比） | 造一张高频细节图，放大 1.7778 倍后比高频能量（拉普拉斯方差）或直接与浏览器 `drawImage` 比 |
| **B3** | 高亮词**段间水平推进量**不同源 | 残差：段间推进量两端不同 —— 浏览器 `measureText` 与画字同引擎（准），`drawtext` 拿不到别段宽（用布局逻辑字宽，中英混排差几像素） | 造一条含 `<span class="hl">` 的 srt，量"高亮段之后那一段"的起始 x |
| **B4** | 弹幕 **`\pos` 静态 cue** 仍走轨道级 travel | 参照是**逐条** travel；底座在 cue 有 `\move` 时已能逐条（6 参数 `\move` 那条路已通），**静态 cue 没有 `\move` 可读** ⇒ 回退轨道级平均 | 造一条只有 `\pos` 没有 `\move` 的 cue，看它在屏时长是"逐条"还是"轨道级平均" |
| **B5** | 字幕**文字阴影**在契约里没有字段 | 下游侧对应的是 DROP 之一（`style.subtitle` 的文字阴影 + 入场动画 `cycle-5`） | 纯契约问题：要不要给字幕轨加阴影字段由底座定；不扩我们就一直如实报 DROP（**不是 bug**） |

---

## 5. 修好之后下游侧会做什么（联动约定）

1. **D1 修好后**：先跑 下游的对拍三样本复测（`demo` / `portrait` / `real`）——
   overlay 色罩会真的出现 ⇒ **基线数字必然变化，那是期望的变化**，我们会重新标定；
   然后划掉台账里的 D1，并把下游仓集成文档 §4.8 的对应行改成"已接"。
2. **D1 修好后顺带能验的**：下游的 `overlay.angle`（D5）—— 我们已按底座约定
   `base = css − 90`（CSS 0° = 向上、顺时针；底座 `dir=(cos,sin)` 0° = 向右），缺省 `135°`（= 参照的 `to bottom right`）。
   请顺手确认一条：**`angle=45` 时渐变应当是"从左上到右下"**（我们按这个换算）。
3. **D2 修好后**：下游**不改代码**（我们两头都给参数），只会核对一下文档措辞。
4. **B1/B2 若修**：竖屏类工程的观感会整体变好；下游侧会把"重采样软 2.5 倍"那条从台账划掉。

## 6. 溯源（只想追证据时才看；不需要读下游仓）

- 本交接单的原始版：下游仓 `docs/dhampir-base-issues.md`（D1–D3 的初版复现）；
- 状态表：下游仓 `docs/plan/dhampir-parity-register.md` §四；
- B 组的读数出处：下游仓的集成文档 §六（第 3/4/5 条）与 §4.8；
- D1 的最小 doc 原始文件（含 `keyframes` 包络那一版）：下游仓 `.workbuddy/dhampir-unify-out/mini-overlay.doc.json`。

---

## 7. 本仓侧的核对结果（2026-10-01，Dhampir 仓内独立完成）

> 全部读数都在本仓、用本仓的 `target/s3/proxy1080p.mp4`（1920×1080 / 60fps / 480 帧 /
> 关键帧每 60 帧，`node scripts/make-test-media.mjs` 生成）量出来的，**不依赖下游仓**。
>
> **一条命令重跑**（已经是仓库里的正式工具，不再是草稿）：
>
> ```bash
> node scripts/handoff-probe.mjs              # 三支全跑：color-mask(D1) / fit(B1) / upscale(B2)
> node scripts/handoff-probe.mjs color-mask   # 只跑一支
> node scripts/handoff-probe.mjs --self-test  # 自检（不碰 GPU / ffmpeg / 不写盘）
> ```
>
> 它按本仓纪律来：`spawn-tool.mjs`（不喂 stdin）、`--self-test`、退出码 **0/1/2**、
> 前置缺失（CLI / 素材 / ffmpeg）**明确报出来**而不是退化成"没量"。

### 7.1 D1：复现不出来，且这一族现在被两条真机测试钉住

| 变体（同一条调整层，只换 `kind`） | space | 帧 60 vs 基线 | 窗口外帧 10 |
|---|---|---|---|
| 无调整层（基线） | — | `473DBBAD…` | `155FBD8E…` |
| `overlay`（交接单 §1 的原始参数，连"无 `frame_count`、层到 720"那版也测了） | Document | **`8CA691B7…`（不同）** | 逐字节相同 ✓ |
| `overlay` `angle=45` | Document | `203,3,6 / … / 23,203,207`（**左上 color_a、右下 color_b**） | 相同 ✓ |
| `flash` | Source | 不同 ✓ | 相同 ✓ |
| `vignette`（`radius=1.0`） | Document | **与基线相同** —— 但那是**参数定义使然**：归一化对角距离只有 0.707，半径 1.0 时按定义就没有暗角 | 相同 ✓ |
| `vignette`（`radius=0.2, softness=0.3`） | Document | 角落压到 0、中心不动 ✓ | 相同 ✓ |

**逐像素对上解析值**（`overlay`，`amount=0.16`、`color_a=(0.0784,0.0392,0.1569)`、`color_b=(0.1569,0.0784,0.2353)`）：

```
左端 实得 (203, 3, 7)   解析 (203.1, 3.3, 6.4)
右端 实得 (6, 217, 224) 解析 (6.4, 217.4, 223.8)
```

**新补的两条测试**（`crates/dhampir-worker/tests/timeline.rs`，都要真 GPU，默认 `#[ignore]`）：

```bash
cargo test -p dhampir-worker --test timeline -- --ignored
# 调整图层的_overlay_逐像素对上解析渐变   ok   ← 含 angle=0 左右两端与 angle=45 左上/右下四个方向判据
# 调整图层的_vignette_中心不动而角落压暗   ok
```

判据是**解析值**（把 `color_mask.wgsl` 的数学抄成可算的期望），不是"跟上一版的图一样" ——
后者只能证明"没变"，证明不了"对"。**写这两条时先抓到了我自己的一处单位错**（0..1 的契约色 vs 0..255 的读回值），
是 GPU 给的值把我纠正过来的 —— 这也说明这条判据真的在算东西。
**`angle=45`（§5.2 问的那条）已确认：从左上（color_a）到右下（color_b）** ✓。

### 7.2 D2：已修（实现改成与承诺一致）

承诺在三处：`docs/api.md:48`、`docs/usage.md:65`、`dhampir --help` —— 都写"只给 `--from 3` 是从第 3 帧**到结尾**"，
而实现是 `to = args.to.unwrap_or(from)`（只出 1 帧、退出码 0）。**改的是实现**：

* `frame_range` 拆成 **`frame_spec`（只判参数层：互斥 / 至少给一头）+ `resolve_frames`（纯函数：缺的那一头按工程补）**；
  补的口径与既有 `resolve_range` 一致（`compose::end_frame_v2` 的那一头，左闭右开 ⇒ 最后一帧 = `end-1`）——
  **同一条规矩不搞两份实现**；
* `cmd_frame` 变成"先判参数、再载工程、再算帧号"（因为"到结尾"必须先知道工程有多长）。

实测（视频层 `0..480`、素材 480 帧）：

| 命令 | 结果 |
|---|---|
| `frame --from 470` | **10 张**（470..=479）退出码 0 —— "到工程结尾" ✓ |
| `frame --to 2` | 3 张（0..=2）不变 ✓ |
| `frame --from 500`（起点越过结尾） | 退 **2**，报"帧区间是空的：from=500 to=479" ✓（不许"一帧不出但退 0"） |
| 都不给 | 退 2 ✓（承诺不变） |

守卫侧加了第 33 条判据 `frame-from-only-to-end`（`scripts/check-cli.mjs`），
并**变异验证过它会红**：把 `resolve_frames` 换回旧行为 → `32/33`，
红点原文 `frame-from-only-to-end：exit=0 帧数=none 落盘=1/3（工程 end=90，--from 87）`。

### 7.3 D3：模块头已按实现补全

实现是 `let travel_ms = cue.travel_ms.unwrap_or(spec.duration_ms);`（`danmaku.rs:157`），
而 `travel_ms` 的唯一来源是素材里 `\move(x1,y1,x2,y2,t1,t2)` 的 `t2-t1`（`subtitle.rs:35/316`）。
原来的模块头只写"由 `duration_ms` 说了算"，**漏了"逐条优先"这一半**。已改成：

> 在屏时长取**它自己的** `travel_ms`；读不到才回退到轨道级的 `duration_ms`；素材的 `End` **始终不参与**。
> 静态 cue（只有 `\pos`）读不到 `travel_ms`，于是回退到轨道级 —— 两条路都合法，但读数的人得知道自己在哪条。

### 7.4 B1：已量清；cover 用现成 `transform.scale` 就能表达，底座不需要新概念

```
1920×1080 源 → 1080×1920 画布（scale=1.0）：透明像素 43.75%（顶边整行透明、左边 840/1920 透明）⇒ 是 contain
同一份工程 + transform.scale = 1.7778（= 1920/1080）：透明像素 0 ⇒ 铺满（cover）✓
```

* **cover**：纯几何，`transform.scale` 就能表达（上面第二行是实拍读数）；谁算这个倍数（转译器算法）是下游的事；
* **模糊底**：用现成原语三步就能表达 —— `[同源层(cover)] → [调整层 gaussian_blur] → [主画面层]`
  （调整层只影响它**下面**的，所以主画面不会被糊）；底座这里没有缺概念；
* 因此 §7 的边界判定适用：**"这个直播间怎么排版"归转译器**，底座只承诺"像素怎么算出来"。

### 7.5 B2：不复现，而且方向相反

```
1 像素棋盘格放大 1.7778x（3413×1920）后的"插值中间值"占比：
  底座（--width 3413）  100.0%
  ffmpeg flags=bilinear 100.0%
  ffmpeg flags=neighbor   0.0%      ⇒ 底座做的是**真双线性**，不是最近邻搬用
```

⚠️ **修正一条我上一版写过的结论**：我起初拿"高频能量（拉普拉斯方差）"当判据，报了
"比 ffmpeg 双线性高 2.59×"。把探针脚本化之后发现 **这个量是内容相关的** ——
同一个实现、同一个放大倍数：testsrc2（照片类）上 **2.59×**，1 像素棋盘上 **0.003×**。
**自相矛盾 ⇒ 它不能当"软/锐"的判据**，只能是诊断量。现在 `handoff-probe.mjs` 里它**只打印、不计入判据**，
能站住的判据只有上面那条（中间值占比 ⇒ 双线性 vs 最近邻）。

结论不变：下游侧那条"比 `drawImage` 软 2.5 倍"**在今天的底座上不成立**（要么来自旧版本，
要么量的是预览 canvas 那条路）。**若下游仍见"软"，请给一条能复现的最小 case（源、输出尺寸、比对对象）** ——
按现在这条判据，底座放大就是双线性。

### 7.6 B3：已知残差（本仓文档早已写明，不是新发现）

`docs/usage.md` §"两端各有一处已知残差" + `crates/dhampir-worker/src/text_overlay.rs` 的
"# 行内偏移怎么来的（**这里是本仓与参照的一处已知残差**）"两块都写着：浏览器 `ctx.measureText`
量字与画字同引擎（准），而出片那一路的 `drawtext` 拿不到别段宽度，只能用共享布局的逻辑字宽，
**中英混排时差几像素**。要抹平得让出片侧也拿到同源度量 —— 那是另一个项目量级的事，不是本轮的修法。

### 7.7 B4：已知残差；**转译侧有现成解，底座不用改**

`docs/usage.md:167` 已经写明"`\pos(x,y)` 那种静态 cue 没有 `\move` 可读，仍走轨道级"。
底座这边的通道是**素材里的 `\move` 6 参数**（只读 `t2-t1`，坐标不参与落点）。
所以转译器只要给静态 cue **合成一个 `\move(0,0,0,0,0,travel_ms)`**，那一条就变成"逐条"了
（落点仍由底座按 spec 算，坐标写什么不影响画面）。这样既拿到逐条，也不给底座加"猜"的口子。

### 7.8 B5：建议**暂不扩契约**（请底座这边拍）

文字阴影是观感项；下游已经能如实报 DROP；扩了要两端各实现一遍、还要重跑"既有工程逐字节不变"那几条不变量。
**建议的触发条件**：出现"这一项必须与参照像素级对齐"的验收需求时再扩（那时连字段形状一起定：
`shadow_dx/dy_px`（文档像素）+ `shadow_blur_ratio` + `shadow_color`，与 `stroke_*` 同族命名）。

### 7.9 D4（本仓新发现）：`shape=0` 的纯色取的是第二组颜色

把探针脚本化的时候，自检按"纯色应当取 `r/g/b`"写，**当场红了**。查 `color_mask.wgsl`：

```wgsl
let grad_t = is_solid + is_linear * linear_t + is_radial * radial_t;   // shape=0 ⇒ grad_t = 1
let overlay_color = mix(color_a, color_b, clamp(grad_t, 0.0, 1.0));    // ⇒ 取的是 color_b！
```

而 `OVERLAY` 的文档（`crates/dhampir-core/src/effects.rs`）写的是
"**shape** 0=纯色 1=线性渐变 2=径向渐变"、"**渐变的第二个颜色**用 `r2/g2/b2`" ——
按这份文档，纯色那一路 `r2` 本不该参与。**现在它参与了。**

**为什么至今没出事**：参数打包里 `overlay_r2 = param("r2").unwrap_or(r)`（`r2` 缺省回落到 `r`），
所以"只给 `r/g/b` + `shape=0`"的工程结论是对的；只有**同时给了 `r2` 且与 `r` 不同**时才露出来。

**两种改法**（都是一行，选一条，别改一半）：

| 改法 | 内容 | 代价 |
|---|---|---|
| **改实现**（**已选这条** ✅） | `grad_t` 去掉 `is_solid` 那一项（纯色 ⇒ `grad_t=0` ⇒ 取 `color_a`） | 改行为：纯色 + 显式 `r2` 的工程画面会变 —— **但实测没有任何这样的工程**（见下） |
| 改文档（未采用） | 把 `OVERLAY` 的文档改成"纯色取 `r2/g2/b2`（`r2` 缺省 = `r`）" | 零行为变更，但契约从此多一条"看第二组颜色"的怪规矩 |

**定案（2026-10-01）**：走"改实现"。**判据是证据，不是偏好** ——
读下游转译器 `F:\para\Code\下游\tools\polish-to-dhampir.mjs` 的 `solidEffect`（约 1990 行）：

```js
r: col.r, g: col.g, b: col.b,
r2: col.r, g2: col.g, b2: col.b,     // ← 纯色把两组写成一样
shape: 0,                             // OverlayShape::Solid
```

⇒ 修正前后**画面完全一致**（`r2 == r` 时 `mix(color_a, color_b, t)` 与 `color_a` 同值）。
所以这条修的是"实现与文档不一致"，**不是**改契约语义。

落点与钉子：
* `crates/dhampir-core/src/shaders/color_mask.wgsl`：`grad_t` 去掉 `is_solid`（并把注释写成"为什么以前会取到 `color_b`"）；
* `crates/dhampir-worker/tests/timeline.rs` 的 `调整图层的_overlay_逐像素对上解析渐变` 里加了一条**专门用 `r2≠r`** 的纯色判据（`r2 == r` 时两种实现看不出区别，那正是它活了这么久的原因）；
* `scripts/handoff-probe.mjs --self-test` 按**修好之后**的语义钉住这条（自检里那行注释写明了它以前的行为）；
* 实测：真机 GPU 用例 **6/6 ok**、探针自检 **10/10**、`color-mask` 判据 **12/12**。
