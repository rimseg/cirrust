//! Status-aware tray icon: composites a colored badge onto the app's cloud
//! icon so the tray reflects the sync state at a glance — green (synced),
//! blue-white (syncing), orange (paused), red (error), gray (offline).
//!
//! The icon is only repainted when the state actually changes: Plasma's
//! StatusNotifier tray re-creates the pixmap on every `set_icon`, so frequent
//! updates (e.g. an animation) make the icon flicker or vanish.

use crate::sync::{SyncState, SyncStatus};
use std::sync::OnceLock;
use tauri::image::Image;
use tauri::AppHandle;
use tokio::sync::watch;

/// Monochrome white-cloud glyph (transparent background) used in the tray —
/// panel-style, unlike the full-color app icon. Decoded once.
static TRAY_GLYPH: OnceLock<Option<(Vec<u8>, u32, u32)>> = OnceLock::new();

/// The base tray icon (before the status badge is composited).
pub fn base_icon() -> Option<Image<'static>> {
    let decoded = TRAY_GLYPH.get_or_init(|| {
        Image::from_bytes(include_bytes!("../icons/tray-cloud.png"))
            .ok()
            .map(|img| (img.rgba().to_vec(), img.width(), img.height()))
    });
    decoded
        .as_ref()
        .map(|(rgba, w, h)| Image::new_owned(rgba.clone(), *w, *h))
}

fn badge_color(state: SyncState) -> [u8; 4] {
    match state {
        SyncState::Idle => [39, 174, 96, 255],     // green — everything synced
        SyncState::Syncing => [61, 174, 233, 255], // Breeze blue — activity
        SyncState::Paused => [246, 116, 0, 255],    // orange
        SyncState::Error => [218, 68, 83, 255],     // red
        SyncState::Offline => [127, 140, 141, 255], // gray
    }
}

fn tooltip(status: &SyncStatus) -> String {
    match status.state {
        SyncState::Idle => "Cirrust — synced".into(),
        SyncState::Syncing => match &status.active_folder {
            Some(f) => format!("Cirrust — syncing {f}"),
            None => "Cirrust — syncing…".into(),
        },
        SyncState::Paused => "Cirrust — sync paused".into(),
        SyncState::Error => "Cirrust — sync error".into(),
        SyncState::Offline => match &status.message {
            Some(m) => format!("Cirrust — offline: {m}"),
            None => "Cirrust — offline / not signed in".into(),
        },
    }
}

/// Draw a filled circle badge with a dark outline into the bottom-right
/// corner of the base icon.
fn badged(base: &Image<'_>, color: [u8; 4]) -> Image<'static> {
    let w = base.width();
    let h = base.height();
    let mut rgba = base.rgba().to_vec();

    let radius = (w as f32 * 0.20).max(4.0);
    let cx = w as f32 - radius - 2.0;
    let cy = h as f32 - radius - 2.0;

    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist <= radius {
                let i = ((y * w + x) * 4) as usize;
                // Dark outline ring so every badge color reads on the blue
                // cloud square and on any panel background.
                let px = if dist >= radius - 1.5 {
                    [30, 47, 60, 255]
                } else {
                    color
                };
                rgba[i..i + 4].copy_from_slice(&px);
            }
        }
    }
    Image::new_owned(rgba, w, h)
}

fn update(app: &AppHandle, status: &SyncStatus) {
    let Some(tray) = app.tray_by_id("main-tray") else {
        return; // tray unavailable (e.g. libayatana missing) — nothing to do
    };
    let Some(base) = base_icon() else {
        return;
    };
    let icon = badged(&base, badge_color(status.state));
    let _ = tray.set_icon(Some(icon));
    let _ = tray.set_tooltip(Some(tooltip(status)));
}

/// Watch sync status and repaint the tray icon + tooltip — but only when the
/// displayed state (or active folder) actually changes.
pub fn spawn_status_badge(app: AppHandle, status_rx: watch::Receiver<SyncStatus>) {
    tauri::async_runtime::spawn(async move {
        let mut rx = status_rx;

        // Paint the initial state once at startup.
        let mut last = {
            let s = rx.borrow().clone();
            update(&app, &s);
            (s.state, s.active_folder)
        };

        while rx.changed().await.is_ok() {
            let s = rx.borrow().clone();
            let key = (s.state, s.active_folder.clone());
            if key != last {
                update(&app, &s);
                last = key;
            }
        }
    });
}
