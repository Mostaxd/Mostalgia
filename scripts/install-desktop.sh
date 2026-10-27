#!/usr/bin/env bash
# Installs Mostalgia for the current user: links the binaries into
# ~/.local/bin, the icons into the hicolor theme, and the desktop entry, so
# Mostalgia shows up in the GNOME app grid.
set -euo pipefail
cd "$(dirname "$0")/.."
BIN="$PWD/target/release"
[[ -x "$BIN/mostalgia-app" ]] || { echo "Build first: cargo build --release --features vulkan"; exit 1; }

mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons
ln -sf "$BIN/mostalgia" ~/.local/bin/mostalgia
ln -sf "$BIN/mostalgia-app" ~/.local/bin/mostalgia-app
cp -r assets/brand/icons/hicolor ~/.local/share/icons/
sed "s|^Exec=mostalgia-app|Exec=$HOME/.local/bin/mostalgia-app|" packaging/io.github.mostaxd.Mostalgia.desktop \
  > ~/.local/share/applications/io.github.mostaxd.Mostalgia.desktop
gtk-update-icon-cache -q -t ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database -q ~/.local/share/applications 2>/dev/null || true
echo "Installed: mostalgia, mostalgia-app (~/.local/bin), icons and desktop entry."
