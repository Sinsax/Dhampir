# 底座调用面清单

**这份文件是生成的**（`node scripts/api-surface.mjs --write`）。
手写清单一定会漂，所以由 `scripts/api-surface.mjs` 生成并用守卫钉住。

## 怎么读

| 分类 | 含义 |
|---|---|
| **底座 API** | 下游会依赖。**只有这部分承诺兼容**；形状变化要升版本。 |
| 宿主内部 | 宿主自己用。可用，但不构成渲染契约。 |
| 弃用 | 已被取代。新代码不要用。 |
| 取证工具 | 本仓库验收用。**不承诺兼容**。 |

## 底座 API

### `demux_wasm.rs`

按帧号定位同步样本（帧精确的前提）。

- `dhampir_demux_description`
- `dhampir_demux_frame_index`
- `dhampir_demux_gop_slices`
- `dhampir_demux_open`
- `dhampir_demux_samples`
- `dhampir_demux_sync_start`

### `timeline_host.rs`

下游会依赖：工程载入/校验/求值/上屏/素材绑定。**这部分才承诺兼容。**

- `dhampir_project_attach`
- `dhampir_project_bind_source`
- `dhampir_project_draw`
- `dhampir_project_end_frame`
- `dhampir_project_first_frame`
- `dhampir_project_frame`
- `dhampir_project_open`
- `dhampir_project_precheck`
- `dhampir_project_render_probe`
- `dhampir_project_resize`
- `dhampir_project_sources_for`
- `dhampir_sample_project_render_png`

## 宿主内部

### `cache_wasm.rs`

帧缓存记账。宿主自己用，**不是渲染契约的一部分**。

- `dhampir_cache_note_ram`
- `dhampir_cache_note_vram`
- `dhampir_cache_open`
- `dhampir_cache_ram_budget`
- `dhampir_cache_remove_ram`
- `dhampir_cache_remove_vram`
- `dhampir_cache_stats`
- `dhampir_cache_texture_capacity`
- `dhampir_cache_touch_ram`
- `dhampir_cache_touch_vram`
- `dhampir_cache_vram_budget`

## 取证工具

### `corpus.rs`

M2 的语料与记录。**不承诺兼容。**

- `dhampir_corpus_frame_png`
- `dhampir_corpus_open`
- `dhampir_corpus_run`
- `dhampir_corpus_scene_names`

### `web.rs`

M0–M2 的探针与 corpus。**不承诺兼容**，供本仓库验收用。

- `dhampir_fnv1a64_hex`
- `dhampir_probe_digest_hex`
- `dhampir_probe_format_version`
- `dhampir_probe_golden_check`
- `dhampir_probe_golden_digest_hex`
- `dhampir_probe_init_canvas`
- `dhampir_probe_line_count`
- `dhampir_probe_offscreen_png`
- `dhampir_probe_offscreen_timing`
- `dhampir_probe_render_canvas`
- `dhampir_probe_report`
- `dhampir_probe_verify`

## 弃用

### `preview.rs`

M3 的单片段预览，已被 timeline_host 取代。**新代码不要用。**

- `dhampir_preview_draw`
- `dhampir_preview_init`
- `dhampir_preview_probe_digest`
