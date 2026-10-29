#!/usr/bin/env bash
# Builds dist/Mostalgia-windows-x86_64.zip: the app and the `mostalgia` CLI,
# the sherpa-onnx DLLs (speaker diarization) and the Visual C++ runtime
# DLLs, in one folder that runs where it's unpacked. With $VULKAN_SDK set
# it's the Vulkan build (Whisper on the GPU, through the GPU driver's own
# vulkan-1.dll); without, the CPU build. Models are not included; the app
# downloads them on first launch.
#
#   bash scripts/build-windows.sh       # Git Bash, with MSVC, CMake and LLVM
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
OUT="$ROOT/target/Mostalgia"
ZIP="dist/Mostalgia-windows-x86_64.zip"
mkdir -p dist

features=()
if [[ -n "${VULKAN_SDK:-}" ]]; then
  echo "Vulkan SDK at $VULKAN_SDK: building with Vulkan"
  features=(--features vulkan)
else
  echo "No Vulkan SDK: CPU build"
fi
# In CI, compiler errors also become annotations.
run=()
[[ -n "${GITHUB_ACTIONS:-}" ]] && run=(bash scripts/ci-cargo.sh)
if ! "${run[@]}" cargo build --release "${features[@]}" -p mostalgia-app -p mostalgia; then
  [[ ${#features[@]} -gt 0 ]] || exit 1
  # A zip without GPU support beats no zip.
  echo "::warning::The Vulkan build failed; packaging the CPU build"
  "${run[@]}" cargo build --release -p mostalgia-app -p mostalgia
fi

rm -rf "$OUT"
mkdir -p "$OUT"
cp target/release/mostalgia-app.exe target/release/mostalgia.exe "$OUT/"

# The sherpa-onnx DLLs: sherpa-rs copies them next to the binaries, but
# only when its build script runs; a build restored from a cache has them
# only in sherpa-rs's download cache.
found=0
for dir in "$ROOT/target/release" "${LOCALAPPDATA:-$HOME/AppData/Local}/sherpa-rs"; do
  [[ -d "$dir" ]] || continue
  while IFS= read -r dll; do
    cp -n "$dll" "$OUT/"
    found=1
  done < <(find "$dir" -maxdepth 4 \( -iname 'sherpa-onnx*.dll' -o -iname 'onnxruntime*.dll' \) 2>/dev/null)
  [[ $found == 1 ]] && break
done
[[ $found == 1 ]] || { echo "sherpa-onnx DLLs not found; cargo clean and build again"; exit 1; }

# The Visual C++ runtime, for machines without the redistributable
# (app-local deployment of these DLLs is allowed).
sys="${SYSTEMROOT:-C:/Windows}/System32"
for dll in vcruntime140.dll vcruntime140_1.dll msvcp140.dll; do
  [[ -f "$sys/$dll" ]] && cp "$sys/$dll" "$OUT/"
done

cp LICENSE-MIT LICENSE-APACHE NOTICE "$OUT/" 2>/dev/null || true

rm -f "$ZIP"
(cd "$ROOT/target" && 7z a -tzip -mx=7 "$ROOT/$ZIP" Mostalgia >/dev/null)
ls -l dist/
