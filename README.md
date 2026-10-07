# dhampir

**渲染与预览的共用底座**——同一个 `dhampir-core` 编译到两个宿主：

| | |
|---|---|
| 底座 | **Rust + wgpu** |
| preview 侧 | **wasm32** 宿主（浏览器 WebGPU + WebCodecs） |
| render 侧 | **native** 宿主（headless wgpu） |

目标形态：素材存在服务器，渲染导出由服务器处理；**浏览器只做剪辑编辑处理**。
底座的核心命题只有一句：

> 同一份工程，在浏览器里看到的和服务器上导出的，是**可比的帧**。

**本仓库不做**（那是下游工程的形态）：API 网关 / 任务队列 / 对象存储 / 部署与容器化 /
分布式分片编排 / 编辑 UI 产品化。

> **范围**：本项目**只做本地开发与本地验证** —— 不发布到 crates.io、不接远端 CI。
> **但发 GitHub Release**：产物 zip 是下游宿主钉固依赖的那个东西（见下「版本与发布」）。

---

## 版本与发布

**当前发布版本：`0.2.0`**（唯一真相 = 根 `Cargo.toml` 的 `[workspace.package] version`）。

产物名 = **`dhampir-<产品版本>+<git sha>`**，另出一份**不带 sha 的稳定名**
`dhampir-<产品版本>-<平台>.zip` + 同名 `.sha256.txt` —— **下载地址写稳定名那个**。

⚠️ **两个"版本"是两回事，别混**（2026-10-03 拆开）：

| | 是什么 | 谁在用 |
|---|---|---|
| `version` | **产品版本**（0.2.0） | 人读、产物名、Release 资产名 |
| `project_schema` | 工程文件**能不能读**（1） | 底座拿它拒错版工程 |
| `host_api` | wasm **导出面对不对得上**（6） | `dhampir_host_api_version()`，只增不减 |

在此之前产物名叫 `dhampir-<schema>+<sha>` —— 名字里那个号是 **schema 版本**，而 schema
在兼容变更时**根本不动**，于是一堆内容不同的产物共用一个名字，"下载地址该写哪个"没有答案。

⚠️ **`0.2.0` 是"契约号没动、产物字节变了"的一次发布**：字幕栅格化器从 `drawtext` 换成 libass，
所以**老工程的字幕像素会变**。下游若做过视觉基线/截图回归，**必须重拍基线**（口径不用改）。
完整清单见 [CHANGELOG.md](CHANGELOG.md) 的 0.2.0 段。

发布流程：

```bash
# 1. 改 [workspace.package] version → 2. 打包（产出稳定名 + sha256 侧车）
node scripts/package.mjs
# 3. 打 tag（tag 名与产品版本对齐）
git tag -a v0.2.0 -m 'dhampir v0.2.0' && git push origin main --tags
# 4. 挂 Release：dist/dhampir-0.2.0-win32-x64.zip + .sha256.txt
```

---

## 从这里开始

**第一次来，从 [docs/quickstart.md](docs/quickstart.md) 开始** —— 五分钟能看见画面、拿到 mp4。

| 你想 | 去哪 |
|---|---|
| **五分钟跑通**（装什么、出片、开预览） | **[docs/quickstart.md](docs/quickstart.md)** |
| **怎么用它**（全部开关、每种用法、常见问题） | **[docs/usage.md](docs/usage.md)** |
| **调用面**（承诺等级、CLI、wasm、HTTP） | **[docs/api.md](docs/api.md)** |
| wasm 导出全名单（**生成的**） | [docs/api-surface.md](docs/api-surface.md) |
| **宿主返回体的形状与版本** | [docs/host-api.md](docs/host-api.md) |
| **从上一版升上来要改什么**（下游宿主看这份） | **[docs/upgrade-0.1.0-to-0.2.0.md](docs/upgrade-0.1.0-to-0.2.0.md)** |
| 现在做到哪了、还剩什么 | [plan/next-steps.md](plan/next-steps.md) |
| 缺陷与架构缺失的台账 | [plan/defects.md](plan/defects.md) |
| 阶段计划与依赖顺序 | [plan/roadmap.md](plan/roadmap.md) |
| 为什么这样设计（决策真相） | [plan/video-editor-plan.md](plan/video-editor-plan.md)、[plan/video-editor-tech-guide.md](plan/video-editor-tech-guide.md) |
| 历史交接记录 | [plan/remaining-work.md](plan/remaining-work.md) |
| **下游怎么接**（转译器、缺口表、对比回路） | 下游仓的 `docs/dhampir/` —— 本仓不存放下游方言的内容 |

**四份文档的分工**（别把它们混成一份）：

- **quickstart** = 最短的路，只讲"怎么做"；
- **usage** = 全部开关与用法，任务导向；
- **api** = 承诺边界与入口地图（**先读承诺等级那张表**）；
- **api-surface / host-api** = 机器生成的名单与形状，是**真值**，不手改。

