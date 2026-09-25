# 五分钟跑通 dhampir

这份文件只回答一个问题：**从零到看见画面、拿到一个 mp4，最短的路是什么。**
不讲设计理由（那在 [../plan/](../plan/)），不列全部开关（那在 [usage.md](usage.md)）。

---

## 一、装什么

| 需要 | 用途 | 没有它会怎样 |
|---|---|---|
| **Rust 1.97.0** | 全部构建 | `rust-toolchain.toml` 会自动切到它 |
| **ffmpeg / ffprobe** | 素材探测、字幕栅格化、出片编码 | `info` / `gop` / `render` 直接失败 |
| **GPU**（Vulkan / DX12 / Metal） | `frame` / `render` 的合成 | `probe` 能跑，出图不能 |
| Chrome / Edge | 浏览器预览 | 只有预览用得到 |

**字体不在这个表里**：要烧字幕就自己给 `--font-file`。本仓不内嵌字体、也不猜系统字体 ——
静默出一份没有字幕的片子比报错难查得多。

---

## 二、确认工具链是活的（不需要工程文件）

```bash
cargo check --workspace
cargo check -p dhampir-wasm --target wasm32-unknown-unknown
#   注意：wasm 侧不能带 --workspace —— dhampir-worker 是 native-only

cargo test --workspace                     # native 全量
node scripts/run-wasm-tests.mjs            # wasm32 运行时

cargo run -q -p dhampir-worker --bin dhampir -- --help
```

最后一条能打出帮助，就说明 CLI 是活的。**`--help` 永远比文档新** —— 子命令的权威说明在它那里。

---

## 三、出一份片（单机，CLI）

样本工程与素材都在仓库里，不用自己准备：

```bash
# 1. 先看工程能不能过校验（不要 GPU、不要 ffmpeg）
cargo run -q -p dhampir-worker --bin dhampir -- probe --project fixtures/sample-project.doc.json

# 2. 出一帧 PNG 看看画面（要 GPU）
cargo run -q -p dhampir-worker --bin dhampir -- frame \
  --project fixtures/sample-project.doc.json --frame 42 --out ./out

# 3. 出一段 mp4（要 GPU + ffmpeg）
cargo run -q -p dhampir-worker --bin dhampir -- render \
  --project fixtures/sample-project.doc.json --from 0 --to 89 --out out.mp4
```

出片时 **stdout 是 NDJSON 进度流**（一行一个对象：`start` → 若干 `progress` → `done`），
不是给人读的日志。诊断一律走 stderr。

**退出码只有三种**：`0` 成功 / `2` 用法或校验错（**你能改的**）/ `1` 运行期失败。
写脚本时判退出码，不要 grep 日志里的 `ok`。

---

## 四、看见画面（浏览器）

浏览器预览与出片**共用同一个 `dhampir-core`** —— 这是本仓库的核心命题：
同一份工程，两边给出**可比的帧**。

```bash
node scripts/web-check.mjs --serve --local
```

它会打印一个 URL（形如 `http://127.0.0.1:5xxxx/?backend=local&port=8802&project=sample-project.doc`），
**静态服务与本机后端都已经起好了**，直接用浏览器打开那个 URL。

- 改前端代码后**刷新页面**即可（服务带 `no-store`，不吃缓存）；
- 结束用 **Ctrl+C**；
- **不要把这个命令的输出接到 `| Select-Object` / `| head`** —— 管道提前关闭会让它立刻退出。

界面上能做的事：播放（空格）、拖动播放头、左右拖元素边缘改时长、
拖元素移动位置、剃刀切分、撤销重做、音量与静音（`M`）。

---

## 五、跑一遍验收

```bash
node scripts/run-guards.mjs          # 全部守卫（每条先自检、再正跑）
node scripts/run-guards.mjs --list   # 只列清单，看一共几条
```

**判定以退出码为准。** 守卫报红时先看它自己的 `--self-test`：
自检红 = 守卫自己坏了，先修守卫，别信它的结论。

---

## 六、接下来读什么

| 你想 | 去哪 |
|---|---|
| 每个开关、每种用法 | [usage.md](usage.md) |
| 调用面（wasm 导出 / CLI 子命令） | [api.md](api.md) |
| 宿主 API 的形状与版本 | [host-api.md](host-api.md) |
| 现在做到哪、还剩什么 | [../plan/next-steps.md](../plan/next-steps.md) |
| 为什么这样设计 | [../plan/video-editor-plan.md](../plan/video-editor-plan.md) |

---

## 七、卡住了先看这三条

1. **`render` 报 ffmpeg / GPU 相关** —— 先跑 `probe`（它不要 GPU）。probe 过、frame 不过，
   问题就在 GPU 或编码器，不在工程。
2. **`wasm-bindgen` 版本不一致** —— 看着像构建坏了，其实是 crate 与 CLI 的版本没对齐。
   见 [usage.md 的常见问题](usage.md#常见问题)。
3. **守卫在受限会话里报红** —— 有些环境不给子进程开 stdin 管道，渲染那条路必然红。
   那是**环境**的读数，不是仓库的。去普通终端重跑。
