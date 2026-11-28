# Third-party notices

Parts of Pinna are ports of, or copies from, the MIT-licensed user-space code
of LibreEcho (https://github.com/aslater3/LibreEcho-UI), open-source firmware
for the Echo (2nd gen). Its copyright notice and permission notice follow, as
the license requires.

Covered here:

- Rust ports of its user-space daemons and DSP: the beamformer, beam snapshot,
  voice DSP and VAD, capture mixer sequences (`crates/pinna-audio`), LED
  rendering and animations, button and privacy
  switch handling (`crates/pinna-hal`, `crates/pinnad`), wake word detection
  rules (`crates/pinna-wakeword`) and the ESPHome entity set
  (`crates/pinna-esphome`).
- The speech and music recordings in `crates/pinna-wakeword/tests/fixtures/`
  and the reference scoring script in `crates/pinna-wakeword/tests/parity/`.

```
MIT License

Copyright (c) 2026 LibreEcho contributors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

## TECHO5

Parts of Pinna's Echo support follow TECHO5
(https://github.com/HuskerMinion/techo5), MIT-licensed firmware work for Echo
devices. Its copyright notice and permission notice follow, as the license
requires.

Covered here:

- The Echo Dot (5th gen)'s audio paths and values taken from TECHO5's
  `paths_cannoli.go` and its discussion #610: the mixer sequences that route
  the microphones and the speaker without Amazon's DSP
  (`crates/pinna-audio/src/mixer.rs`), the speaker's volume curve, the
  microphone gain, positions and lanes, and the button codes
  (`crates/pinna-board`), the ring's LED order (`crates/pinna-hal/src/led.rs`,
  `crates/pinna-bootctl/src/ring.rs`) and the lane layout of its PipeWire
  graph (`mkosi/mkosi.extra/etc/pipewire/pinna-echo-dot-5.conf.d/`).
- The microphone benchmark in `crates/pinna-wakeword/examples/micbench/` and
  the Echo Dot (3rd gen) values that ADR 0031 and its kernel patches cite.

```
MIT License

Copyright (c) 2026 TECHO5 contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Other licenses

- `kernel/patches/`: changes to the Linux kernel, GPL-2.0 like the kernel
  itself; each added file carries its own SPDX header and copyright lines.
- `tools/wmt/`: GPL-2.0, see the SPDX headers in the files.
- The playback volume curve (`crates/pinna-audio/src/volume.rs`) and the
  output mixer settings in `crates/pinna-audio/src/mixer.rs` take a formula
  and mixer values from `audio_engine.c` in LibreEcho's platform tree
  (https://github.com/aslater3/LibreEcho-Platform).
- Release images and the device kernel carry Amazon's MT8163 Wi-Fi and
  Bluetooth firmware files from the Echo Dot (2nd gen)'s stock Fire OS and the
  microphone FPGA bitstream (`i2s_to_spi_v34.bin`), unmodified. The
  repository doesn't contain them; `just firmware` fetches them during the
  build, see `scripts/fetch-firmware.sh`.
- Release images carry the wake word models
  (`/usr/share/pinna/models/openwakeword-0.5.1/`) with a `NOTICE` next to
  them: the openWakeWord v0.5.1 models (CC BY-NC-SA 4.0) and Okay Nabu,
  from a pinned `rhasspy/pyopen-wakeword` (an Apache-2.0 repository; the
  model file carries no license of its own). The repository doesn't contain
  them; `just models` fetches them during the build.
- Release images carry the speaker model
  (`/usr/share/pinna/models/speaker/`) with a `NOTICE` next to it: 3D-Speaker's
  CAM++ (`speech_campplus_sv_zh_en_16k-common_advanced`, Apache-2.0), as
  sherpa-onnx exports it. Pinna stores its weights as float16
  (`scripts/speaker-f16.py`); the model computes as before. The repository
  doesn't contain it; `just models` fetches and converts it during the build.
