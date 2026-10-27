#!/usr/bin/env bash
# Adds a GNOME keyboard shortcut that runs `mostalgia toggle` (start/stop
# recording; launches the app if needed). Wayland doesn't let apps grab
# global hotkeys, so GNOME runs the command for us.
#
#   ./scripts/install-gnome-shortcut.sh                # Super+Shift+R
#   ./scripts/install-gnome-shortcut.sh '<Ctrl><Alt>m'
set -euo pipefail
BINDING=${1:-'<Super><Shift>r'}
CMD="$(command -v mostalgia || realpath "$(dirname "$0")/../target/release/mostalgia") toggle"
SCHEMA=org.gnome.settings-daemon.plugins.media-keys
PATH_KEY=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/mostalgia/

existing=$(gsettings get $SCHEMA custom-keybindings)
if [[ $existing != *"$PATH_KEY"* ]]; then
  if [[ $existing == "@as []" || $existing == "[]" ]]; then
    new="['$PATH_KEY']"
  else
    new="${existing%]}, '$PATH_KEY']"
  fi
  gsettings set $SCHEMA custom-keybindings "$new"
fi
gsettings set $SCHEMA.custom-keybinding:$PATH_KEY name 'mostalgia: Aufnahme starten/stoppen'
gsettings set $SCHEMA.custom-keybinding:$PATH_KEY command "$CMD"
gsettings set $SCHEMA.custom-keybinding:$PATH_KEY binding "$BINDING"
echo "Shortcut $BINDING → $CMD"
