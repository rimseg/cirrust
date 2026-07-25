#!/usr/bin/env bash
# Local AppImage build with the workarounds non-Debian hosts need.
#
# CI (Ubuntu) needs none of this — linuxdeploy's defaults match Debian's
# filesystem layout. On Arch/Manjaro two things break:
#
#  1. linuxdeploy's bundled `strip` predates `.relr.dyn` relocation sections
#     (used by Arch-built libraries) and fails the bundle step -> NO_STRIP=1.
#  2. linuxdeploy-plugin-gstreamer looks for the GStreamer helper binaries
#     (gst-plugin-scanner, gst-ptp-helper) in Debian's
#     /usr/lib/x86_64-linux-gnu/gstreamer1.0/gstreamer-1.0; Arch keeps them in
#     /usr/lib/gstreamer-1.0. Without them the AppImage warns
#     "External plugin loader failed" and media playback breaks.
#     -> stage the helpers and pass GSTREAMER_HELPERS_DIR.
#
# The build is verified afterwards: the helpers must exist inside the AppDir
# at the exact path the AppRun hook exports, or this script fails loudly —
# a silently helper-less AppImage has bitten us before (a /tmp staging dir
# was cleaned between builds).
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
appdir="$repo/app/src-tauri/target/release/bundle/appimage/Cirrust.AppDir"

helpers_src=""
for dir in "/usr/lib/$(uname -m)-linux-gnu/gstreamer1.0/gstreamer-1.0" /usr/lib/gstreamer-1.0; do
  if [ -x "$dir/gst-plugin-scanner" ]; then
    helpers_src="$dir"
    break
  fi
done
[ -n "$helpers_src" ] || { echo "gst-plugin-scanner not found on this host"; exit 1; }

# Stage only the helpers (Arch keeps them inside the plugins dir, and pointing
# GSTREAMER_HELPERS_DIR at the whole plugins dir would copy ~275 plugins twice).
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
cp "$helpers_src/gst-plugin-scanner" "$staging/"
[ -x "$helpers_src/gst-ptp-helper" ] && cp "$helpers_src/gst-ptp-helper" "$staging/"

echo "==> Building AppImage (helpers from $helpers_src)"
cd "$repo/app"
GSTREAMER_HELPERS_DIR="$staging" NO_STRIP=1 npm run tauri build -- --bundles appimage

echo "==> Verifying bundled GStreamer helpers"
hook_dir="$appdir/usr/lib/gstreamer1.0/gstreamer-1.0"
[ -x "$hook_dir/gst-plugin-scanner" ] || {
  echo "FAIL: gst-plugin-scanner missing from $hook_dir — media playback would break"
  exit 1
}

out="$(ls "$repo"/app/src-tauri/target/release/bundle/appimage/Cirrust_*.AppImage)"
echo "==> OK: $out"
