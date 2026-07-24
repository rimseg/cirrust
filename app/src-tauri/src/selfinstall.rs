//! First-run self-install for the AppImage build (Linux only).
//!
//! When Cirrust runs from an AppImage, the `APPIMAGE` env var points at the
//! `.AppImage` file. On the first such run we can offer to copy the AppImage to
//! `~/.local/bin/cirrust` and register a desktop entry + icons, so it appears in
//! the applications menu and the tray/autostart entries resolve to a stable
//! path. The offer is shown once; declining writes a marker that suppresses it
//! on later launches. `--install` / `--uninstall` do the same non-interactively.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const APP_ID: &str = "org.cirrust.client";
const BIN_NAME: &str = "cirrust";

/// This build's version — compared against the installed copy's recorded
/// version to decide whether to offer an update.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Icons embedded at compile time, so installing never depends on the AppImage's
/// mount layout being present. Sizes mirror `packaging/install-dev-desktop.sh`.
const ICONS: &[(u32, &[u8])] = &[
    (32, include_bytes!("../icons/32x32.png")),
    (64, include_bytes!("../icons/64x64.png")),
    (128, include_bytes!("../icons/128x128.png")),
    (256, include_bytes!("../icons/256x256.png")),
    (512, include_bytes!("../icons/512x512.png")),
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".local/share")))
}

fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(".config")))
}

/// The running AppImage's own path, if launched from one (else `None`).
pub fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

fn target_bin() -> Option<PathBuf> {
    home().map(|h| h.join(".local/bin").join(BIN_NAME))
}

fn desktop_file() -> Option<PathBuf> {
    data_home().map(|d| d.join("applications").join(format!("{APP_ID}.desktop")))
}

fn declined_marker() -> Option<PathBuf> {
    data_home().map(|d| d.join(APP_ID).join(".install-declined"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Records the version of the currently installed copy.
fn version_marker() -> Option<PathBuf> {
    data_home().map(|d| d.join(APP_ID).join(".installed-version"))
}

/// Records that the user declined *updating to* a specific version.
fn update_declined_marker() -> Option<PathBuf> {
    data_home().map(|d| d.join(APP_ID).join(".update-declined"))
}

/// Version recorded for the installed copy. `None` if never recorded (e.g. an
/// install predating this marker) — treated as older than any real version.
fn installed_version() -> Option<String> {
    version_marker()
        .and_then(|m| fs::read_to_string(m).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Parse `MAJOR.MINOR.PATCH` into a comparable tuple. Missing minor/patch pad to
/// zero, but a *present* component that is not a number yields `None` — a
/// malformed version should not be silently treated as a valid one.
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.split('.');
    let major = parts.next()?.trim().parse::<u32>().ok()?;
    let component = |p: Option<&str>| -> Option<u32> {
        match p {
            Some(s) => s.trim().parse::<u32>().ok(),
            None => Some(0),
        }
    };
    let minor = component(parts.next())?;
    let patch = component(parts.next())?;
    Some((major, minor, patch))
}

/// True when `running` is a strictly newer version than `installed`.
fn is_newer(running: &str, installed: &str) -> bool {
    match (parse_version(running), parse_version(installed)) {
        (Some(r), Some(i)) => r > i,
        _ => false,
    }
}

/// Whether to offer self-install: running from an AppImage that is not already
/// the installed copy, with nothing installed yet and no prior decline.
pub fn should_offer_install() -> bool {
    let Some(appimage) = appimage_path() else {
        return false;
    };
    let Some(target) = target_bin() else {
        return false;
    };
    if same_file(&appimage, &target) || target.exists() {
        return false; // already the installed copy, or something is installed
    }
    !declined_marker().map_or(false, |m| m.exists())
}

/// Whether to offer to *update* the installed copy: running from an AppImage
/// that is newer than the installed one (and is not that installed copy), with
/// no prior decline for this exact version. An install with no recorded version
/// (predating the marker) counts as older, so it is offered the update once.
pub fn should_offer_update() -> bool {
    let Some(appimage) = appimage_path() else {
        return false;
    };
    let Some(target) = target_bin() else {
        return false;
    };
    if same_file(&appimage, &target) || !target.exists() {
        return false; // running the installed copy, or nothing installed
    }
    if !is_newer(VERSION, &installed_version().unwrap_or_else(|| "0.0.0".into())) {
        return false;
    }
    // Not already declined for this exact version.
    let declined = update_declined_marker()
        .and_then(|m| fs::read_to_string(m).ok())
        .map(|s| s.trim().to_string());
    declined.as_deref() != Some(VERSION)
}

/// Remember that the user declined updating to this version.
pub fn mark_update_declined() {
    if let Some(m) = update_declined_marker() {
        if let Some(dir) = m.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&m, VERSION.as_bytes());
    }
}

/// Copy the AppImage to `~/.local/bin`, write the desktop entry and icons.
/// Returns the installed binary path.
pub fn install() -> std::io::Result<PathBuf> {
    let err = |m: &str| std::io::Error::new(std::io::ErrorKind::Other, m.to_string());
    let appimage = appimage_path().ok_or_else(|| err("not running from an AppImage"))?;
    let target = target_bin().ok_or_else(|| err("no HOME"))?;

    // 1. The binary itself.
    if let Some(dir) = target.parent() {
        fs::create_dir_all(dir)?;
    }
    if !same_file(&appimage, &target) {
        fs::copy(&appimage, &target)?;
        let mut perm = fs::metadata(&target)?.permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&target, perm)?;
    }

    // 2. Desktop entry — Exec points at the installed copy, not the AppImage the
    //    user may have downloaded to a temp/Downloads folder.
    if let Some(desktop) = desktop_file() {
        if let Some(dir) = desktop.parent() {
            fs::create_dir_all(dir)?;
        }
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Cirrust\n\
             GenericName=Cloud Sync Client\n\
             Comment=Browse, sync and play files from Nextcloud and compatible servers\n\
             Exec={} %U\n\
             Icon={APP_ID}\n\
             Terminal=false\n\
             Categories=Network;FileTransfer;Utility;\n\
             StartupWMClass={APP_ID}\n\
             StartupNotify=true\n",
            target.display()
        );
        fs::write(&desktop, contents)?;
    }

    // 3. Icons, under the app id so the desktop entry and window group resolve.
    if let Some(data) = data_home() {
        for (size, bytes) in ICONS {
            let p = data
                .join("icons/hicolor")
                .join(format!("{size}x{size}"))
                .join("apps")
                .join(format!("{APP_ID}.png"));
            if let Some(dir) = p.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(&p, bytes)?;
        }
    }

    // 4. Record the installed version so a later, newer AppImage can offer an
    //    update; clear any stale "declined update" marker.
    if let Some(m) = version_marker() {
        if let Some(dir) = m.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&m, VERSION.as_bytes())?;
    }
    if let Some(m) = update_declined_marker() {
        let _ = fs::remove_file(m);
    }

    refresh_desktop_db();
    Ok(target)
}

