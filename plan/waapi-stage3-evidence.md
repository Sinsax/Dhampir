# 阶段 3（HTML 宿主 / 第二个宿主）证据

> 上游：[plan/web-animation-parity.md](./web-animation-parity.md) 阶段 3；
> 口径：[plan/web-animation-criteria.md](./web-animation-criteria.md)（D1/D2/D3）。
> 本文件存**当时的原始读数**；想知道今天绿不绿，按下面的命令重跑。

## 1. 交付物

| 文件 | 是什么 |
|---|---|
| `web/anim-eval.mjs` | **网页那份求值**：CSS 缓动（关键字 / cubic-bezier / steps 四态）+ 通道插值（与 curve.rs 的四条规则逐条相同） |
| `web/dom-host.mjs` | **第二个宿主**：把工程文件摊成 DOM/CSS，接口名与 wasm 宿主同一套（`open / resize / sources_for / clear_bitmaps / set_bitmap / text_frame / draw / describe`） |
| `web/dom-host.html` | 让它**看得见**：帧号滑条 + 播放 + 每帧通道值读数 + 接口清单 |
| `crates/dhampir-worker/examples/eval_channels.rs` | 对照值来源：逐帧打出每层的五个通道（NDJSON） |
| `scripts/check-anim-eval.mjs` | 判据守卫（已登记进 `run-guards.mjs`），带 `--self-test` |
| `scripts/check-host-parity.mjs` | **形状判据**：DOM 宿主接口 ↔ `api-surface.md` 承诺过的导出名；带 `--self-test` |

## 2. 数值一致（这是本阶段**能验**的那一半）

```
node scripts/check-anim-eval.mjs
  对照 31 帧 × 最多 5 个通道，最大偏差 0.000009369659281333043（title.x@20），容差 0.0001
  ✓ 网页求值与 core 逐帧逐通道一致（含缓动、关键帧插值、步进与过冲）
  ✓ 区分度：把全部缓动换成 linear 会被抓住（133 条，最大偏差 35.95181528727214，是实测偏差的 3837046 倍）
```

**容差不是拍的，是量出来的**：实测最大偏差 **9.37e-6**（f32 的 core vs f64 的 JS，公式与迭代次数两边一致），
所以取 **1e-4**（比实测大一个数量级）。它够不够用由**区分度**那条回答：把所有缓动换成 linear（最典型的一种漂）
会带来 133 条超差、最大偏差 35.95 —— 是噪声底的 380 万倍。**容差松到这个程度仍然抓得住真漂移**，
而它要是抓不住了，守卫会直接红（区分度没了就是判据没了）。

## 3. 判据（守卫）

```
node scripts/check-anim-eval.mjs --self-test -> exit 0
  ✓ 自检：10 个变异被抓住，缓动与通道的边界也钉住了
node scripts/run-guards.mjs --list -> exit 0（对表通过）
node --check web/anim-eval.mjs web/dom-host.mjs scripts/check-anim-eval.mjs -> 0 / 0 / 0
```

自检覆盖：帧数不同、帧号不同、层数不同、层 id 不同、通道超容差、**容差之内不许红**；
以及缓动的边界：认不出的串必须抛（`bounce` / `linear(0,1)` / `cubic-bezier(2,0,0,1)` / `steps(0)`）、
`back_out` 必须冲过 1、`ease_in` 与 `ease-in` 不许被当成同一条曲线、中点必须用**后一个键**的缓动。

前置是 fail-closed 的：样本里只要有一层带 `transition_in`，判据直接判"前置不成立" ——
因为网页那份还没实现转场权重，混在一起就分不清"没实现"与"算错了"。

## 4. 这一阶段**没做**的（写下来，不假装）

1. **真浏览器里看一眼**：本会话起不了浏览器（见第 5 节），所以"HTML 侧能看到画面"这一条**由你确认**：
   打开 `web/dom-host.html`（file:// 下 fetch 会被拒 → 用 `scripts/dhampir-local.mjs` 起静态服务，或把 doc 粘进右侧文本框）。
2. 转场权重、素材帧换算、特效参数求值、文字布局 —— 网页那份都没实现（判据用前置把它们挡在对照之外）。
3. 锚点：DOM 宿主用"宽高一半"当中心近似；真正的锚点在引擎里（契约目前没有锚点概念）。
4. ~~接口形状的机器判据还没写~~ → **已补**：`scripts/check-host-parity.mjs`（已登记进 `run-guards.mjs`）。
   DOM 宿主的 8 个接口全部对应到 `docs/api-surface.md`「底座 API」段**承诺过的**导出名（35 个）；
   wasm 调用顺序的 7 步全部「实现或写明不做」；16 项「不做」都带**非占位**理由。
   读数：`node scripts/check-host-parity.mjs` → `✓ 宿主形状对表：…`；`--self-test` → 6 个变异全被抓。
   两个宿主的形状从此**不再是注释在保证**。

## 5. 环境读数（两条，都影响读数不影响代码）

**(a) 本会话起不了浏览器**：Chrome 报 `crashpad: OpenProcess 拒绝访问 (0x5)`，
离线试各种开关拿到根因那一行 `FATAL:mojo platform_channel.cc:112 Check failed: 拒绝访问 (0x5)` ——
**命名管道建不了**，与 `bash` 工具在本会话不可用是同一个原因。Firefox 也起不来（60 秒内零 HTTP 请求）。
所以 `target/easing-reference.json`（阶段 1-B 的浏览器真值）同样还没采集。

**(b) wasm pkg 陈旧，且本会话重建不了**：改了 `dhampir-timeline/src/{easing,schema,project,edit}.rs` 之后，
`node scripts/check-web-invariants.mjs` 会红一句"wasm pkg 比它的源码旧"（pkg 是 gitignore 的构建产物，判据是 mtime）。
本会话的处置与结果：

```
node scripts/stale-pkg.mjs --rebuild -> exit 1（spawnSync wasm-pack EPERM：它用管道起子进程）
wasm-pack build --dev --target web --out-dir www/pkg -> 编译成功（Finished dev profile in 32.70s），
  但后置步骤写 crates/dhampir-wasm/www/pkg 时被沙箱拒：Error: 拒绝访问。(os error 5)
```

结论：**这条红是环境造成的，不是代码缺陷**；它在普通终端里一条命令就消：

```
cd crates/dhampir-wasm && wasm-pack build --dev --target web --out-dir www/pkg
```

## 7. 能力登记表（本轮补：阶段 4/5 的前提）

**真值**：`web/capabilities.json`（23 条）。**判据**：`scripts/check-capabilities.mjs`（已登记进 `run-guards.mjs`）。

为什么要有它：阶段 4 是「设计完整度期」、阶段 5 是「能力逐条迁移」—— 两件事都需要一张**可判定**的表，
而不是脑子里的印象。表里每条能力一个状态：`supported` / `partial` / `needs-primitive` / `explicitly-not-doing`。

读数：

```
node scripts/check-capabilities.mjs
  ✓ 能力登记表：23 条（supported 7 / explicitly-not-doing 5 / partial 7 / needs-primitive 4）
node scripts/check-capabilities.mjs --self-test
  ✓ 自检：8 个变异被抓住，且未改动的输入是绿的
```

判据钉六条，其中两条是这张表**敢被信**的原因：

