# tract-tflite 0.23.10, patched

The crate as published on crates.io (upstream commit `6c392b8028fde233e433afd250067125f7c83a22`,
`tflite/` in sonos/tract), used through `[patch.crates-io]` in the workspace `Cargo.toml`. It lets
`pinna-wakeword` run openWakeWord classifiers shipped as `.tflite` (ADR 0013).

The commit that moved it to 0.23.10 adds it unchanged; `git log -p -- vendor/tract-tflite` shows the
change since:

1. **`SQUARED_DIFFERENCE`** (`src/ops/math.rs`) is read as a subtraction followed by a square, as a
   LayerNorm's variance uses it. `crates/pinna-wakeword/tests/tflite_ops.rs` builds a model with the
   operator and fails without this patch.

0.22.5 also needed negative reduce axes and a float `MEAN` that doesn't round to an integer; 0.23 has
both.

0.23.10's package carries no licence files. `LICENSE-APACHE` and `LICENSE-MIT` are the ones from
0.22.5's package: the licence is the same, `MIT OR Apache-2.0`.

Remove this copy once a tract release reads `SQUARED_DIFFERENCE`.
