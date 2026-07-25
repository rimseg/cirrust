#!/usr/bin/env bash
# Live sync-engine tests against a throwaway Nextcloud.
#
# Spins up nextcloud:stable-apache in Docker, runs the #[ignore] live tests
# (everything named `live_*` — the sync engine, WebDAV, OCS, shares, trash) and
# tears the container down. Intended as a PRE-RELEASE GATE: run it green before
# tagging a new version, since these paths cannot be exercised without a real
# server and CI skips them.
#
#   packaging/live-tests.sh          # up, test, down
#   packaging/live-tests.sh --keep   # leave the container running afterwards
#   PORT=8091 packaging/live-tests.sh   # use a different host port
set -euo pipefail

keep=false
[ "${1:-}" = "--keep" ] && keep=true

repo="$(cd "$(dirname "$0")/.." && pwd)"
name="cirrust-livetest-nc"
port="${PORT:-8080}"
url="http://127.0.0.1:${port}"
admin=admin
adminpw="livetest-pw"

cleanup() { $keep || docker rm -f "$name" >/dev/null 2>&1 || true; }
trap cleanup EXIT

command -v docker >/dev/null || { echo "docker not found or not on PATH"; exit 1; }
docker info >/dev/null 2>&1 || { echo "docker daemon not reachable (start it, or add yourself to the docker group)"; exit 1; }

echo "==> Starting Nextcloud ($name) on $url"
docker rm -f "$name" >/dev/null 2>&1 || true
docker run -d --name "$name" \
  -p "127.0.0.1:${port}:80" \
  -e SQLITE_DATABASE=nc \
  -e NEXTCLOUD_ADMIN_USER="$admin" \
  -e NEXTCLOUD_ADMIN_PASSWORD="$adminpw" \
  -e NEXTCLOUD_TRUSTED_DOMAINS=127.0.0.1 \
  nextcloud:stable-apache >/dev/null

echo -n "==> Waiting for install"
ready=false
for _ in $(seq 1 90); do
  if curl -s -m 5 "$url/status.php" 2>/dev/null | grep -q '"installed":true'; then
    ready=true; echo " ready"; break
  fi
  echo -n "."; sleep 3
done
$ready || { echo " TIMEOUT — Nextcloud did not come up"; exit 1; }

echo "==> Minting an app password"
pw="$(docker exec -e OC_PASS="$adminpw" -u www-data "$name" \
  php occ user:add-app-password "$admin" --password-from-env | tail -1)"

echo "==> Running live tests (cargo test live_ -- --ignored)"
cd "$repo/app/src-tauri"
NC_URL="$url" NC_USER="$admin" NC_PASS="$pw" \
  cargo test --lib live_ -- --ignored --nocapture

echo "==> Live tests PASSED"