**改决策要改文档，不能只在代码里改。**

### 最短的一次「跑通」

```bash
cargo test --workspace                     # native 全量
node scripts/run-guards.mjs                # 20 条守卫全绿
cargo run -q -p dhampir-worker --bin dhampir -- --help
```

---

## 名字

引擎名 **`dhampir`**：crate 名、包名、CLI、文档一律用它，不再引入第二套命名。

「dhampir」是英语里半人半吸血鬼的存在——白天一侧（浏览器/预览）与夜晚一侧
（服务端/出片）同源异形，且两边都得能活。词源到此为止，不要往外延伸。

---

## crate 地图与依赖方向

```
        dhampir-timeline          （纯数据：整数帧号 / 有理数时间基 / 时间码）
           ↑          ↑
   dhampir-media   dhampir-core    （CPU 编解码契约 ‖ 渲染图 + WGSL）
           ↑          ↑             ← 两者是**兄弟**，互不依赖
           └────┬─────┘
        dhampir-wasm   dhampir-worker
       （wasm32 宿主）    （native 宿主）   ← 两个宿主互不依赖
```

几条硬规则：

- **单向无环**。`media` 依赖 `timeline` 不是笔误：`VideoInfo` 的时间基必须与时间轴
  共用同一个 `Timebase`，各层各写一份有理数定义，迟早在 `30000/1001` 上分叉。
- **`dhampir-core` 里不允许出现任何 `#[cfg]`**（`#[cfg(test)]` 除外）。
  平台差异用 Cargo 的 target-specific 依赖表达，不在代码里堆条件编译。
  理由：core 是两个宿主唯一的公共资产，它一有条件编译，"同一份源码"就不再是事实。
- **分叉只有两处**：`Instance` 的后端选择（只在两个宿主里），以及 `core::io` 的
  `FrameSource` / `FrameSink` 实现。
- 上面每一条都有守卫脚本盯着，不靠自觉（见「验证」）。

### 五条铁律

1. **时间用整数帧号，不用浮点秒**。
2. **帧率是有理数**：29.97 是 `30000/1001`，不是 `29.97`。
3. **特效是「类型 + 参数」的声明式数据**，不是代码。
4. **始终按 WebGPU 的能力下限写**，不为某个后端开后门（否则两端等值当场失效）。
5. **SSIM 必须设容差**，且容差表要单独成文件、可被引用——判断"是否回归"的唯一依据。

---

## 现在到哪了

**T0–T7 八段全部收口，台账零待办**（26 条：done 20 / todo 0 / wontfix 5 / unmeasurable 1）。

| 段 | 内容 | 状态 |
|---|---|---|
| **T0** | 可观测性 + 缺陷台账 + 预览判定回传通道 | ✅ |
| **T1** | 文档坐标系（预览所见 = 成片所得） | ✅ |
| **T2** | 文字与字幕上屏（两个宿主） | ✅ |
| **T3** | 弹幕（共享泳道、两端结构一致） | ✅ |
| **T4** | 编辑模型补完（撤销/重做、曲线、拖拽吸附） | ✅ |
| **T5** | 解码与素材通路（倒放、同源多帧、并发） | ✅ |
| **T6** | 音频（AudioPlan、音轨接通、同步口径） | ✅ |
| **T7** | 交付面收口（具名子命令、陈旧 pkg、Linux 守卫） | ✅ |

更早的里程碑（**M0** 骨架与双编译、**M1** headless 基线、**M2** 双运行时同帧 SSIM）已收官，
证据在 [`records/`](records/)。

**一句实话**：现在最接近「能用」的那条路是 **CLI 单机出片**——
预览能编辑、能拖拽、能撤销重做、能出片，字幕与弹幕两个宿主都画得出来，
素材通路不怕倒放与同源多帧，**片子现在也带声音了**。

### 还差什么（诚实的边界，不是待办）

1. ~~"GPU 渲染的 90 帧 + 音轨整条出片"没验过~~ —— **2026-10-01 在 Linux 上验过了**：
   容器里 `node scripts/check-cli.mjs` **33 / 33**、`node scripts/check-local-backend.mjs` **25 / 25**，
   整条出片腿（含往编码器 stdin 写帧）真的跑通。
2. ~~Linux"能跑"没验过~~ —— **2026-10-01 在 Linux 上跑过了**：`cargo check --workspace --all-targets`
   **0 warning**、`cargo test --workspace` **686 passed / 1 failed / 30 ignored**（失败那条是**真缺陷**，
   已修：`asset.uri` 的绝对判定不能按平台语义）、**30 条 ignored 测试全绿**、M1 的两条 Linux 腿
   （RADV + lavapipe）归档进 [`records/m1/`](records/m1/README.md)。**仍未验**：别的发行版、别的 GPU。
3. **解码代价表与 14.56 ms/帧 未对账**（40.90 对 14.56）——**这个前置现在有了**：Linux 容器里有 ffmpeg，
   只等有人跑 `node scripts/measure-export.mjs`。

