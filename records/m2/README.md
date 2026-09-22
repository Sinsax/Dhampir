# M2 —— corpus 记录与浏览器取证

> 这份目录是 M2 的**可复核产物**。读它的人不需要相信我，只需要跑两条命令：
>
> ```sh
> node scripts/check-m2-record.mjs --record records/m2   # 整份记录重算一遍
> node scripts/check-m2-record.mjs --self-test           # 守卫自己的自检
> ```
>
> 守卫不是"读一遍记录看像不像"：它把 160 张 PNG 重新解码、重算 SSIM / PSNR / 差异统计、
> 重新解析档位、重新判定一次，再把 `summary.csv` / `verdict.json` / `report.txt` / `shape.json`
> 重渲一遍**逐字节对账**。对不上就红——记录里的数必须能被第二个人算出来。

## 这个里程碑要回答什么

M1 已经证明「同一份 corpus 全表在 native headless wgpu 上跑得出 80 帧」。
M2 要回答的是**另外两个宿主**：

1. **同一份渲染真的被两个宿主共用** —— 浏览器（Chromium WebGPU）跑出来的 80 帧，
   与 M1 那份 native 归档**逐字节相等**：两边的整表摘要都是 `71ecc80cade3d73d`，
   帧**集合**摘要（含文件名与文件字节）都是 `4bc004b502a1301a`，PNG 总字节都是 `129619`。
2. **换一块卡（另一个厂商）之后差异长什么形状** —— AMD iGPU 腿的整表摘要是
   `a37b0ab5140b18e6`（**与上面不同，而这正是结论的一部分**），PNG 总字节 `129765`。
   差异不是"有没有差"，而是"差在哪、什么形状"（见下面那张表），且能对着容差说清楚。
3. **差异的归因清单落文件** —— `plan/wgsl-portable-subset.md`（第 3、4 条退出标准）。

## 目录里有什么

| 条目 | 是什么 |
|---|---|
| `browser/` | **NVIDIA 浏览器腿**（`browser-webgpu-nvidia`）：Chromium 的 WebGPU 上跑完整 corpus（5 场景 × 16 帧 = 80 帧）。含 `run.json`（全表 + 逐帧 23 个采样点）、`frames/` 80 张 PNG、`readings.txt`、`adapter.json`、`host-gpu.json`、截图对（`screenshot-browser-corpus.png` + 同名 `.json`） |
| `browser-amd/` | **AMD iGPU 腿**（`browser-webgpu-amd`，启动参数多一条 `--force_low_power_gpu`）：同上，外加 `rerun-repro.json`（同一台机器重跑一次的复现证据：80/80 帧逐字节相同、`readings.txt` 两次 sha256 前 16 位同为 `496a5e3ef2e8e2ba`） |
| `framediff/` | **严格档比对** `browser/` vs native 归档：档位是「逐字节相等」（`max_abs_diff_max = 0`），档位摘要 `fce01cde0b735693`，5 场景 80 帧全 `pass`（`max\|Δ\| 0`、差异像素 0）。产出 `summary.csv` / `verdict.json` / `report.txt`（**没有 `diff/`**——没有差异就不写黑图充数） |
| `framediff-crossvendor/` | **跨厂商档比对** `browser-amd/` vs native 归档：档位是实测定出来的（`mean/min SSIM 0.9995`、`max_abs_diff_max 1`，`checker` 与 `srgb_linear` 两个场景单独收紧到 0），档位摘要 `3f9560d1e3d4fb3f`。产出同上，外加 `shape.json`（差异的**形状**）与 `diff/` 48 张放大差异图，差异像素合计 `1105968` |
| `wasm-tests.json` | wasm32 侧的双运行时等值报告（同一份 golden 报告在 native 与 wasm32 上逐字节相等），`10/10` |
| `acceptance.json` | 这份记录的**退出标准**：每一条判据的命令、退出码、耗时；每一项的原始输出在同名 `.txt` |
| `README.md` | 你正在读的这份 |

## 跨厂商的差异长什么形状（`framediff-crossvendor/report.txt` 原样）

| 场景 | mean SSIM | min SSIM | max\|Δ\| | 差异像素 |
|---|---|---|---|---|
| `gradient` | 0.9997868247257939 | 0.9997810177884726 | 1 | 57344 |
| `alpha_stack` | 0.9999911822267717 | 0.9999892068517506 | 1 | 1048576 |
| `blur` | 0.9999999782648813 | 0.999999978264881 | 1 | 48 |
| `checker` | 1 | 1 | 0 | 0 |
| `srgb_linear` | 1 | 1 | 0 | 0 |

