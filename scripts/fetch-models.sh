#!/usr/bin/env bash
# Fetch the openWakeWord v0.5.1 ONNX models into blobs/models/openwakeword-0.5.1/
# and verify their sha256. The models are CC BY-NC-SA 4.0 and must never be
# committed (blobs/ is gitignored).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${1:-$root/blobs/models/openwakeword-0.5.1}"
base=https://github.com/dscripka/openWakeWord/releases/download/v0.5.1

models=(
  "melspectrogram.onnx ba2b0e0f8b7b875369a2c89cb13360ff53bac436f2895cced9f479fa65eb176f"
  "embedding_model.onnx 70d164290c1d095d1d4ee149bc5e00543250a7316b59f31d056cff7bd3075c1f"
  "alexa_v0.1.onnx 6ff566a01d12670e8d9e3c59da32651db1575d17272a601b7f8a39283dfbae3e"
)

mkdir -p "$dest"
for entry in "${models[@]}"; do
  read -r name sha <<<"$entry"
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