1. **`supported` 必须给出底座原语与判据证据** —— 说支持就得有依据，「我们大概支持吧」不算；
2. **证据里的每个路径必须真的存在** —— 证据不能指向不存在的文件。

还有一条专治「看不见的债」：**DOM 宿主源码里出现的受管 CSS 能力必须登记**。
受管词表（`transform / opacity / filter / backdropFilter / clipPath / maskImage / mixBlendMode /
boxShadow / borderRadius / textShadow`）是**写死在判据里**的，独立于登记表 —— 否则就成了循环论证：
只扫自己登记过的东西，永远都过。自检里有一发就是「往 DOM 宿主源码里塞一个没登记的能力」，它必须红。

同时把 `plan/web-animation-parity.md` 的 §8 从「抄一份表」改成**指向真值**：抄本会漂，
而这仓库里会漂的抄本最后都变成了假数。

## 8. 登记表后续（本文件之外的两轮）

登记表是活的，后面几轮把它从 23 条长到 26 条，并把口径一条条量出来：

| 轮次 | 做了什么 | 读数在哪 |
|---|---|---|
| 第 5 轮 | `filter.blur` 的 CSS 口径（`radius = 2 × CSS σ`）钉住 + 反向验证 | criteria 的 **D5** |
| 第 6 轮 | TS 开放串成例（`"linear" | … | (string & {})`），还掉阶段 1 埋的派生代价 | criteria 的 **D2**「已定：走 B」 |
| 第 7 轮 | `filter.color` **拆成四条**：contrast 等价、saturation 几乎等价、**brightness 不等价**（乘 vs 加）、**hue 矩阵不同源** | criteria 的 **D6** |

这几轮的共同点：量出来的是**口径**，而不是「看着差不多」。而且每条都写清了「还没钉的一半」——
例如转译器侧还没有滤镜，所以那张映射表目前没有判据。
| 第 8 轮 | `timing.direction` 从「明确不做」推到**能转**：镜像展开 + 三条限制明说；顺手修掉一个**静默少渲染**（早期只展开一遍） | criteria 的 **D7**；判据在 `check-waapi2doc` |
| 第 9 轮 | DOM 宿主能画特效：`web/dom-css.mjs` 只搬**能证明等价**的三条（对比度/模糊逐值等价、饱和度带近似说明），亮度与色相**拒绝并报出**；判据 `check-dom-css`（含最小 DOM 替身验接线 + 两个错误映射器必红） | criteria 的 **D8** |
| 第 10 轮 | 混合模式：DOM 侧 **9/9 可画**（`add` 用 `plus-lighter` 近似）、引擎 4/9 —— **预览首次比成片能画得多**；差额由跨文件判据盯着（`ENGINE_IMPLEMENTED_BLENDS` ↔ `layer.rs::is_implemented`） | criteria 的 **D9** |
| 第 11 轮 | 把 `needs-primitive` 的账分成三档（读回型 / 几何型 / 其他）—— 判据强制每条标明档位；`clip-path` 拆成圆角 / clip-path / mask 三条（28 条）；几何型的设计（圆角先行、SDF、无分支、三条判据）落成 D10 | criteria 的 **D10** |
| 第 12 轮 | **圆角**：契约字段已立（缺省 0 不写文件）、`probe` 报警告、**渲染层拒绝整帧**、DOM 侧先画出来（`border-radius`）；跨文件判据把「引擎还没实现」钉住。引擎那个无分支 SDF 是下一步 | criteria 的 **D10**（含落地进度与一条过程实话） |
| 第 13 轮 | **圆角做完**：合成着色器里的无分支 SDF + `corner` uniform；半径 0 的 PNG sha256 与基线一字不差；GPU 用例钉住「只切四个角」；WGSL 允许表先申报 `min(` 再用；三处（Rust/JS/登记表）一起翻 | criteria 的 **D10**（第 13 轮读数） |
| 第 14 轮 | 补上 D10 判据 ③ 欠的那条：**「允许的差异」台账**独立成文件（`scripts/dom-parity-differences.toml`，4 条：圆角边缘抗锯齿 / 饱和度权重取整 / add 的 plus-lighter / Lottie 明确接受），判据钉「没量过的不许写成数」与「每条都要被引用」 | criteria 的 **D6 / D9 / D10** 各挂了一条引用 |
| 第 15 轮 | **转译器侧补上 D5/D6/D9/D10 那笔债**：`filter:`（blur×2 / contrast / saturate 搬，brightness / hue-rotate 明说跳过）、`mix-blend-mode`（9 条 + plus-lighter 近似 + 引擎缺口警告）、`border-radius`（单一半径）都进工程；自检 10→**14** 个变异，含跨文件比对 layer.rs / dom-css / 转译器三处清单必须一致 | criteria 的「转译器侧」一节 |
| 第 16 轮 | **裁剪形状落地**：契约 `Layer.clip`（circle / ellipse / inset）、着色器里无分支形状分派、DOM 侧 `clip-path`；GPU 用例含**椭圆判别点**；没有裁剪时出片 sha256 与基线一字不差；`polygon`/`path` 单列成「明确不做」 | criteria 的 **D11** |
| 第 17 轮 | 裁剪形状的**转译器侧**：`circle/ellipse/inset` 按 CSS 规范展开简写、`at` 百分比 → 归一化比例、`polygon` 与百分比半径明说跳过；**契约口径改成归一化比例**（被转译器逼出来的：像素偏移需要图层尺寸，而转译器不知道）；自检 14→**17** 个变异 | criteria 的 **D11**（第 17 轮那节） |
| 第 18 轮 | **关键帧驱动的滤镜**接上：`effect.<i>.<param>` 通道（引擎本就有，是转译器之前整条跳过）；函数列表变了明说跳过；判据那条"只有五个通道"的白名单**太窄**，改成放行并校验下标/参数名；自检 17→**21** 个变异 | criteria 的 **D6**（转译器那节） |
| 第 19 轮 | 三份**设计**落地（D12 掩码纹理 / D13 读回型 / D14 矢量渲染器），把「要新原语」从一句话欠着变成**必须指向真存在的设计节**（新判据 + 两条自检变异）；顺手修掉一条自检的**第二次空转**（写死的 token 又登记了 → 改成运行时挑未登记的，挑不到就判红） | criteria 的 **D12 / D13 / D14**；`check-capabilities` 自检 10→**12** |
| 第 20 轮 | `Layer.mask` 契约 + 校验 + 拒绝落地：`unknown_asset`（掩码引用不存在的素材，路径指到 `.mask.asset_id`）/ `unimplemented_mask` 警告 / 掩码计入"被引用" / **求值层那一层的整帧拒绝（占位层也拦得住）**；老工程 sha256 不变 | criteria 的 **D12**（落地进度） |
| 第 21 轮 | 掩码的**引擎侧**落地：bind group 第三项 + 1×1 兜底图（没掩码时精确乘 1.0）+ 自己写的双线性 + alpha/亮度通道 + 反相；GPU 用例 2×2 铺 32×32 四象限/反相/两种通道；没有掩码时出片 sha256 一字不差；**还差 stap 3.5**（素材 → 纹理），工程路径仍拒绝 | criteria 的 **D12**（落地进度 + 一处实现时发现的修正） |
| 第 22 轮 | 掩码的 **DOM 侧**：只在 uri 能直接取到时写 `mask-image`，**相对文件名刻意不设**（取不到会把整个元素遮没）并报出来；反相在 CSS 里表达不了也报出来；台账新增 `mask.sampling_and_availability`（未实测）；自检 5→**6** 个错误映射器 | criteria 的 **D12**（第 22 轮那节） |
| 第 23 轮 | 掩码最后一步（素材 → 纹理）：`mask_texture_for` 带默认实现（没接的宿主返回 `None` ⇒ 调用方**拒绝整帧**）；worker 复用同一条解码通路；GPU 用例两条（四象限+反相在**渲染路径**上；解析不出来必须拒绝）；登记表 `geometry.mask` → `partial`（去掉 needs/design）；老工程 sha 不变 | criteria 的 **D12**（③ 与 ③′） |
| 第 24 轮 | **多边形裁剪**从「明确不做」变成可做：core 里的栅格化器（6 条单测）+ 复用掩码通路（**着色器一行没改**）+ 两条契约口径（顶点不足 / 与掩码互斥）+ DOM `polygon(…)` + 登记表翻成 supported | criteria 的 **D11**（第 24 轮那节） |
| 第 25 轮 | **`path()`** 接上：细分器落在 timeline（校验层与渲染层同一份解析）+ 按图层框文档像素归一化 + 复用栅格化通路；支持 M/L/H/V/C/Q/Z，弧与光滑续接**明说不支持**；8 条细分单测 + GPU 用例（直线与曲线）；DOM 原样透传 | criteria 的 **D11**（第 25 轮那节） |
| 第 26 轮 | 路径补齐：**弧 `A`**（SVG F.6.5 端点→圆心参数化，含半径放大那一步）+ **光滑续接 `S`/`T`**（控制点镜像，有等价性单测）；判据用**方程**判（点在圆上）而不是判轴向；顺手更新一条过期测试（它还在断言 A/S 会被拒） | criteria 的 **D11**（第 25/26 轮） |
| 第 27 轮 | **投影契约先行**（`Layer.shadow` +σ/opacity 口径 + 能力查询 + 渲染前拒绝）；**修正 D13 一处判断**：投影不是读回型（它读自己，分离式模糊已有），档位另记 `compose`；schema 已重生成且与仓库一致 | criteria 的 **D13**（第 27 轮修正） |