另外 **A9（续渲）是"决策为不做"**，不是"做完了"——理由与重审触发条件见
[plan/t7-evidence.md](plan/t7-evidence.md) 的 T7.3。

---

## 验证

### 守卫脚本

一共 **20 条**（`scripts/` 下，各带自己的 `--self-test`），用 `node scripts/run-guards.mjs` 全跑。
每个守卫都被**反向验证**过（临时植入违例，确认它真的会红）。**守卫若不会红，就不是守卫。**

CI 里跑的是其中**不依赖本机资产**的三条：

```bash
node scripts/check-core-purity.mjs    # core 里没有 #[cfg] / cfg!（只扫去注释后的代码）
node scripts/check-dep-graph.mjs      # 依赖方向单向无环 + 纯层不碰平台 crate
node scripts/check-text-hygiene.mjs   # 全仓 LF + 无 BOM + 合法 UTF-8
```

三者的共同纪律：**不在空文件集上通过**。没有文件可查时退出码是 2，不是 0——
"没扫到"和"扫过了没问题"是两件事。

另外 **16 条**要本机上的东西（ffmpeg、GPU、真实浏览器、`records/` 里的取证存档），
所以只在开发机上跑，不进 CI —— 它们是：
`check-defects`、`check-sequential-decode`、`check-web-invariants`、`check-backend-seam`、
`check-preview-parity`、`check-overlay-plumbing`、`check-linux-portability`、`check-effect-registry`、
`check-m1-record`、`check-m2-record`、`check-media-status`、`check-local-backend`、
`api-surface`、`timeline-contract`、`check-cli`、`check-dual-end`。
**清单以 `scripts/run-guards.mjs --list` 为准**，不从这份文档抄。

**绿不绿以守卫自己的输出为准**：`records/` 里存的是当时的原始输出，不随代码走；
想知道今天绿不绿，跑一遍。

### 完整命令

```bash
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能用 --workspace —— dhampir-worker 是 native-only
cargo test --workspace                     # native 全量
node scripts/run-wasm-tests.mjs            # wasm32 运行时
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

### 留取证记录

```bash
node scripts/record-acceptance.mjs --milestone m0   # 也支持 m1 / m2
```

它按 `plan/` 里写的退出标准逐条跑，把**原始 stdout/stderr** 与每项退出码落进 `records/`。
**判定以退出码为准，不靠读日志下结论。**

---

## `records/` 是什么

里程碑证据。截图可以骗人，文件可以被人重新算一遍摘要，所以结论都要落成文件。
行尾不做任何转换：`.gitattributes` 的 `* text=auto eol=lf` 在仓库层面钉死 LF。
（**不要**给 `records/**` 标 `-text`：那会让 JSON 变成不可读的 Binary diff，
而记录就是要被人逐行看的。）每个里程碑目录各有一份自己的导览
（[`records/m0/README.md`](records/m0/README.md) 等），说明每份文件证明什么、怎么重跑。

---

## 目录结构

```
crates/
  dhampir-timeline/   帧号 ↔ 时间码（纯整数）、自检探针、golden 报告
  dhampir-media/      纯契约 trait（解码/编码接口，仍是零实现）
  dhampir-core/       渲染、GPU 抽象、读回、PNG 编码。零 #[cfg]
  dhampir-wasm/       浏览器宿主：wasm-bindgen 导出 + canvas + www/ 自检页
  dhampir-worker/     native 宿主：dhampir CLI（src/bin/）+ dhampir-render（src/main.rs）
web/                  浏览器剪辑预览（无打包器，直接 ES module）
docs/                 使用说明与调用面清单
plan/                 执行计划与决策真相（改决策要改这里）
records/              里程碑证据（要提交）
scripts/              守卫脚本、测量脚本、本地服务
```

---

## 许可证

**Apache-2.0** —— 全文见 [`LICENSE`](LICENSE)（与 [`LICENSE-APACHE`](LICENSE-APACHE)
**逐字节相同**的同一份：前者是 GitHub 许可证识别器认的标准名，后者是既有工具链与产物里
引用的名字，两个都留着）。

第三方依赖的许可清单是**生成的**（[`THIRD-PARTY-LICENSES.md`](THIRD-PARTY-LICENSES.md)，
由 `node scripts/licenses.mjs --check` 钉住不许漂）；分发时产物里会带上这两份。

**用它做什么都行**：Apache-2.0 允许商用、允许闭源集成，且自带专利授权（§3）——
下游不必为"用了这个引擎"承担额外的许可证义务，只需保留许可与 NOTICE。

> 2026-09-30 由 `MIT OR Apache-2.0` **收窄为 `Apache-2.0`**：此前没有发布过任何版本，
> 所以不存在"已按 MIT 授权出去的副本"。收窄给下游的是更明确的专利授权（Apache-2.0 §3）。
