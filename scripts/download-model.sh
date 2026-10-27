#!/usr/bin/env bash
# Downloads the Whisper large-v3-turbo ggml model (~1.6 GB), the Silero VAD
# model (~1 MB) and the speaker-diarization models (~33 MB: pyannote
# segmentation + WeSpeaker embeddings) into mostalgia's data directory,
# ${XDG_DATA_HOME:-~/.local/share}/mostalgia/models. Models already in the
# checkout's models/ folder (older layout) are moved there instead.
set -euo pipefail
cd "$(dirname "$0")/.."
DEST="${XDG_DATA_HOME:-$HOME/.local/share}/mostalgia/models"
mkdir -p "$DEST"

fetch() {
  local name=$1 url=$2
  if [[ -s "$DEST/$name" ]]; then
    echo "$DEST/$name already present, skipping"
  elif [[ -s "models/$name" ]]; then
    echo "Moving models/$name to $DEST/"
    mv "models/$name" "$DEST/$name"
  else
    echo "Downloading $name …"
    curl -fL --progress-bar -o "$DEST/$name.part" "$url"
    mv "$DEST/$name.part" "$DEST/$name"
  fi
}

fetch ggml-large-v3-turbo.bin "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin"
fetch ggml-silero-v5.1.2.bin "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v5.1.2.bin"
fetch wespeaker_en_voxceleb_resnet34_LM.onnx "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx"
if [[ ! -s "$DEST/pyannote-segmentation-3.0.onnx" ]]; then
  echo "Downloading pyannote segmentation model …"
  tmp=$(mktemp -d)
  curl -fL --progress-bar -o "$tmp/seg.tar.bz2" \
    "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2"
  tar -xjf "$tmp/seg.tar.bz2" -C "$tmp"
  mv "$tmp"/sherpa-onnx-pyannote-segmentation-3-0/model.onnx "$DEST/pyannote-segmentation-3.0.onnx"
  rm -rf "$tmp"
fi
rmdir models 2>/dev/null || true
echo "Done: models are in $DEST"
