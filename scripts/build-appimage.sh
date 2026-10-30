#!/usr/bin/env bash
# Builds dist/Mostalgia-x86_64.AppImage: the app and the `mostalgia` CLI
# (Vulkan build), the sherpa-onnx libraries and the other shared libraries
# a desktop doesn't reliably have, bundled with linuxdeploy. The Vulkan
# loader stays the system's, so it finds the system's GPU drivers. Models
# are not included; the app downloads them on first launch.
#
#   ./scripts/build-appimage.sh            # needs the packages from the README
#
# Build on the oldest distribution you want to support (CI uses Ubuntu
# 22.04): the AppImage runs on systems with the same or a newer glibc.
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
ARCH=x86_64
TOOLS="$ROOT/target/appimage-tools"
APPDIR="$ROOT/target/AppDir"
mkdir -p "$TOOLS" dist

cargo build --release --features vulkan -p mostalgia-app -p mostalgia

fetch() {
  [[ -x "$TOOLS/$1" ]] && return
  curl -fsSL -o "$TOOLS/$1" "$2"
  chmod +x "$TOOLS/$1"
}
fetch linuxdeploy "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-$ARCH.AppImage"
fetch linuxdeploy-plugin-appimage "https://github.com/linuxdeploy/linuxdeploy-plugin-appimage/releases/download/continuous/linuxdeploy-plugin-appimage-$ARCH.AppImage"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/icons"
cp target/release/mostalgia-app target/release/mostalgia "$APPDIR/usr/bin/"
cp -r assets/brand/icons/hicolor "$APPDIR/usr/share/icons/"

# Runs without FUSE (containers, CI); the plugin is found next to linuxdeploy.
export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$TOOLS:$PATH"
# The sherpa-onnx libraries: sherpa-rs copies them next to the binaries,
# but only when its build script runs; a build restored from a cache has
# them only in sherpa-rs's download cache.
sherpa=$(find "$ROOT/target/release" "${XDG_CACHE_HOME:-$HOME/.cache}/sherpa-rs" \
  -name libsherpa-onnx-c-api.so -print -quit 2>/dev/null || true)
[[ -n "$sherpa" ]] || { echo "libsherpa-onnx-c-api.so not found; cargo clean and build again"; exit 1; }
export LD_LIBRARY_PATH="$(dirname "$sherpa")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
# Lets AppImageUpdate / Gear Lever fetch new releases.
export LDAI_UPDATE_INFORMATION="gh-releases-zsync|Mostaxd|Mostalgia|latest|Mostalgia-$ARCH.AppImage.zsync"
export LDAI_OUTPUT="dist/Mostalgia-$ARCH.AppImage"
export LDAI_NO_APPSTREAM=1

linuxdeploy --appdir "$APPDIR" \
  --executable "$APPDIR/usr/bin/mostalgia-app" \
  --executable "$APPDIR/usr/bin/mostalgia" \
  --desktop-file packaging/io.github.mostaxd.Mostalgia.desktop \
  --icon-file assets/brand/icons/hicolor/256x256/apps/io.github.mostaxd.Mostalgia.png \
  --custom-apprun packaging/appimage/AppRun \
  --exclude-library 'libvulkan.so*' \
  --output appimage

# The .zsync goes wherever the tool was run; keep it with the AppImage.
[[ -f "Mostalgia-$ARCH.AppImage.zsync" ]] && mv "Mostalgia-$ARCH.AppImage.zsync" dist/
ls -lh dist/
