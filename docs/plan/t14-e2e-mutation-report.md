# End-to-end pixel-mutation self-proof (text_enter.rs)

Each entry breaks ONE thing and runs the REAL GPU/ffmpeg pixel test:

    TMPDIR=<in-repo> TEMP=... TMP=... cargo test -p dhampir-worker --test text_enter -- --ignored

Baseline: 3 passed / 0 failed.

| mutation | what was broken | result | e2e criterion that caught it |
|---|---|---|---|
| `E1` | revert the real fix: font_ratio does not multiply item_scale (the shipped bug) | RED | - |
| `E2` | T4 shake not wired to dy_px/dx_px | RED | - |

## Why this file exists
The `E1` mutation IS the bug that actually shipped: all unit tests were green
while the encoded output did not move a single pixel. Only this layer sees it.