/// Remove the installed binary, desktop entry, icons and autostart entry.
pub fn uninstall() -> std::io::Result<()> {
    if let Some(t) = target_bin() {
        let _ = fs::remove_file(&t);
    }
    if let Some(d) = desktop_file() {
        let _ = fs::remove_file(&d);
    }
    if let Some(data) = data_home() {
        for (size, _) in ICONS {
            let p = data
                .join("icons/hicolor")
                .join(format!("{size}x{size}"))
                .join("apps")
                .join(format!("{APP_ID}.png"));
            let _ = fs::remove_file(&p);
        }
    }
    // Autostart entry written by tauri-plugin-autostart (auto-launch names it
    // after the product: `Cirrust.desktop`).
    if let Some(cfg) = config_home() {
        let _ = fs::remove_file(cfg.join("autostart").join("Cirrust.desktop"));
    }
    for m in [declined_marker(), version_marker(), update_declined_marker()]
        .into_iter()
        .flatten()
    {
        let _ = fs::remove_file(&m);
    }
    refresh_desktop_db();
    Ok(())
}

/// Remember that the user declined, so the offer is not shown again.
pub fn mark_declined() {
    if let Some(m) = declined_marker() {
        if let Some(dir) = m.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(&m, b"");
    }
}

fn refresh_desktop_db() {
    if let Some(data) = data_home() {
        let _ = Command::new("update-desktop-database")
            .arg(data.join("applications"))
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::{is_newer, parse_version};

    #[test]
    fn parses_and_pads() {
        assert_eq!(parse_version("0.1.5"), Some((0, 1, 5)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2"), Some((2, 0, 0)));
        assert_eq!(parse_version("nope"), None);
        assert_eq!(parse_version("0.1.x"), None);
    }

    #[test]
    fn newer_compares_numerically_not_lexically() {
        assert!(is_newer("0.1.5", "0.1.4"));
        assert!(is_newer("0.2.0", "0.1.9"));
        // The lexical trap: "0.1.10" < "0.1.9" as strings, but 10 > 9.
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.5", "0.1.5")); // equal is not newer
        assert!(!is_newer("0.1.4", "0.1.5")); // older
        assert!(is_newer("0.1.5", "0.0.0")); // unrecorded install migrates
        assert!(!is_newer("bad", "0.1.5")); // unparseable -> do not offer
    }
}
