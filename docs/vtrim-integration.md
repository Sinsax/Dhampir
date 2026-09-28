# V-Trim → Dhampir：**Dhampir 这一侧**的部分

> **这份文件只留 Dhampir 自己那两节。**
>
> 转译接口的正文（形状 / 快速开始 / 契约要点 / 行为对照表 / 三级记录 /
> 已知差异 / 怎么扩 / 要给回 V-Trim 的清单）**住在 V-Trim 那一侧**：
>
>     <V-Trim>/docs/vtrim-integration.md      （一 ~ 八节）
>
> 转译器本体与它的守卫也在那边：
>
>     <V-Trim>/tools/polish-to-dhampir.mjs
>     <V-Trim>/scripts/check-vtrim-translator.mjs
>
> **为什么不在这里留一份**：那八节讲的是 V-Trim 的事件语义，改一次要动两边，
> 而"两份并存的文档"一定会**静默漂移** —— 读的人不知道该信哪一份。
> 所以这里只保留**跨仓库的那一节（第九节，运行在 Dhampir 侧）**与边界声明。
>
> **本仓不保存转译器的任何副本**（工作树里没有、git 也不跟踪它）。
> 需要的时候**从 V-Trim 拿**，不改本仓的状态。

---

## 怎么从 V-Trim 拿（用的时候才拿）

**什么都不用复制。** `scripts/vtrim-compare.mjs` 自己会去找，顺序是：

| 优先级 | 来源 |
|---|---|
| 1 | `--translator <路径>` |
| 2 | 环境变量 `VTRIM_TRANSLATOR` |
| 3 | `<本仓>/../V-Trim/tools/polish-to-dhampir.mjs` |
| 4 | `<本仓>/../../V-Trim/tools/polish-to-dhampir.mjs` |

四个都找不到就**报错并列出找过哪些地方**（不静默退成"没有转译器"）。

所以两个仓并排放着（`.../Code/Dhampir` 与 `.../Code/V-Trim`）时**开箱即用**；
放别处就给它一个路径：

```bash
node scripts/vtrim-compare.mjs --clip "<片段目录>" --events fx.json \
  --translator /path/to/V-Trim/tools/polish-to-dhampir.mjs
```

**只想转译、不跑对比**时，直接跑 V-Trim 那份（它自带 CLI）：

```bash
node <V-Trim>/tools/polish-to-dhampir.mjs "<片段目录>" --out out/t.json
```

它用起来仍然是零依赖的 —— 转译器**不 import 本仓的任何东西**，
本仓也**不 import 它**。两个方向都没有代码依赖，只有"约定"。

---

## 九、对比回路：`scripts/vtrim-compare.mjs`

一条命令跑完"改 toml -> 两边渲 -> 按窗口出表 -> 恢复工程"：

```bash
node scripts/vtrim-compare.mjs --clip "<片段目录>" --events out/fx-events.json --out out/vtrim-run
```

`--events` 是一个 JSON 数组，描述**要插进 `polish.toml` 的测试事件**（**源时间**）：

```json
[
  { "type": "blur",    "time": 66.2 },
  { "type": "stutter", "time": 68.2, "intensity": 30 },
  { "type": "shake",   "time": 70.2, "intensity": 20 }
]
```

不传 `--events` 就是"只比现有工程"（那是**基线**，用来确认两边本来就一致）。

输出长这样：

```text
  基线（窗口外 6 处）：像素差 7.02   高频比 0.98

  特效            输出窗口          像素差   高频比   位移  判定
  blur@66.2      60.90-61.70      26.17    0.99   -21   ✗ 高于基线 位移-21px
  stutter@68.2   62.90-63.02      20.75    0.96    +8   ✗ 高于基线 位移+8px
  shake@70.2     64.90-65.10       9.93    0.97    -2   ✓
```

### 它把三个坑堵死了

这三条都是**我在手工搭这条回路时真踩过的**，而且**每一次都产出了"看起来有据"的假结论**：

| 坑 | 症状 | 脚本怎么防 |
|---|---|---|
| **甲：源时间当输出时间** | `mute_ranges` 有 17.49s 位移，按源时间去 `-ss` 量的是**不相干的时刻** —— 第一批结论整个是反的 | **先算映射再取样**，并把映射表打在最前面 |
| **乙：产物提前清理** | 追一个差异，回头要复核时基准已被自己删掉 | 运行目录**默认保留**，`--clean` 才删；报告里写明"结论要复核时还得看它" |
| **丙：两宿主的帧率不同** | `--to` 用参照的帧数（2907@30fps），而本仓是 60fps -> **只渲了一半** | `--to` 按**本仓自己的时间基**算，并跟工程帧数取小 |

另外整个流程包在 `try/finally` 里：**无论中途哪一步崩，`polish.toml` 都还原**
（已实测：跑完与跑前**逐字节相同**）。

自检：`node scripts/vtrim-compare.mjs --self-test`
—— 19 项，按**逐值**钉住上面甲/乙两个坑（映射要对多段 `mute_ranges` 逐点精确；
多个事件落到不同 segment 时行号不能串）。4 个变异里 3 个打红
（第 4 个是**无操作变异**，两个正则对那份输入行为相同 —— 不是自检的漏洞）。

### 依赖

`ffmpeg` / `ffprobe` 在 PATH 上、本仓的 `dhampir.exe`、V-Trim 的 `vtrim.exe`
（`--vtrim` 或 `VTRIM_EXE`，不传就找 `../V-Trim/.workbuddy/perf/bin/vtrim.exe`）、
以及**转译器**（`--translator` 或环境变量 `VTRIM_TRANSLATOR`，默认找 `<V-Trim>/tools/polish-to-dhampir.mjs`）。

**转译器路径可配**是有意的：它属于 V-Trim 那一侧（见第十节），
等它搬过去之后，这里传个路径就能继续用。

---

## 十、这条接口的边界

**转译器只读不写 V-Trim 的东西**：它读 `polish.toml` / `clip.srt` / `clip.dm.srt`，
把旁写文件放在**输出目录**里。

**它不承诺兼容**：它不是 `dhampir-core` 底座的一部分，形状可以随 V-Trim 演化而改。
底座承诺兼容的只有 `docs/api-surface.md` 里"底座 API"那一栏。

**它不做媒体探测**：`frame_count` 留空由 `dhampir probe` 回填 ——
转译要能在一台没有 ffmpeg 的机器上跑完并给出清单。