## 守卫清单与本会话的跑法（第 27 轮补记）

仓库共 **26 条**守卫（`scripts/run-guards.mjs --list`）。**本会话的沙箱里 `run-guards.mjs` 自己跑不动**：
它用管道 spawn 每条守卫，而沙箱不给开管道（`spawnSync node EPERM`）—— 所以它报的"0/26"是**环境读数，不是代码状态**。
我的跑法是**逐条直接跑**（把 stdout/stderr 重定向到文件）。第 27 轮逐条跑完的结果：

- **21 条绿**；
- **5 条被环境挡住**（都不是代码缺陷）：`check-web-invariants`（wasm pkg 比源码旧 —— 要 `wasm-pack build`，而沙箱不许写那目录）、
  `timeline-contract`（要管道 spawn `cargo`）、`check-local-backend`（同理）、`check-cli`（同理）、`check-dual-end`（要浏览器那条腿）。

**教训**（写在这儿免得下次再犯）：报"守卫全绿"时必须说清**跑了哪些、哪些没跑**；
本会话前几轮我只跑了自己列的 10 条，而仓里有 26 条 —— 那不是撒谎，但是**不完整的读数**。
| 第 28 轮 | **投影画出来了**：准备循环多推一张（模糊 + 偏移 + 染色 + 浓淡）+ 着色器 `tint` 开关；GPU 用例四条（含「画在下面」的逐字节相同、以及必须用**有 alpha 边界**的源才验得出模糊）；DOM `drop-shadow(x y 2σ rgba)` + 判据；**如实记一条已知差距**（模糊只能向内）并把 status 记 partial | criteria 的 **D13**（⑤/⑤′）、台账 `filter.drop-shadow.spread` |
| 第 29 轮 | **投影的向外扩散做对了**：先画轮廓进带透明留白的中间纹理（形状烘进去）→ 模糊 → 偏移染色画在下面；扩散**有界**（半径 × scale），判据点按几何选（(5,16) 有、(2,16) 无）；登记表 `filter.drop-shadow` → **supported**，台账里那条"已知差距"删掉 | criteria 的 **D13**（⑤′） |
| 第 31 轮 | **读回型回路落地**（D13）：`Step::BlendFn` + `plan_steps` + `render/blend_fn.rs`（W3C 公式 × 5，branchless）+ 复用分段渲染的中间纹理；GPU 用例逐条对手算期望值（含半透明支）；`is_implemented` → 9 条，两处 JS 清单同步；判据抓到两个真坑（单独渲要 `Normal`、**预乘 vs 直通**） | criteria 的 **D13**（第 30/31 轮） |
| 第 32 轮 | D13 判据 ④ 的落地与**诚实说明**：4 条固定方程用与读回型同一套判据钉住（`normal/add/multiply/screen` 对手算值）；老工程 sha256 仍 `8b7d9e18a7320ee7`；**记下教训**：动共用路径前要先留基线（这已是第二次） | criteria 的 **D13**（判据落地情况表） |

## 第 33 轮：一次失败与回退（记下来，因为它会在同一个地方再咬人）

本意是给 `backdrop-filter` 立契约（读回型的第二个用户）。我加了一个字段 `Layer.backdrop_effects`，
然后用**启发式批量补丁**去补全工程的构造点 —— **那是错的**：

启发式是"往上找最近的 `effects:` 行、在它后面插"。它在 `effects: if adjustment { ... }` 这类**多行块**上失手，
把字段插进了代码块里；错误数从 7 涨到 27，我还**继续跑第二轮启发式**去"修"它，越修越糟（又插错 20 行）。

**回退**：删掉字段与全部插入行（20 行），回到第 32 轮那个已验证状态 —— `cargo check` 0 warning、测试与守卫全绿。

**规矩（写下来）**：给契约加字段时的构造点补全**必须**：
1. **一次只动一个文件**，改完立刻编译；
2. 用**编译器给的行号**驱动（`--> file:line`），**不要**用"最近的某个字段名"这种启发式 ——
   它会在多行表达式上给出错误的插入点，而且**下一轮启发式会去修上一轮的错误**，错误会越滚越多；
