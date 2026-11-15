#!/usr/bin/env bash
# Fetch the wake word models into blobs/models/openwakeword-0.5.1/ and verify
# their sha256. Nothing here may be committed (blobs/ is gitignored):
#   - openWakeWord v0.5.1 release: the mel spectrogram and embedding models,
#     and the Alexa, Hey Jarvis, Hey Mycroft and Hey Rhasspy classifiers.
#     CC BY-NC-SA 4.0.
#   - rhasspy/pyopen-wakeword, pinned to a commit: the Okay Nabu classifier
#     (.tflite only), from a repository under Apache-2.0.
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
echo "models ready in $dest"