读法：**`checker` 与 `srgb_linear` 在 AMD 上逐字节相同**（纯 8 位量化、不经过 sRGB 往返的
那些通道没被碰），差异全部集中在经过 sRGB 编解码的通道上，且**只有 1 个字节的差**
（`max|Δ| 1`）——不是"画得不一样"，是"在舍入边界上落到了另一侧"。
`alpha_stack` 的 1048576 是整张 256×256×16 帧都在动的场景（差异像素按帧累加），
所以它的像素数大而 SSIM 依然 0.99999；**"差异像素多"和"画错了"必须分得开**。

## 三处**容易读错**的地方（都是故意的，不要去"修正"）

1. **两份 `run.json` / `adapter.json` 里的 `milestone` 写的是 `M1`，不是 `M2`。**
   那一栏是**记录契约版本**（表的形状），不是跑的时刻——表契约自 M1 起没变过，
   所以 M2 跑出来的表照样写 `M1`。而截图 json 里的 `milestone` 写 `M2`，因为那一栏记的是
   **这次取证属于哪个里程碑**；`wasm-tests.json` 里写 `M0`，同理是那份报告形状的版本。
   三处都故意如此；把它们改成一致，反而会让 M1 那边的守卫与记录对不上。

2. **`browser-amd/` 里有几处 `false`（`server_findings` 的第 2、3 条、`audit.findings` 的第 4、5 条），
   而 `audit.ok` 仍然是 `true`。** 这不是失败：那几条问的是"两台宿主的这些维度是否**相同**"，
   答 `false` 是在如实报告"不同"（不同的一块卡、不同的驱动）——**"不同"与"出错"是两件事**。
   `audit.ok === true` 才是"这份取证本身没有问题"。

3. **native 腿不在这个目录里。** 它是 `records/m1/dx12`——M1 那次归档，由
   `scripts/check-m1-record.mjs` 整份核过。M2 的比对拿它当锚，所以本目录的 `acceptance.json`
   里有一条判据是把 M1 归档再跑一遍；它红了，M2 的结论就建在没核过的目录上。

## 两条浏览器腿之间的差异是什么

`adapter.json` 逐字段比下来恰好 **6 条路径**不同（其余全同）：`backend_slug`、
`in_page.architecture`、`in_page.subgroup_max_size`、`in_page.vendor`、`unix_epoch_seconds`、
`unix_epoch_millis`。前四条是"这是另一块卡"，后两条是"这是另一次运行"。

`readings.txt`（同一份逐点测量文本，532 行）在 browser 与 native 之间**逐字节相同**——
0 行不同。AMD 腿与它们差 **38 行**，**第一个**不同的行是**第 22 行**（0 基）。

`run.json` **三份各不相同**，差的是这些：browser 与 native 只差 2 条路径
（`backends[0].adapter_name`——浏览器读不出卡名，服务端写 `null`；`backends[0].requested`
——`BROWSER_WEBGPU` / `DX12`），差的只是"这是谁跑的"；两条浏览器腿之间 **273 条**：
48 帧的三种摘要（`pixel_digest` / `png_digest` / `repeat_pixel_digest`）+
`frames_digest` + 31 处 `png_bytes` + 97 处采样点字段（`measured` / `distance` / `detail`）。

> 这一版 README 早先在这里写错过一句（把 `readings.txt` 说成两份都逐字节相同），
> 量了一遍才发现不对——所以这句被**逐字**钉成了禁令，写 README 时连引用旧说法都不行。
> 数差异**组**不是为了说"差异有多大"，是为了把差异钉住：多出一组就说明有东西变了。

## 这份记录的边界（明确不做）

- 跨厂商那条结论**只对"这两个厂商 + 这五个场景"成立**。下一块卡（Intel Arc、Apple M 系）进来时，
  归因表要么被验证、要么被改写（见 `plan/wgsl-portable-subset.md` §6）。
- `framediff/` 的"全达标"是**对照**不是**考题**：同厂商同栈下逐字节相等本来就不该有分辨力。
  工具的分辨力由另外三处证：工具自检里的合成负用例、反向验证（逐处抠掉都要红）、
  真数据负对照（真错位 → 只点名 `checker`、`max|Δ| 174`、EXIT=1）。
- **唯一不许的做法**：为了让某次记录变绿，去删掉或放宽 `[scenario.checker]` /
  `[scenario.srgb_linear]` 的 `max_abs_diff_max = 0`。档位文件改一个字节，摘要就变，
  对应记录必须重跑。
