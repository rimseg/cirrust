#!/usr/bin/env bash
# Uninstall / clean up a *native* Cirrust install (the binary + desktop entry +
# icons put in place by `install-dev-desktop.sh` / a manual copy to ~/.local/bin).
#
#   packaging/uninstall.sh           # remove the app (keeps your settings & data)
#   packaging/uninstall.sh --purge   # also remove config, cache, data & the keyring password
#   packaging/uninstall.sh --purge --yes   # …without the confirmation prompt
#
# It does NOT touch AppImage/Flatpak installs — remove those the way you added
# them (delete the .AppImage, or `flatpak uninstall org.cirrust.client`).
set -euo pipefail

app_id="org.cirrust.client"
bin_name="cirrust"
purge=false
assume_yes=false
for arg in "$@"; do
  case "$arg" in
    --purge) purge=true ;;
    --yes|-y) assume_yes=true ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
config_home="${XDG_CONFIG_HOME:-$HOME/.config}"
cache_home="${XDG_CACHE_HOME:-$HOME/.cache}"

rm_v() { for p in "$@"; do [ -e "$p" ] && rm -rf "$p" && echo "  removed $p"; done; return 0; }

# 1. Stop a running instance (exact process name; never a broad pkill -f).
if pgrep -x "$bin_name" >/dev/null 2>&1; then
  echo "Stopping running $bin_name…"
  pkill -x "$bin_name" 2>/dev/null || true
  sleep 1
fi

# 2. Remove the binary (both the common install location and a stray on PATH).
echo "Removing binary…"
rm_v "$HOME/.local/bin/$bin_name"
if command -v "$bin_name" >/dev/null 2>&1; then
  echo "  note: '$bin_name' is still on your PATH at $(command -v "$bin_name") — remove it manually if unwanted."
fi

# 3. Remove the desktop entry, autostart entry and hicolor icons.
echo "Removing desktop entry, autostart & icons…"
rm_v "$data_home/applications/$app_id.desktop"
rm_v "$config_home/autostart/$app_id.desktop"
for s in 32 64 128 256 512; do
  rm_v "$data_home/icons/hicolor/${s}x${s}/apps/$app_id.png"
done
rm_v "$data_home/icons/hicolor/scalable/apps/$app_id.svg"

# 4. User data — only with --purge (config, cache, synced-journal/PIM data, and
#    the stored app password). Synced *file* folders on disk are never touched.
if $purge; then
  if ! $assume_yes; then
    echo
    echo "--purge will delete Cirrust settings, cache, sync journals, the CalDAV/"
    echo "CardDAV cache, and the stored app password. Your synced files on disk are"
    echo "kept. This cannot be undone."
    read -r -p "Proceed? [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]] || { echo "Aborted purge."; exit 0; }
  fi
  echo "Purging user data…"
  rm_v "$config_home/$app_id" "$data_home/$app_id" "$cache_home/$app_id"
  # Best-effort: clear the app password from the Secret Service (KWallet).
  if command -v secret-tool >/dev/null 2>&1; then
    secret-tool clear service "$app_id" >/dev/null 2>&1 \
      && echo "  cleared keyring entry (service=$app_id)" \
      || echo "  note: no keyring entry cleared (may use different attributes; remove via KWalletManager if needed)."
  else
    echo "  note: install 'secret-tool' (libsecret) to auto-clear the stored password, or use KWalletManager."
  fi
fi

# 5. Refresh desktop/icon caches so the launcher entry disappears immediately.
echo "Refreshing caches…"
update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
gtk-update-icon-cache -f -t "$data_home/icons/hicolor" >/dev/null 2>&1 || true
rm -f "$cache_home/icon-cache.kcache" 2>/dev/null || true
kbuildsycoca6 >/dev/null 2>&1 || kbuildsycoca5 >/dev/null 2>&1 || true

echo
echo "Done. Panel widgets, if installed, are separate:"
echo "  KDE:      kpackagetool6 --type Plasma/Applet --remove $app_id"
echo "  GNOME:    rm -rf \"$data_home/gnome-shell/extensions/cirrust@cirrust.app\""
echo "  Cinnamon: rm -rf \"$data_home/cinnamon/applets/cirrust@cirrust.app\""
$purge || echo "Settings & data kept — re-run with --purge to remove them too."
