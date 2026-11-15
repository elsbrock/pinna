# tract-tflite 0.22.5, patched

The crate as published on crates.io (upstream commit `8010058209c8a3f4d39dcafc4c292e263e8e458c`,
`tflite/` in sonos/tract), used through `[patch.crates-io]` in the workspace `Cargo.toml`. It lets
`pinna-wakeword` run openWakeWord classifiers shipped as `.tflite` (ADR 0013).

The first commit adds it unchanged; `git log -p -- vendor/tract-tflite` shows the changes:

1. **Negative reduce axes** (`src/ops/nn.rs`, `de_reduce`, `de_reduce_mean`). An axis of `-1` was cast to
   `usize` and panicked. Backported from tract 0.23, which counts it from the end.
2. **Float `MEAN` no longer rounds** (`src/ops/nn.rs`, `de_reduce_mean`). The sum was divided with
   `quant::scale()`, which rounds to an integer, so a LayerNorm's mean and variance came out as whole
   numbers. Floats now use a plain multiply; quantized tensors keep `scale`.
3. **`SQUARED_DIFFERENCE`** (`src/ops/math.rs`) is read as a subtraction followed by a square.

With these, tract matches Google's LiteRT to about 1e-6 on `alexa_v0.1.tflite`, `okay_nabu.tflite`
and a community-trained model (`crates/pinna-wakeword/tests/parity_tflite.rs`).

Remove this copy once a tract release has 2 and 3; tract 0.23 already has 1.
