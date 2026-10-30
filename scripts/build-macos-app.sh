#!/usr/bin/env bash
# Builds dist/Mostalgia.app and dist/Mostalgia-macOS-<arch>.dmg: the app and
# the `mostalgia` CLI (Metal build) with the sherpa-onnx libraries in
# Contents/Frameworks. Models are not included; the app downloads them on
# first launch.
#
#   ./scripts/build-macos-app.sh
#
# Signing: ad hoc (`codesign -s -`) unless MACOS_SIGN_IDENTITY names a
# "Developer ID Application" identity in the keychain; then it signs with the
# hardened runtime and packaging/macos/Mostalgia.entitlements, ready for
# notarization (see .github/workflows/release.yml).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT=$PWD
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP="$ROOT/dist/Mostalgia.app"
DMG="$ROOT/dist/Mostalgia-macOS-$ARCH.dmg"
IDENTITY=${MACOS_SIGN_IDENTITY:--}

# The oldest macOS the app runs on (LSMinimumSystemVersion in Info.plist),
# for Rust and the C/C++ of whisper.cpp alike.
export MACOSX_DEPLOYMENT_TARGET=13.0
cargo build --release --features metal -p mostalgia-app -p mostalgia

rm -rf "$APP" "$DMG"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources/de.lproj"
cp target/release/mostalgia-app target/release/mostalgia "$APP/Contents/MacOS/"
sed "s/@VERSION@/$VERSION/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
cp packaging/macos/de.lproj/InfoPlist.strings "$APP/Contents/Resources/de.lproj/"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# The icon: every size of an .iconset from the 512 px PNG.
iconset="$ROOT/target/Mostalgia.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
src=assets/brand/icons/hicolor/512x512/apps/io.github.mostaxd.Mostalgia.png
for s in 16 32 128 256 512; do
  sips -z $s $s "$src" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  [[ $d -le 512 ]] && sips -z $d $d "$src" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$APP/Contents/Resources/Mostalgia.icns"

# sherpa-onnx's libraries: sherpa-rs copies them next to the binaries, but
# only when its build script runs; a build restored from a cache has them
# only in sherpa-rs's download cache.
libdir=$(dirname "$(find "$ROOT/target/release" "$HOME/Library/Caches/sherpa-rs" \
  -name 'libsherpa-onnx-c-api.dylib' -print -quit 2>/dev/null || true)")
[[ -f "$libdir/libsherpa-onnx-c-api.dylib" ]] || { echo "libsherpa-onnx-c-api.dylib not found; cargo clean and build again"; exit 1; }
for lib in "$libdir"/libsherpa-onnx-c-api.dylib "$libdir"/libonnxruntime*.dylib; do
  [[ -e "$lib" ]] && cp -L "$lib" "$APP/Contents/Frameworks/"
done
frameworks=$(ls "$APP/Contents/Frameworks")

# Every reference to one of those libraries goes through @rpath; the
# binaries look in ../Frameworks (see the crates' build.rs), the libraries
# next to themselves.
relink() {
  local file=$1
  otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r dep; do
    base=$(basename "$dep")
    if grep -qx "$base" <<<"$frameworks" && [[ "$dep" != "@rpath/$base" ]]; then
      install_name_tool -change "$dep" "@rpath/$base" "$file"
    fi
  done
}
add_rpath() {
  otool -l "$1" | grep -A2 LC_RPATH | grep -q "path $2 " || install_name_tool -add_rpath "$2" "$1"
}
for lib in "$APP"/Contents/Frameworks/*.dylib; do
  install_name_tool -id "@rpath/$(basename "$lib")" "$lib"
  relink "$lib"
  add_rpath "$lib" @loader_path
done
for bin in "$APP"/Contents/MacOS/*; do
  relink "$bin"
  add_rpath "$bin" @executable_path/../Frameworks
done
echo "Linked libraries:"
otool -L "$APP/Contents/MacOS/mostalgia-app" | grep -E 'rpath|sherpa|onnx' || true

# Sign inside out: libraries, the CLI, then the bundle (its main binary).
sign=(codesign --force --timestamp=none --sign "$IDENTITY")
if [[ "$IDENTITY" != "-" ]]; then
  sign=(codesign --force --timestamp --options runtime --sign "$IDENTITY")
fi
for lib in "$APP"/Contents/Frameworks/*.dylib; do "${sign[@]}" "$lib"; done
"${sign[@]}" --entitlements packaging/macos/Mostalgia.entitlements "$APP/Contents/MacOS/mostalgia"
"${sign[@]}" --entitlements packaging/macos/Mostalgia.entitlements "$APP"
codesign --verify --deep --strict --verbose=2 "$APP"

# The disk image: the app and a link to /Applications to drag it to.
stage="$ROOT/target/dmg"
rm -rf "$stage" && mkdir -p "$stage"
cp -R "$APP" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Mostalgia -srcfolder "$stage" -ov -format UDZO "$DMG"
if [[ "$IDENTITY" != "-" ]]; then
  codesign --force --sign "$IDENTITY" "$DMG"
fi
ls -lh dist/