3. 判据是**编译通过**，不是"我补完了"。
| 第 33 轮 | **一次失败与回退**：`backdrop-filter` 契约的批量补丁用启发式（找最近的 `effects:` 行）**插错了 20 行**、错误数越修越多 → **干净回退**回到第 32 轮状态；顺带发现并修掉**第 31 轮留下的 3 条陈旧单测**（能力声明 9 条 / 预检改成"旧对端只声明 4 条" / `unimplemented_blends` 恒空的断言换掉）。规矩已归档：构造点补全**一次一个文件 + 编译器行号驱动**，不用启发式 | 本轮无新增能力；教训写进 evidence |
| 第 34 轮 | **按新规矩重做 backdrop-filter**：契约 + **两张表同一套校验** + 能力查询 + 文档校验**报错**（不是警告，理由写进 note）+ DOM 侧复用 `layerFilters` 映射成 `backdrop-filter`；构造点补全用**编译器行号逐文件驱动**，14 处一次到位、零误伤（第 33 轮的教训直接兑现） | criteria 的 **D13**（下一步） |
| 第 35 轮 | 背景滤镜的**回路试做与回退**：`Step::Backdrop` 写出来了、计划单测过，但 GPU 判据抓到**我自己的实现错** —— 滤波结果在**目标分辨率**，却被通过"这一层的 0.5 缩放四边形"画回去 ⇒ 被**压成 2:1**（测试读到蓝只漏进 6）。**回退**（保留第 34 轮的契约+校验+报错）。**下一步的正确做法已写明**：不要用这一层的 transform 去贴（那会缩放），要用**这一层矩形的覆盖度掩码**（复用多边形栅格化器）把它 1:1 裁出来 | 本轮无能力落地；发现与做法写进 evidence |
| 第 36 轮 | **背景滤镜回路做成**：`Step::Backdrop` + 对累积结果跑特效 + 用**这一层矩形的覆盖度掩码**（复用多边形栅格化器）把结果 **1:1** 贴回去；GPU 判据「矩形外逐字节不变 / 双色边界两边互相染色」+ 计划单测；拆掉第 34 轮那条"还没画"的文档错误、登记表 `backdrop-filter` → **supported** | criteria 的 **D13** |
| 第 37 轮 | **`linear()` 断点表**（本仓最后一条"明确不做"的缓动形式）落地：按规范补全位置、值允许过冲、位置相等取后一个；`EasingForm` 失去 `Copy`（带 `Vec`）；3 条手算单测 + 浏览器参照用例 3 条 | criteria 的 **D2**（第 37 轮那节） |
| 第 38 轮 | **转译器侧镜像 `linear()`**：值取反 + 位置取反 + 升序（精确改写，非近似），且**一律写成显式百分比**（下游不必重推分布规则）；判据：样本 `wave`（alternate + 断点表）的既定答案 + 第 22 个变异（"线性断点表没镜像"）；另在 parity plan 末尾整理了**"要你做的五件事"**（5 条环境红各自的前置条件） | criteria 的 **D2**；plan 的附录 |
| 第 39 轮 | **登记表总校对**：逐条核 29 条，抓到**一个真漏网** —— 第 37 轮做的 `linear()` 断点表**没登记**（已补进 `easing.linear-cubic-steps` 的 label/note/evidence）；同时给判据加了一条新规矩：**`supported` 必须有一条"能跑的判据"**（`check-*.mjs` / `tests/*.rs` / 含 `#[test]` 的源文件），**光有文档不算** —— 12 条 supported 全部通过（说明登记表本来就诚实），自检 12→**13 个变异** | 判据：`check-capabilities.mjs` 的新规矩 |

**第 39 轮的两个真漏网**（都在这轮修掉了）：

1. `linear()` **没登记** —— 第 37 轮做的能力在登记表里查不到；
2. `easing.linear-stops` 还登记成 **`explicitly-not-doing`（"不做"）**，而它**已经做了** ——
   这是**最坏那类**：登记表在说假话，而假消息比不报更坏。已删掉（能力并入 `easing.linear-cubic-steps`）。

**新增的判据规矩**：`supported` 必须有一条**能跑的判据**（`check-*.mjs` / `tests/*.rs` / 含 `#[test]` 的源文件），
**光有文档不算**。12 条 supported 全部通过 —— 说明登记表本来就诚实，缺的是"它做了但没登记"这一侧。

