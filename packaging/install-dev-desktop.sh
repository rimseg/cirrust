#!/usr/bin/env bash
# Install a desktop entry + icon so KDE/Plasma identifies the *dev* app
# (`npm run tauri dev`) as "Cirrust" instead of falling back to a wrong
# entry (e.g. KDE Connect). A real `tauri build` install does this for you.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(dirname "$here")"

app_id="org.cirrust.client"

install -Dm644 "$here/$app_id.desktop" \
  "$HOME/.local/share/applications/$app_id.desktop"

# Install the full resolution set so KDE picks a crisp size for every context
# (panel, task manager, Kickoff, Alt+Tab). The scalable SVG is preferred by
# most Plasma surfaces when present.
for s in 32 64 128 256 512; do
  src="$repo/app/src-tauri/icons/${s}x${s}.png"
  [ -f "$src" ] && install -Dm644 "$src" \
    "$HOME/.local/share/icons/hicolor/${s}x${s}/apps/$app_id.png"
done
install -Dm644 "$repo/packaging/app-icon.svg" \
  "$HOME/.local/share/icons/hicolor/scalable/apps/$app_id.svg"

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
kbuildsycoca6 >/dev/null 2>&1 || true

echo "Installed $app_id.desktop + icon."
echo "Restart the app (npm run tauri dev) — Plasma should now show 'Cirrust'."
echo "If it still shows wrong, your Wayland app_id differs; check it while the app runs:"
echo "  qdbus6 org.kde.KWin /KWin org.kde.KWin.showDebugConsole   # → Windows tab → resourceName"
