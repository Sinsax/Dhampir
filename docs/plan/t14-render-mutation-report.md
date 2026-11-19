# Rendering-layer mutation self-proof - T1/T3/T4 (eval layer -> TextItem)

These criteria were forced by a real end-to-end pixel measurement:
all `text_motion` function-level criteria were green while T1 did NOT move a single
pixel in the encoded output - because the render side takes glyph size from
`font_ratio` and never reads `scale`. Hence criteria must watch `font_ratio`.

Baseline: `cargo test -p dhampir-core --lib` -> 35 passed / 0 failed (overlay module).

| mutation | what was broken | result | criterion that caught it |
|---|---|---|---|
| `P1` | font_ratio does NOT multiply item_scale (the real shipped bug) | RED | `弹入窗口内字号随帧变化而窗口外回到单位`, `弹入缩放与逐条强调相乘`, `逐条强调缩放乘进字号` |
| `P2` | item_scale drops enter_scale (T1 dead) | RED | `弹入窗口内字号随帧变化而窗口外回到单位`, `弹入缩放与逐条强调相乘` |
| `P3` | item_scale drops cue.scale (T3 dead) | RED | `弹入缩放与逐条强调相乘`, `逐条强调缩放乘进字号` |
| `P4` | T3 overrides instead of multiplying | RED | `弹入缩放与逐条强调相乘` |
| `P5` | shake not added to dy_px | RED | `抖动落到偏移字段上而关掉时为零` |
| `P6` | shake not applied to dx_px | RED | `抖动落到偏移字段上而关掉时为零` |

## Conclusion
- **6/6 mutations turned red**, including `P1` (the bug that actually shipped).
- `P5`/`P6` closed a gap found by this very harness (`shake not wired up`).