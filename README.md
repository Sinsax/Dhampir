# dhampir

渲染与预览的共用底座：同一个 `dhampir-core`，编译到两个宿主。

| | |
|---|---|
| 底座 | Rust + wgpu |
| preview | wasm32 宿主（浏览器 WebGPU + WebCodecs） |
| render | native 宿主（headless wgpu） |

命题一句话：同一份工程，浏览器里看到的与服务器上导出的，是**可比的帧**。
素材与出片在服务器，浏览器只做剪辑编辑。网关 / 任务队列 / 对象存储 / 部署 / 分片编排 /
编辑 UI 产品化**不在本仓库**。

## 版本

`0.4.1` —— 唯一真相是根 `Cargo.toml` 的 `[workspace.package] version`。

| 号 | 管什么 |
|---|---|
| `version` | 产品版本：产物名、Release 资产名 |
| `project_schema = 1` | 工程文件能不能读 |
| `host_api = 7` | wasm 导出面对不对得上（只增不减） |

产物 = `dhampir-<版本>-<平台>.zip` + `.sha256.txt` + `.manifest.json`，
平台 `win32-x64` / `linux-x64`，各平台原生构建（不交叉编译）。
发布 = 推 `v*` tag → `.github/workflows/release.yml` 门控、打包、建 Release，正文取自
[CHANGELOG.md](CHANGELOG.md) 对应版本段；手工打包 `node scripts/package.mjs`。

## 上手

```bash
cargo test --workspace
node scripts/run-guards.mjs
cargo run -q -p dhampir-worker --bin dhampir -- --help
```

## 文档

| 想 | 看 |
|---|---|
| 五分钟出片、开预览 | [docs/quickstart.md](docs/quickstart.md) |
| 全部用法与开关 | [docs/usage.md](docs/usage.md) |
| 承诺边界与调用入口 | [docs/api.md](docs/api.md) |
| 宿主 API 的形状与版本 | [docs/host-api.md](docs/host-api.md) |
| wasm 导出全名单（生成） | [docs/api-surface.md](docs/api-surface.md) |
| 下游升级要改什么 | [CHANGELOG.md](CHANGELOG.md)（0.4.0 起 `host_api` 6→7）；历史对接：[0.2.0→0.3.0](docs/upgrade-0.2.0-to-0.3.0.md)、[0.1.0→0.2.0](docs/upgrade-0.1.0-to-0.2.0.md) |
| 接下来做什么 / 缺陷台账 | [plan/next-steps.md](plan/next-steps.md)、[plan/defects.md](plan/defects.md) |
| 为什么这样设计 | [plan/video-editor-plan.md](plan/video-editor-plan.md) |

## 结构

```
crates/
  dhampir-timeline/   帧号 ↔ 时间码（纯整数）、自检、golden 报告
  dhampir-media/      编解码契约 trait（零实现）
  dhampir-core/       渲染、GPU 抽象、读回、PNG 编码；零 #[cfg]
  dhampir-wasm/       浏览器宿主（wasm32）
  dhampir-worker/     native 宿主：dhampir CLI + dhampir-render
web/                  浏览器预览（无打包器）
docs/                 用法与调用面
plan/                 计划与决策
records/              里程碑证据
scripts/              守卫、测量、打包、本地服务
```

依赖单向无环：`timeline ← media`、`timeline ← core`；`wasm` 与 `worker` 各依赖
`core + media`，互不依赖。

五条铁律：整数帧号，不用浮点秒；帧率是有理数（29.97 = `30000/1001`）；特效是
「类型 + 参数」的声明式数据；按 WebGPU 能力下限写；SSIM 判回归必须设容差，容差表独立成文件。

## 验证

```bash
node scripts/run-guards.mjs        # 30 条守卫，各带 --self-test
node scripts/run-guards.mjs --list # 名单以此为准，不抄文档
node scripts/run-wasm-tests.mjs    # wasm32 运行时
node scripts/record-acceptance.mjs --milestone m0   # 留取证记录（m1 / m2 同理）
```

## 许可证

**Apache-2.0** —— [LICENSE](LICENSE)（与 [LICENSE-APACHE](LICENSE-APACHE) 同一份）。
第三方依赖清单由 `node scripts/licenses.mjs --check` 钉住：
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md)。