**还没做的判据**（记下来）：`explicitly-not-doing` 目前**只能靠人记得删** —— 上面第 2 条就是这么漏的。
下一轮值得加一条：那条目必须写明拒绝理由（`why_not`，非占位），让它至少"有据可查"。
| 第 40 轮 | **`explicitly-not-doing` 必须写明拒绝理由**（新判据，非占位 ≥20 字；反向也管：不是这个状态就不许有 `why_not`）+ 给剩下 3 条写出真理由（`layout` / `dom-only-semantics` / `plugins`：本仓不是浏览器 / DOM 状态不是"画什么" / 插件把时间交给外部事件而求值必须是时间的纯函数）；自检 13→**15 个变异** | 判据：`check-capabilities.mjs` 的 `why_not` 规矩 |
| 第 41 轮 | **`partial` 复审**：12 条逐条核，抓到**两条过期**（`blend-mode` 还写着"引擎实现 4"——实际 9/9 都能画；`gradient-fill` 还写着"得先有几何原语"——原语早已齐），都已改写；新增判据 **`partial` 必须写明「差在哪」**（`gap`，非占位）—— 于是**三种状态各自要自证**：`supported` 拿得出能跑的判据、`partial` 说得出具体差距、`explicitly-not-doing` 写得出理由；自检 15→**17 个变异** | 判据：`check-capabilities.mjs` 的 `gap` 规矩 |
| 第 42 轮 | **乘性亮度落地**（`filter.brightness` 那条 gap）：**新增** `brightness_multiply`（`c·k`，保黑），既有的加性 `brightness` **不动**（铁律：不改既有画法）；`scale` 放在 uniform **末尾**（老字段偏移不动，`×1.0` 逐位精确 ⇒ 老工程 sha 仍 `8b7d9e18a7320ee7`）；DOM 与转译器两端都接上（`brightness(k)` 精确对应）；GPU 判据钉住"乘 vs 加方向相反" | 登记表 `filter.brightness` → **supported**（13/11/3/1） |
| 第 43 轮 | **一次失败与回退**（同第 33/35 轮那类）：本意照第 42 轮的成功路径给 `hue-rotate` 加一条**规范矩阵**的新原语。实现写完了，GPU 判据立刻说**不一致**：纯红转 90°，我手算规范那条应给 `(0, 91, 0)`、实现给的是 `(184, 87, 0)`（绿分量对得上、红分量完全不对）。本轮网络拿不到权威系数（w3.org 取不到），**不能把没弄懂的值当成判据**（那正是"断言代码现在干什么"那种最坏的判据）⇒ **干净回退**，登记表 `filter.hue-rotate` 仍为 partial。**下一步**：把矩阵写成显式 `dot` 而不是 `mat3x3`（先消掉"行/列约定"这个疑点），再拿权威系数（用户浏览器跑一次或另一次网络可用时） | 本轮无能力落地；发现与做法记此 |
| 第 44 轮 | **规范色相旋转落地**（`filter.hue-rotate` 那条 gap 关掉）：系数取自**权威源**（规范自己的 MathML `feColorMatrix03.mml`，本轮抓到的）、用**显式 `dot`** 写三行（消掉矩阵行/列约定的未知数）；GPU 判据：0° 逐位不变、90° 规范约 `(0,91,0)` vs YIQ 约 `(119,234,0)`（手算）；**真凶不是矩阵而是 uniform 字段顺序写反**（WGSL `hue,hue_css,scale` vs Rust `hue,scale,hue_css`）⇒ `cargo check` 抓不到（WGSL 建管线时才校验），已**补强那条 `uniform_布局与着色器一致`：现在连字段顺序一起钉** | 登记表 `filter.hue-rotate` → **supported**（14/10/3/1） |
| 第 45 轮 | **规范权重饱和度落地**（`filter.saturation` 那条 gap 关掉）：新增 `saturation_css`（权重 0.213/0.715/0.072），既有 Rec.709 那条不动；GPU 判据：amount=1 逐位不变、手算 `(200,137,73)`、**与 Rec.709 实测逐通道差 ≤ 1**（**实测代替了原来那条文字容差登记** `filter.saturation_luma_rounding`，已删）；着色器里我自己写错过一处（luma 取旧向量、mix 取新向量），改对了；自检抓到我"把警告期望改成必须不存在"之后那个旧变异**再也红不了**，已换成仍被盯的那种 | 登记表 `filter.saturation` → **supported**（15/9/3/1） |
| 第 46 轮 | **渐变遮罩（程序化）**：契约 `MaskSpec.gradient: Option<LinearGradient>`（与 `asset_id` **二选一**，空=老行为）、校验（二选一 + 断点 ≥2 + `at`/`coverage` ∈ [0,1] + 位置非递减）、**栅格化器** `rasterize_linear_gradient`（几何照 CSS：0° 朝上、90° 朝右，轴长 `|w·sinθ|+|h·cosθ|`，取像素中心投影）＋ **2 条手算单测**（0°/90° 方向与同列一致、`t=0.375 ⇒ 191`、超出末尾取端点）。**渲染接线与 DOM 映射是下一步** | 登记表 `geometry.mask` 仍 partial（下一轮更新） |
| 第 47 轮 | **渐变遮罩接上两端**：渲染侧在**这一层自己的尺寸**上栅格化（掩码按图层局部坐标采样，而 CSS 的轴长规矩是相对元素自己的盒子）→ `coverage_texture` → `MaskInput`；DOM 侧写成 `linear-gradient(<angle>deg, rgba(0,0,0,c) p% …)`（覆盖度落在 **alpha** 上，两端角度约定**正好一致**）；GPU 判据「渐变遮罩按层自己的尺寸铺开_左右与反相都对」（左透右实、中间半透明、框外为空、反相左右对调 —— 这同时钉住了"算在哪个尺寸上"）；DOM 判据补了一条内联检查 | 登记表 `geometry.mask` 的 gap 已改写 |
| 第 48 轮 | **一次"墙"被证明不是墙**（纯推理更正，行为暂不变）：`normal/reverse + iterations>1` 的交界跳变曾被判为"表达不了"。错在漏了一步 —— CSS 迭代是**半开区间**、而本仓只在**整数帧**求值 ⇒ 在 `B−1` 补一个键（值取前一遍那一帧的值）即可**逐帧完全一致**，那段一帧缓动**永远不会被求值**。更正写进 D7 与代码注释；**实现与判据留下一步**（继续拒绝 = 过拒绝，响亮且安全） | criteria 的 **D7**（更正段） |
| 第 49 轮 | **对上一轮更正的二阶更正**："在交界前一帧补一个键"**不够** —— 键的 easing 描述整段，段尾从 `B` 挪到 `B−1` 会让段长变、段内每帧都偏（`E(f/12)` vs `E(f/11)`）。逐帧精确要**按整数帧把这一遍烘开**（键数按遍长增长），那是**产品决定** ⇒ 继续拒绝、把代价写清（D7 与代码注释同步） | criteria 的 **D7**（二阶更正段） |
| 第 50 轮 | **`timing.iterationStart` 查清并改精确**：**整数相位早就支持且精确**（`isReversedPass(iterationStart + pass)` + `start += iterationStart * span`）—— 但**没有判据**钉它；**非整数相位**与「同向重播的跳变」**同源**（第一遍变成部分遍，而补一个键不成立：键的 easing 描述整段，段尾一挪段内每帧都偏），精确表达要**按整数帧烘开** ⇒ 产品决定。**本轮试加的判据没有生效**（第一次被吞成注释、第二次锚点没匹配，守卫始终绿 ⇒ 没改动 —— 已核实文件是干净的）；临时加的样本动画已撤回 | 登记表 `timing.iterationStart` 的 gap 改写；**补判据是下一步** |
| 第 51 轮 | **整数相位的判据补上了**（第 50 轮欠的那条）：样本加回 `drift`（`iterationStart=1`、`iterations=2`、`alternate`），既定答案 = **起点 12 / 层长 25 / 三键 `1 → 0 → 1` 且第 1 遍反向**（与"奇偶性跟着变"的预测一致）；**先故意让守卫红一次**证明判据在跑（第 50 轮两次插入都没生效），再加自检变异「整数相位的起点没右移」⇒ 自检 22→**23 个变异** | 登记表 `timing.iterationStart` 的 gap/evidence 改写 |
| 第 52 轮 | **把一句论断量成了数**：台账 `geometry.edge_antialias` 的 note 写着"引擎是 1 目标像素宽的线性斜坡"——**没量过**。本轮加 GPU 判据「圆角层的边缘是窄抗锯齿_不是硬边也不是模糊」（层转 30° 逼出真抗锯齿，量**全图**半透明像素 ÷ 周长）：实测 **28/128 ⇒ 斜坡 ≈ 0.22 目标像素宽**（α ∈ [133,236]）⇒ **原论断被推翻**（实际更窄）。判据两头都能红（0 个 = 硬边、比值过大 = 模糊）。台账 note 改成不含量化数字的准确说法（该条仍 status=未实测：DOM 侧要浏览器），实数记进 `geometry.corner-radius` 的 evidence。**注意**：台账守卫只查字段自洽、**抓不到 note 写错**——本轮我自己就把 note 弄脏过一次（已修） | 台账 `geometry.edge_antialias` + registry |
| 第 53 轮 | **`add` vs `plus-lighter`：把"未必"变成精确关系**：写下两边的定义（本仓 `src·1 + dst·1` **不看源 alpha**；CSS `αs·Cs + Cb` 预乘后相加）⇒ **相同当且仅当 αs = 1**，αs < 1 时本仓多加 `(1−αs)·Cs`（alpha 走法也不同：`αs+αb(1−αs)` vs `min(1, αs+αb)`）。GPU 判据「加法混合_rgb_不看源_alpha_而_css_会看」实测：opacity 1.0 与 0.5 **rgb 一字不变**（这正是差别的来源），并把"差 = (1−αs)·Cs"也当成断言（不靠注释）。台账 note 与 registry evidence 同步 | 台账 `blend.add_plus_lighter` |
| 第 54 轮 | **台账里一条塞了三种差异，拆开了**：`mask.sampling_and_availability` 拆成 `mask.availability`（取不到就不设 —— **刻意**，status 未实测→**明确接受**）、`mask.invert_dom`（反相在 CSS 里表达不了 —— **刻意**，→**明确接受**）、`mask.interpolation_kernel`（插值核不同 —— 真差异，**未实测**，引擎侧有 GPU 实测）。**拆的理由**：一条 `未实测` 会把"刻意且已判"的差异伪装成"还没量"。三条的 JS 判据**本来就在**（`check-dom-css` 的 checkMaskCases 早有：取不到必须 css 空 + 报「遮没」、带 invert 必须报出来）—— 只是台账没这么说 | 台账 4→**6 条**（未实测 3 / 明确接受 3） |
| 第 55 轮 | **掩码插值核：引擎侧实测，顺带量出一个真缺陷**。把 2×2 掩码（第一个纹素全亮）铺到 32×32：剖面 `120×8, 116, 108, 101, 93, 86, 78, 71, 64, 56, 49, 41, 34, 26, 19, 11, 4, 0…` —— ① 斜坡**线性**（增量恒定 ≈ −7.4）⇒ 确实是自己写的双线性 ✓；② 但**第一个纹素是全亮 255，读出来只有 120** ⇒ **掩码外边缘没有被钳住**（`u = uv·N − 0.5` 为负时小数权重没丢，把相邻暗纹素混了进来）✗。这是**引擎内部缺陷**（不是 DOM↔引擎差异），已记为 **D17**（`plan/defects.md` + roadmap 引用），判据把 **120 这个错值**钉住并注明"D17 修好后这里会红、届时改成 255" | `plan/defects.md` D17 |
| 第 56 轮 | **撤回一条假警报（D17）** —— 上一轮我"量出"的掩码边缘缺陷，**是我自己量错了**：2×2 掩码铺到 32×32 时，两行纹素的**分界正好在 y=16**，而我读的就是那一行 —— 120 **本来就是对的**（亮暗各半）。决定性对照：**全亮掩码**处处 255 ⇒ 钳制没问题。改读纹素中心那一行（y=4）后，剖面是 `255×8` + **恒定增量 −16** 的线性斜坡 ⇒ **我们的双线性是对的**。已撤回 D17（`plan/defects.md` 用 git 里的原版恢复：26 条 ✓）并把判据改成三条真断言（边缘精确 255 / 斜坡线性 / 单调）。**教训**：量之前先想清楚"我在量哪一行/哪一个点"——本会话第三次栽在"位置"上 | 台账 `mask.interpolation_kernel` 的 note 同步 |
| 第 57 轮 | **把本会话散在各轮的引擎侧实测收拢成一张表**：新建 `plan/web-engine-measurements.md`（**与 `plan/measurements.md` 分工**：那份量性能、这份量正确性）。11 条量过的（每条带实测值 + 会红的判据 + 轮次）+ 5 条**没量过的**（写明"要浏览器"与谁来量）。并如实写上这批实测的**两个短板**：① GPU 适配器型号没记；② 全部是引擎侧，DOM 侧一条都没量 | 新文件 `plan/web-engine-measurements.md` |
| 第 57 轮（续） | 台账里承认的短板**当场补掉**：让实测用例自己打印 GPU 适配器 ⇒ 记下 **NVIDIA GeForce RTX 4070 / driver 610.74 / Vulkan / Windows**（`ctx.adapter_info` 现成）。短板从两处减到一处（剩下的那处是 DOM 侧要浏览器，能力之外） | `plan/web-engine-measurements.md` 的机器口径 |
| 第 58 轮 | **浏览器宿主：把"接不上"变响亮**。预览宿主（wasm）没有素材表，取不到掩码图；以前那是"只 debug_assert ⇒ 静默画一张**没有掩码**的图" ✗ —— 那种错没人看得出来。现在：① `dhampir-timeline` 新增纯函数 `mask_asset_ids`（去重、**不含渐变遮罩**，有单测）② 预览宿主在 `draw_impl` 开头查出掩码素材清单，非空就**返回错误**（画面宁可报错，也不要一张看起来正常、其实少了掩码的图）。**完整的掩码上传/解码没做**（那需要 JS 侧解码 + 纹理上传，超出本轮可稳妥完成的范围）—— 现在这条链是"**要么画对，要么响亮报错**" | 判据：`crates/dhampir-timeline/src/layer.rs` 的单测「掩码素材清单_去重且不含渐变」 |
| 第 59 轮 | **登记表终审 + 一条更狠的规矩**：① 终审发现 15 条 `supported` **全部**都已点名可跑的判据（0 条不合格）；② 于是把这条钉成规矩，并加狠一档：**点名的判据必须真的在仓库里找得到**（用例名去 Rust 源里找 `fn <名字>`，`check-*.mjs` 去 scripts 里找文件）—— 规矩**当场就抓到一个真过期**：`filter.hue-rotate` 的 evidence 点名的是 `规范色相旋转与_YIQ_…`，而第 44 轮为修 lint 已把它改名成小写 `_yiq_`（evidence 没跟着改）⇒ 已修。自检加一个变异（"点名的用例不存在"） | 判据：`check-capabilities.mjs` 的新规矩 |
| 第 60 轮 | **GPU 用例索引 + 判据保证它不腐烂**：`plan/web-engine-measurements.md` 新增"GPU 用例索引"（**29 条** = timeline 21 + compose 8，每条给 文件 + 类别（量值/几何/行为），并给出两条复现命令）。判据：`check-capabilities.mjs` 的 `judgeMeasurementIndex` 把索引里的名字与代码里的 `#[ignore]` 用例**双向比对** ⇒ 新加/删掉用例而没同步就红。**已反向验证会红**（加一行假索引 ⇒ 红，删掉 ⇒ 绿） | `plan/web-engine-measurements.md` 第三节 |
| 第 61 轮 | **掩码链的"宿主该回答什么"立好了接口**：`dhampir-timeline` 新增 `missing_mask_assets(timeline, registered)`（纯函数 + 4 组判据：一张没注册⇒列出素材那张、注册过⇒空、注册了别的⇒照样缺、没有掩码⇒永远空、**渐变永不出现**）；预览宿主的拒绝**改成调用它** —— 现在传 `&[]`（行为与第 58 轮相同），但**调用点已是最终形状**：宿主掩码注册表落地时，只把 `&[]` 换成已注册 id 列表。**取图 + 上传那一半（JS fetch/createImageBitmap + 纹理上传 + 注册表）本轮仍然没做** —— 它在这里验不了，我不写验不了的代码 | 判据：`crates/dhampir-timeline/src/layer.rs` 的「缺哪些掩码素材_注册过的就不缺了」 |
| 第 62 轮 | **量法本身也会腐烂**：台账 `method` 里点名的脚本（3 个）都在 ✓，但守卫**不查这件事** ✗。新增规矩 **R7**：`未实测` 的量法里出现的 `scripts/*.mjs` 必须真的存在（否则那份量法只是装饰）。自检加变异「量法点了一个不存在的脚本」⇒ **自检 7 → 8 个变异**（第一次写变异时用了中文脚本名 ✗，规则的正则只认 ASCII，改成 `no-such-script.mjs` 后当场红 ✓） | 判据：`check-dom-differences.mjs` 的 R7 |
| 第 63 轮 | **"要你做的事"改成生成式**：新增 `scripts/web-parity-pending.mjs` —— 三段清单（① 台账 `未实测` 的条目 + 它们的量法 **直接从台账读**；② 环境判据 5 件（wasm 重建 / `run-guards.mjs` / 双端 / 19 条缓动对照 / 打开 dom-host）；③ 等你拍板的两件 D7 与 asset.uri）。**结构上不可能与台账不一致**（写死的清单一定会过期）。plan 附录里那份**手写**清单删掉、改成指向脚本（一份真相） | 新脚本 + plan 附录 |
| 第 64 轮 | **判据家底做成生成器 + 一个不太好看的发现**：新增 `scripts/guards-inventory.mjs`（逐个跑 26 条守卫的 `--self-test`，从它们自己的输出读变异数，且报出正检现状）。**发现**：26 条里只有 **6 条带自检**（共 **72 个变异**），**20 条没有** ⇒ "守卫会红"这句承诺目前只对 6 条被**证明**过；没自检的那些靠"正检够具体"，但没人反向验证过。已写进 plan 附二（结论），下一步给它们补自检（从 `check-defects` 起）。生成器已用**我这条能跑的路径**复现过解析（26 条 → 6/72 一致） | plan 附二 + 新脚本 |
| 第 65 轮 | **更正第 64 轮那条错结论**：我在 plan 附二里写过"26 条守卫只有 6 条带自检" ✗ —— 真相是 **26/26 全都有**，合计 **428** 条断言/变异。错因是解析只认两种措辞（`个变异`/`个错误映射器`），而实际还有 `条断言`/`条用例`/`8 条解析用例 + 5 条规则用例` 这类**加和**形式。生成器 `guards-inventory.mjs` 的解析已改成"把所有能认出的计数加起来、一条都认不出才诚实地说读不出"；plan 附二已更正并写下教训：**"读不出来"不等于"没有"** | plan 附二 + 生成器 |
| 第 66 轮 | **刚踩过的坑，趁热自查**：待办清单生成器 `web-parity-pending.mjs` 也在"解析别人写的台账" —— 于是给它的解析加**合成台账自检**（跨行 method / **单行 method** / note 里的引号 / 三档筛选），并把单行 method 这种写法补进支持（原来只认跨行 ✗，与我第 64 轮那个错**同源**）。**已反向验证**：故意去掉单行支持 ⇒ 自检红 ✓、恢复 ⇒ 绿 ✓。另与台账守卫**交叉核对**：两边都说"未实测 3 条" ✓ | 脚本自检 + 交叉核对 |
| 第 67 轮 | **把"26 条"变成可核对的事实**：`run-guards.mjs` 里**本来就有**"清点 scripts/ 与 GUARDS 对表"的逻辑（还有自检）—— 所以这个数一直可信；本轮做的是**核它**：清单 26 = 盘上 26（**两个方向都无缺口**）、而且**我这一路手打的那份与清单完全一致** ✓（历轮报的 21/26 因此是清单一致的）。按清单驱动跑一次：**21/26 全绿、26 条自检全部通过、5 条红全是环境**。**沙箱限制如实记**：`run-guards.mjs` 在本会话跑不动（管道 ✗），而把"清点+总跑"委托给子脚本也不行 —— **第二层 spawn 被拒**（`EPERM`）⇒ 清点只能在我自己程序里做（本轮就是这么做的） | 清点 + 按清单总跑 |
| 第 68 轮 | **把台账的 3 条 `method` 变成操作手册**：待办脚本新增 `STEPS`（键在**节名**上）—— 每条给出"跑什么 / 看什么 / 把数填到哪"四步，且**双向查同步**：台账里有条目而我没写步骤 ⇒ 报"⚠ 还没有操作步骤"；我有步骤而台账里没有那个节名 ⇒ 报"⚠ 节名改了？"。**已反向验证**（把一个键改名 ⇒ 两条警告同时出现 ✓，恢复 ⇒ 干净 ✓）。另核了步骤里点名的 `target/` 路径是否真实存在 | 脚本 + 反向验证 |
| 第 69 轮 | **三向对表（跑成数据再决定加什么规矩）**：① 索引 29 条用例里 **17 条没被登记表点名** ✗（低危：用例在跑在守，只是没被引用）；② `supported` 15 条里 5 条没提实测 ✓**不是缺口**（它们由单测钉着 —— 我那条启发式太严 ✗，如实记下）；③ **登记表 0 条指向实测台账** ✗✗ 真缺口。**修**：给 10 条带数的条目各补一句指向 `plan/web-engine-measurements.md`（`blend-mode`/`filter.hue-rotate`/`filter.brightness`/`filter.saturation`/`filter.drop-shadow`/`geometry.corner-radius`/`geometry.clip-path`/`geometry.clip-path-polygon`/`geometry.mask`/`backdrop-filter`），并在台账里写明"谁在引用我 + 那 17 条照跑照守"。**加判据**：讲了实测却没指向台账 ⇒ 红（含变异） | 登记表 + 台账 + `check-capabilities` 新规矩 |
| 第 70 轮 | **把"等你拍板的 D7"变成"已经把代价算清、且不必现在拍板"**：从真实键体积（≈60 B/键）与 `iterations × span + 1` 算出三种情形的代价（`badge` 4→37 键 **+825%**；2 秒 5 通道 **+52 KB**；10 秒 8 通道 **+280 KB**），并把选项**重构成三个** —— A 烘帧 / B 现状（明说不支持、作者拆两段，**成本在作者侧且一次性**）/ **C 契约里加 `iterations`+`direction` 让引擎原生求值**（没有体积、作者也不用做，但那是 D2/D14 那条线上的**结构性改动**，属另一件事）。**建议 B**，并写明**何时该翻案**：出现第一个"不愿意拆两段"的真实需求时 | criteria 的 D7 |
| 第 71 轮 | **全量交接报告**：新增 `plan/web-alignment-handover.md`（快照，写于第 71 轮）—— 目标与三阶段状态 / 这一路加的能力（15 条 supported）/ 判据体系（26 守卫 · 428 断言 · 三条铁规矩各自的判据）/ 三本台账 / **如实列出没做的五件**（浏览器侧掩码上传、DOM 侧数字、真渲染确认、17 条未被点名的用例、asset.uri 那 2 条既有红）/ 接手第一件事。**所有数字从现场读**（登记表用真实计数、台账条数用解析结果；第一版把"实测行"数成了 47 ✗ —— 把索引也数进去了，已按第一节数据行改正） | 交接报告 |
| 第 72 轮 | **浏览器腿掩码通路：wasm 半做完了**。定位到工程预览用的是 `BoundVideos`（第 1458 行）⇒ 加：宿主字段 `masks`（asset id → 纹理）+ `BoundVideos` 的 `masks` 字段与 **`mask_texture_for` 覆写** + 导出 **`dhampir_project_set_mask_image(asset_id, width, height, rgba)`**（长度不符即报错）+ 拒绝改用**宿主注册表**（`missing_mask_assets(&doc.timeline, &registered_mask_ids)`）⇒ `cargo check -p dhampir-wasm --all-targets` 通过 ✓。**JS 半（fetch → 解码 → 上传）没写** —— 它要对 `web/engine.js`（1100+ 行）的内部结构动手，我的剩余预算不足以**稳妥**做完 ✗；而"写没验过的 JS"正是本会话反复吃亏的那种事 ✗。接口即文档：一行 export 的签名 + 像素布局（RGBA、`width*4` 步长）已足够写出来。**安全网**：没注册的掩码 = `draw` **响亮报错**，绝不静默画一张没有掩码的图 ✓ | wasm 宿主 |
| 第 73 轮 | **浏览器腿掩码通路：JS 半也写了**（先读清 `engine.js` 再动手 ✓）。① 读到本仓位图进纹理的**唯一**一条路是 `copy_external_image_to_texture`（`ExternalImageSource::ImageBitmap`，`BoundVideos` 就是这么传的）⇒ 把 setter 从"收裸像素"改成**收 `ImageBitmap`** ✓（与 `dhampir_project_set_bitmap` 同形状；JS 侧因此只要 `fetch` + `createImageBitmap` 两步，不必走离屏 canvas + `getImageData`）；② 读到 `open()` 是**同步**的、取图要 await ⇒ 在 `web/engine.js` 加**独立一步** `async uploadMasks()`（`open` 之后 await 一次；掩码只在换工程时上传，不逐帧）—— 只认 `asset_id` 那种、**渐变遮罩不算**；取不到的**不静默失败**，留给渲染时那条响亮拒绝说明白是哪几个 asset；③ 待办脚本补上这一步（含"不做会怎样"）。**`node --check` 通过 ✓、`cargo check -p dhampir-wasm` 通过 ✓；浏览器里的实际行为我这里验不了（第 75 轮起由你在浏览器确认）** | wasm 宿主 + `web/engine.js` |
| 第 74 轮 | **收三处措辞尾巴**（我第 72/73 轮改了接口与实现，文档就过期了 —— 这类"说假话"是本会话抓得最多的失效模式）：① 差异台账 `mask.availability` 补一句说清**两套宿主**（DOM 宿主刻意不设 vs wasm 预览宿主走 `set_mask_image` + `uploadMasks`）；② `docs/host-api.md` 的签名改回**收位图**（`(asset_id, bitmap)` ✓，并写明"一个 `copy_external_image_to_texture` 就上了 GPU"与调用方是 `uploadMasks()`）；③ 交接报告 §五 第 1 项从"**没做**"改成"**两端都接上了 ✓，剩浏览器实测**"。三个相关守卫 + 全量 21/26 复跑全绿 ✓ | 三份文档 |
| 第 75 轮 | **给"改了实现 ⇒ 文档滞后"补判据**：名字级判据查不出**签名漂移**（第 74 轮那处就是这样漏的 ✗：文档 `(asset_id, width, height, rgba)` vs 代码 `(asset_id, bitmap)`）。新增：`scanSignatures`（扫导出函数的**参数名**，处理跨行签名）+ `documentedParams`（解析文档 `### \`name(参数)\``）+ 判据里逐个比对。**已反向验证**：自检新增"参数漂了 ⇒ 红"变异 ✓；并**现场**把文档签名改回旧写法 ⇒ 守卫红 ✓、恢复 ⇒ 绿 ✓。当前全仓无签名漂移 ✓ | `scripts/api-surface.mjs` |
| 第 76 轮 | **"改了实现 ⇒ 该跑哪条守卫 / 该改哪份文档"做成生成器**（`scripts/guard-coverage.mjs`）：从 `run-guards.mjs` 的**唯一清单**取名字，再读每条守卫**自己的题头**与它源码里的仓库路径 ⇒ 输出一张三列表（守卫 / 它管什么 / 绑定的路径）。**它只读文件、不 spawn** ⇒ 在连管道都没有的环境里也能跑（这一条与 `guards-inventory`/`run-guards` 不同，那两条在我这跑不动）。**如实记两处不足**：① 路径提取第一版把注释与消息串也抓进来了（"看起来像表其实是噪音" ✗）⇒ 加了"必须像路径"的 ASCII 过滤 ✓；② 少数守卫自检里的**夹具路径**（如 `crates/a/Cargo.toml`）仍会出现在表里 —— 是噪音，无害，但读表时要留意 ✓。交接报告已指向它 | 新脚本 + 交接报告 |
| 第 77 轮 | **按目标原文核对三个出口并收口**：① 老工程逐字节不变 —— `frame(plain)` sha256 仍 `8b7d9e18a7320ee7`（第 0 轮到第 77 轮每次改动后重测）✓；② 守卫会红 —— **自检 26/26 通过**、正检 21/26 绿（5 条红全是环境）、GPU 用例 timeline 21 + compose 8 全绿 ✓；③ 证据归档 —— 三份 stage 证据在、三本台账 + 交接报告 + 三件生成式工具都在 ✓。另：`cargo check --all-targets` 0 warning、`cargo test --workspace` **750 passed**（剩余 2 条为**既有红**，本会话未动）。据此把目标判为**完成**；仍未做的三件全部是**人/浏览器侧**（浏览器看一眼、应用层调 `uploadMasks()`、DOM 侧实测数字），逐条写在交接报告 §五 | 交接报告 §七（出口核对） |
| 第 78 轮 | **三份产物（全部枚举效果）**：① 生成器 `scripts/effects-demo.mjs`（素材用捆绑 Pillow 生成；工程 **4 轨 / 103 层 / 860 帧**，覆盖 **16 条登记效果 + 9 条混合 + 5 种裁剪 + 圆角/素材掩码/渐变掩码/投影/背景滤镜/8 种缓动**；probe **0 错 0 警**；附 43 条索引）。② **native**：新增 examples/effects_demo.rs（readback 自己解 PNG + compose::evaluate_v2 + render_frame_at）出 860 帧 → **MJPEG/AVI 10.0MB（430 帧，结构自校验：索引逐项指对、每帧可解）** + APNG 3.9MB + GIF 1.8MB + 联系表 404KB。③ **wasm 网页** web/effects-demo-wasm.html（pkg 已构建）+ **前端 HTML** web/effects-demo.html（DOM/CSS 宿主）；两份都把工程内联、素材走 <img>+createImageBitmap（file:// 下 fetch 会被拒）。**顺手修掉三处真破损**：--features json-schema 编译冲突（重复 derive + Layer 的文档/derive 被挤到 MaskChannel 头上）、**契约派生物落后于契约**（doc-v1/timeline-v4 缺本会话新字段，按官方 emitDts 重生成并模拟三条检查全过）、**wasm32 编译不过**（9 错：语法错 / 两处漏 masks / 重复 #[wasm_bindgen] / host.device 应为 host.ctx.device）。守卫 **21/26 -> 24/26**（只剩两条要管道）| out/effects-demo、web/、crates/dhampir-wasm、scripts/ |
| 第 79 轮 | **本机 ffmpeg 找到了**（不在机器 PATH 上，在应用自带目录里：`D:\Download\DownKyi-1.1.6-1.win-x64\ffmpeg\ffmpeg.exe`，含 libx264）⇒ 据此出**真 mp4**：`out/effects-demo/effects-demo.mp4`（H.264 861 帧 640x360 30fps 2.63MB 28.70 秒）；用 ffmpeg 抽第 500 帧与引擎 PNG 比，**平均差 1.42**（最大 182 在锐利彩边，是 yuv420p 色度半采样的固有误差）。**顺带找到并定位一处真 bug**：`dhampir render`（产品出片路径）在第 370 帧 panic —— `pipeline.rs:371 attempt to subtract with overflow`；根因是池子记账按「同一 (素材,帧号) 只被要一次」预算，而**背景滤镜那一趟会把身后的层再画一遍** ⇒ 同一帧同一个源被要两次 ⇒ `*count -= 1` 透支（debug panic / release 静默回绕）；最小复现：帧 360..385。**另一处**：CLI 的 `frame`/`render` 对**静态图素材**（`kind: image` 而未声明 `timebase`+`frame_count`）会取「第 N 帧」⇒ 解码为空 ⇒ **静默跳过该层**（全黑图，实测 1699 字节/均值 0），声明 `frame_count:1` + `timebase` + `loop_source` 后正常。修好素材声明后，**CLI 与 examples/effects_demo 出同一帧逐字节相同**（sha `7a2ce327f0a3475e`，最大像素差 0）| out/effects-demo、README |
