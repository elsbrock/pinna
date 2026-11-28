#!/usr/bin/env bash
# Fetch the wake word models into blobs/models/openwakeword-0.5.1/ and verify
# their sha256. Nothing here may be committed (blobs/ is gitignored):
#   - openWakeWord v0.5.1 release: the mel spectrogram and embedding models,
#     and the Alexa, Hey Jarvis, Hey Mycroft and Hey Rhasspy classifiers.
#     CC BY-NC-SA 4.0.
#   - rhasspy/pyopen-wakeword, pinned to a commit: the Okay Nabu classifier
#     (.tflite only), from a repository under Apache-2.0.
# And the speaker model into blobs/models/speaker/: 3D-Speaker's CAM++, as
# sherpa-onnx exports it, with its weights stored as float16 by
# scripts/speaker-f16.py (needs uv). Apache-2.0.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:-$root/blobs/models/openwakeword-0.5.1}"
oww=https://github.com/dscripka/openWakeWord/releases/download/v0.5.1
pyoww=https://raw.githubusercontent.com/rhasspy/pyopen-wakeword/6bc5c5f5c9c71e46a723b6c9277b1d50f2ba13fd/pyopen_wakeword/models

models=(
  "melspectrogram.onnx ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f $oww"
  "embedding_model.onnx 70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f $oww"
  "alexa_v0.1.onnx 6ff566a01d12670e8d9e3c59da32651db1575d17272a601b7f8a39283dfbae3e $oww"
  "hey_jarvis_v0.1.onnx 94a13cfe60075b132f6a472e7e462e8123ee70861bc3fb58434a73712ee0d2cb $oww"
  "hey_mycroft_v0.1.onnx c2a311e8fa1338de89c31b3b46dc4dffd4af2f9a8d6ddead48893c2d301b1f18 $oww"
  "hey_rhasspy_v0.1.onnx 5a9b3ed3be2910e35780e097905aa9f35a9c10038df47914cf2b3ec4d670f6ea $oww"
  "okay_nabu.tflite 2982cecde4ee81cc7a2573d2602a7d54f0669425c94a7b64af77e0ff92b03a18 $pyoww"
)

mkdir -p "$dest"
for entry in "${models[@]}"; do
  read -r name sha base <<<"$entry"
  file="$dest/$name"
  if [[ -f $file ]] && [[ "$(sha256sum "$file" | cut -d' ' -f1)" == "$sha" ]]; then
    echo "ok       $name"
    continue
  fi
  echo "fetching $name"
  curl -fsSL --retry 3 -o "$file.part" "$base/$name"
  got="$(sha256sum "$file.part" | cut -d' ' -f1)"
  if [[ $got != "$sha" ]]; then
    rm -f "$file.part"
    echo "sha256 mismatch for $name: expected $sha, got $got" >&2
    exit 1
  fi
  mv "$file.part" "$file"
done

# The models' sources and licenses travel with them (ADR 0015).
oww_files=""
pyoww_files=""
for entry in "${models[@]}"; do
  read -r name _ base <<<"$entry"
  if [ "$base" = "$oww" ]; then
    oww_files=${oww_files:+$oww_files, }$name
  else
    pyoww_files=${pyoww_files:+$pyoww_files, }$name
  fi
done
cat > "$dest/NOTICE" <<EOF
openWakeWord v0.5.1 models
Files: $oww_files
Source: https://github.com/dscripka/openWakeWord/releases/tag/v0.5.1
Author: David Scripka and the openWakeWord contributors
License: Creative Commons Attribution-NonCommercial-ShareAlike 4.0
         International (CC BY-NC-SA 4.0),
         https://creativecommons.org/licenses/by-nc-sa/4.0/
The files are distributed unchanged. They may not be used commercially.

Okay Nabu
Files: $pyoww_files
Source: https://github.com/rhasspy/pyopen-wakeword/tree/6bc5c5f5c9c71e46a723b6c9277b1d50f2ba13fd
License: the model file carries no license of its own; its repository is
         Apache-2.0, https://www.apache.org/licenses/LICENSE-2.0
The file is distributed unchanged.
EOF
echo "models ready in $dest"

# The speaker model (ADR 0038). Upstream ships float32 only; the image carries
# the float16 conversion, which is deterministic, so its sha256 is pinned too.
speaker_dir="$root/blobs/models/speaker"
speaker_src=3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx
speaker_src_sha=53cab20ba8f2eb2f6e2d89ecc18e88ef3222169b61b4419cbc13666744a4574b
speaker=3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced-f16.onnx
speaker_sha=1dc2529e7b1f2f38105a158e4105d350f1e9367cc8102c65b5a2312a90792d62
speaker_url=https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models

mkdir -p "$speaker_dir"
if [[ -f $speaker_dir/$speaker ]] && [[ "$(sha256sum "$speaker_dir/$speaker" | cut -d' ' -f1)" == "$speaker_sha" ]]; then
  echo "ok       $speaker"
else
  command -v uv >/dev/null || { echo "uv not found: it converts the speaker model to float16" >&2; exit 1; }
  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  echo "fetching $speaker_src"
  curl -fsSL --retry 3 -o "$work/$speaker_src" "$speaker_url/$speaker_src"
  got="$(sha256sum "$work/$speaker_src" | cut -d' ' -f1)"
  if [[ $got != "$speaker_src_sha" ]]; then
    echo "sha256 mismatch for $speaker_src: expected $speaker_src_sha, got $got" >&2
    exit 1
  fi
  echo "converting to $speaker"
  uv run -q --no-project --python 3.12 --with numpy==1.26.4 --with onnx==1.17.0 \
    python -I "$root/scripts/speaker-f16.py" "$work/$speaker_src" "$work/$speaker"
  got="$(sha256sum "$work/$speaker" | cut -d' ' -f1)"
  if [[ $got != "$speaker_sha" ]]; then
    echo "sha256 mismatch for $speaker: expected $speaker_sha, got $got" >&2
    exit 1
  fi
  mv "$work/$speaker" "$speaker_dir/$speaker"
fi
cat > "$speaker_dir/NOTICE" <<EOF
CAM++ speaker verification model
File: $speaker
Model: 3D-Speaker's CAM++, speech_campplus_sv_zh_en_16k-common_advanced,
       https://modelscope.cn/models/iic/speech_campplus_sv_zh_en_16k-common_advanced
Export: sherpa-onnx, $speaker_url/$speaker_src
Authors: the 3D-Speaker authors (Alibaba DAMO Academy), and the sherpa-onnx
         contributors for the ONNX export
License: Apache License 2.0, https://www.apache.org/licenses/LICENSE-2.0
Changes: Pinna stores the float32 weights as float16, with casts back to
         float32 (scripts/speaker-f16.py). The model computes as before.
EOF
echo "speaker model ready in $speaker_dir"
