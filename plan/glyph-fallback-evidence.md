# 字形回退（glyph fallback）链的证据台账

这份文档收 T1–T4 那条链的**实测数字**。规矩与仓库里别的证据文档一样：
每一条判据都要能**复算**（命令、版本、期望值都写下来），并且每条判据都要有
**反向用例**——一个不能变红的判据不是判据。

本机环境（下面所有数字都出自这一套）：

| 项 | 值 |
|---|---|
| ffmpeg | `9.0.1-full_build-www.gyan.dev`（gcc 16.1.0 / MSYS2） |
| libass | 在（`--enable-libass`，`subtitles` 与 `ass` 滤镜都在） |
| `fc-list` | **没有** |
| fontconfig 默认配置 | **没有**（`Cannot load default config file`） |
| 系统字体 | `C:/Windows/Fonts/msyh.ttc`、`C:/Windows/Fonts/NotoSansSC-VF.ttf` |
| 非 ASCII 名字体 | `%LOCALAPPDATA%/Microsoft/Windows/Fonts/乐米波波体（免费商用）_爱给网_aigei_com.ttf` |

---

## T1：非 ASCII 字体路径不再静默失败

### 缺陷（先复现，再修）

同一份字体、同一行文字、同一个字号，只改**路径的写法**：

```bash
printf '笑靥如花' > t.txt
FONT='.../乐米波波体（免费商用）_爱给网_aigei_com.ttf'

# A：原始非 ASCII 路径
ffmpeg -v error -nostdin -f lavfi -i "color=c=black@0.0:s=400x60,format=rgba" \
  -vf "drawtext=fontfile='${FONT}':textfile='t.txt':fontsize=40:fontcolor=white:\
expansion=none:x=(w-text_w)/2:y=(h-text_h)/2" \
  -frames:v 1 -f rawvideo -pix_fmt rgba - > a.raw 2> a.err

# B：同一份字体复制成 ASCII 名
cp "$FONT" lemi_ascii.ttf   # 然后把上面那句的 fontfile 换成 lemi_ascii.ttf
```

| | A（原始非 ASCII） | B（ASCII 拷贝） |
|---|---|---|
| 退出码 | **139**（SIGSEGV） | 0 |
| 产出字节 | **0** | **96000** = 400×60×4 |
| 非零 alpha 像素 | 0 | **3184** |
| stderr | `Fontconfig error: Cannot load default config file: File not found` | 空（0 字节） |

A 连跑 3 次：`139 / 139 / 139`，每次都 0 字节——**稳定复现**。

### 为什么这条最坏：它连"退出码非零"都不一定给

上表里 A 是**段错误**（139），所以 `run_ffmpeg` 里"退出码不是 0 就报错"那条**接得住**它。

但同一份探针在别的形态下给的是 `rc=0 && 0 字节`（任务书里记的那一版）——
**真正接住这一个缺陷的是"产出字节数不对就报错"那一条**，不是退出码那一条。
本仓在这条路上的安全性**建立在字节数检查上**，所以下面专门有一条反向用例盯着它。

### 修法

`crates/dhampir-worker/src/text_raster.rs`：路径含非 ASCII 字节时，把字体**复制**到
一份 ASCII 名的临时路径再用（`ascii_font_path` / `stage_font_file`），用完由
`StagedFont` 的 `Drop` 删掉。

* 临时名 = `dhampir-font-{pid}-{内容 FNV-1a}{扩展名}`：内容进名字，
  于是"换一份字体"与"同一份字体"在参数串上可区分，且残留文件可识别。
* 用 **copy 不用 hard_link**：Windows 上跨卷硬链会失败，而临时目录与字体目录常常不同卷。
  一份中文字体约 2.5 MB，搬一次的代价远小于一次 ffmpeg 进程。

### 验收数据

**1）老工程逐字节不变（T1 的硬前提）**

```bash
dhampir frame --project fixtures/sample-project.doc.json --frame 15 --out <dir>
```

| | sha256 |
|---|---|
| 改动前（HEAD = 97c3d8e） | `91ac70187a5d3fdd7d01a568effa7f7df7463783c0623b583bd6a8aeca3ceff2` |
| T1 改动后 | `91ac70187a5d3fdd7d01a568effa7f7df7463783c0623b583bd6a8aeca3ceff2` |

**逐字节相同**。判据 `不画阴影时参数串逐字符与改动前相同`（`frozen_argv`）继续绿。

**2）非 ASCII 路径真的画得出来（端到端）**

```bash
dhampir frame --project fixtures/sample-subtitle.doc.json --frame 15 \
  --out <dir> --font-file '...\乐米波波体（免费商用）_爱给网_aigei_com.ttf'
```

* `failed: false`、`lines_failed: 0`、`lines_drawn: 1`、`cache_misses: 3`
* 出图 sha256 `6712558a430a93cecde4cc92bd7007f6d09e792404aba096f7b918076b3062a3`
* 目视：笔画正常，无 .notdef 方框
* 对照（同一帧换 ASCII 名体的 `msyh`）：`failed: false, lines_failed: 0`，
  sha256 `cbf4754cc8d2aaf83cb4ad6401a39922385230f0c219bc16e5eeafe0dba0c91a`
  —— 两份**不同**是对的：不同的字体本来就该画出不同的像素。
  这条只证"两条路都能跑通"，不证"像素一致"。

**3）临时文件清干净**

跑完上面那条端到端之后，`$TEMP/dhampir-font-*` 计数 = **0**。

### 反向用例（会红的那一半）

| 变异 | 预期红的那条判据 | 实测 |
|---|---|---|
| 让 ASCII 路径也走复制（`is_ascii() && false`） | `ascii_字体路径不搬_这一条是默认路径字节冻结` | **红**（29 过 / 1 挂） |
| 默认滤镜串里多塞一个字符（`fixbounds=none`） | `不画阴影时参数串逐字符与改动前相同` | **红**（29 过 / 1 挂） |
| 源文件不存在 | `搬字体失败要响亮报错而不是递一个空路径` | **绿**（在修好的实现上） |

两个变异各自**只**挂掉那一条判据，没有连带红——说明判据是**对着**那件事的。

**一条要如实记下的边界**：变异"搬了却仍用 `key.font_file`"（把搬出来的路径丢掉）
**不被任何断言接住** —— 它只是编译出一个 `unused variable: font` 警告。第一版实现里
这是个真的静默错法。改法不是补一条判据（那要注入一个钩子），而是**让它写不出来**：
`ascii_font_path` 返回 `StagedFont`，路径只从 `StagedFont::path()` 出，
调用方**没有第二条路径可选**。这条纪律记在这里，将来重构这一段时别退回去。

### 一条环境限制（不是产品缺陷）

DSH 的文件沙箱会让**位于工作区内的可执行文件写不了 `%TEMP%`**
（`拒绝访问 (os error 5)`）。`cargo test` 的产物在 `crates/*/target/` 里，正好撞上，
所以两条**造源文件 / 看临时目录**的单测在这个环境里拿不到草稿目录。

处理办法是 `writable_scratch_dir()`：**真去试着建一次**，建不了就打印原因并如实跳过
（返回 `None`），而不是让一条环境差异伪装成"实现坏了"。

**这条限制吃掉的是测试的覆盖，不是产品的行为**——产品路径在**同一台机器**上验过：
把 `dhampir.exe` 拷到工作区外再跑，非 ASCII 字体路径端到端出图成功（见上"验收数据 2"）。
在不受沙箱约束的环境里，那两条单测会真正执行。

---

## T2 / T3 / T4

（各自完成时补在这里。）
