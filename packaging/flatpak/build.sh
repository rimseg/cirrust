#!/usr/bin/env bash
# Build and install the Flatpak from the deb produced by `tauri build`.
#
#   cd app && npm run tauri build        # (or: docker compose run --rm build)
#   packaging/flatpak/build.sh           # → installs org.cirrust.client (user)
#
# Requires: flatpak, flatpak-builder, and the Flathub remote.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

# Locate the newest deb from either a local or a Docker build.
deb=$(ls -t \
  "$repo"/app/src-tauri/target/release/bundle/deb/*.deb \
  "$repo"/dist/bundle/deb/*.deb 2>/dev/null | head -1 || true)
if [ -z "$deb" ]; then
  echo "No .deb found — run 'npm run tauri build' (or the Docker build) first." >&2
  exit 1
fi
echo "Using: $deb"
cp "$deb" "$here/cirrust.deb"

flatpak remote-add --user --if-not-exists flathub \
  https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak-builder --user --install --force-clean \
  "$here/.build" "$here/org.cirrust.client.yml"

rm -f "$here/cirrust.deb"
echo
echo "Installed. Run with:  flatpak run org.cirrust.client"
