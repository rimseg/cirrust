//! Cirrust — Tauri backend entry point.

mod auth;
mod config;
mod dashboard;
mod error;
mod files;
mod media;
mod mediahttp;
mod pim;
mod sharing;
mod state;
mod stream;
mod sync;
mod trash;
mod tray_badge;
mod webdav;

use state::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;

/// Whether the system tray was created. When false (e.g. libayatana-appindicator
/// missing) the window must close/quit normally instead of hiding to the tray.
static TRAY_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Slim native KDE (Breeze) title bar instead of GTK's thick client-side
    // decoration. On Wayland GTK forces CSD and KWin can't override it, so run
    // the webview on XWayland where KWin draws the server-side title bar;
    // GTK_CSD=0 disables GTK's own. (Music playback stalling was a separate
    // bug, not caused by this.) Both must be set before GTK inits.
    if std::env::var_os("GTK_CSD").is_none() {
        std::env::set_var("GTK_CSD", "0");
    }
    if std::env::var_os("GDK_BACKEND").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_some()
    {
        std::env::set_var("GDK_BACKEND", "x11");
    }
    // WebKitGTK's DMABUF video renderer produces a black frame for `<video>` on
    // many Intel/Mesa setups (accelerated compositing path). Disabling it falls
    // back to a renderer that actually paints the decoded frames. Must be set
    // before the webview (WebKit) initializes.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        // Streams Nextcloud files into the webview with Range support (media
        // playback + previews) — see `stream.rs`.
        .register_asynchronous_uri_scheme_protocol("stream", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn(async move {
                responder.respond(stream::serve(app, request).await);
            });
        })
        .plugin(
            tauri_plugin_log::Builder::new()
                // Keep our own logs at Info; silence chatty dependency TRACE/DEBUG.
                .level(log::LevelFilter::Info)
                .level_for("app_lib", log::LevelFilter::Debug)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            auth::auth_start_login,
            auth::auth_poll_login,
            auth::auth_add_manual,
            auth::auth_list_accounts,
            auth::auth_active_account,
            auth::auth_set_active_account,
            auth::auth_remove_account,
            files::files_list,
            files::files_search,
            files::files_delete,
            files::files_download,
            files::files_upload,
            files::files_mkdir,
            files::files_move,
            files::files_copy,
            files::files_read_text,
            media::media_local_path,
            media::media_reveal_path,
            media::media_cache,
            media::media_bytes,
            media::media_http_url,
            trash::trash_list,
            trash::trash_restore,
            trash::trash_delete,
            trash::trash_empty,
            sync::sync_list_folders,
            sync::sync_folder_stats,
            sync::sync_add_folder,
            sync::sync_remove_folder,
            sync::sync_status,
            sync::sync_progress,
            sync::sync_activity,
            sync::sync_now,
            sync::sync_set_paused,
            sync::sync_set_folder_enabled,
            sync::sync_settings,
            sync::sync_set_ignore_patterns,
            sync::sync_conflicts,
            sync::sync_resolve_conflict,
            sync::sync_dismiss_identical_conflicts,
            pim::caldav::caldav_calendars,
            pim::caldav::caldav_refresh,
            pim::caldav::caldav_events,
            pim::caldav::caldav_save_event,
            pim::caldav::caldav_delete_event,
            pim::carddav::carddav_addressbooks,
            pim::carddav::carddav_refresh,
            pim::carddav::carddav_contacts,
            pim::carddav::carddav_save_contact,
            pim::carddav::carddav_delete_contact,
            dashboard::account_info,
            dashboard::account_activity,
            sharing::shares_list,
            sharing::share_create,
            sharing::share_delete,
        ])
        .setup(|app| {
            // Single-instance guard: claim the app's D-Bus name with strict
            // flags on the long-lived runtime. If another instance owns it,
            // raise that instance's window and exit — the main window is
            // created hidden, so a blocked duplicate never flashes.
            match tauri::async_runtime::block_on(sync::dbus::acquire_name()) {
                sync::dbus::BusAcquire::AlreadyRunning => {
                    eprintln!(
                        "cirrust is already running — raising the existing window"
                    );
                    if !std::env::args().any(|a| a == "--hidden") {
                        tauri::async_runtime::block_on(sync::dbus::raise_running_instance());
                    }
                    std::process::exit(0);
                }
                sync::dbus::BusAcquire::NoBus => {
                    log::warn!("no session D-Bus — widget service and instance guard disabled");
                }
                sync::dbus::BusAcquire::Primary => {}
            }

            // Loopback HTTP server for `<video>`/`<audio>` playback (WebKitGTK's
            // custom scheme can't seek media — see `mediahttp.rs`).
            match tauri::async_runtime::block_on(mediahttp::MediaServer::start(app.handle().clone())) {
                Ok(server) => {
                    app.manage(server);
                }
                Err(e) => log::warn!("media http server failed to start: {e}"),
            }

            setup_tray(app.handle());

            // The window is configured hidden; show it now unless this launch
            // is a hidden autostart (tray-only).
            if !std::env::args().any(|a| a == "--hidden") {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                }
            }

            // Start the sync engine up front so `sync_*` commands always have a
            // managed manager (no race with the UI).
            app.manage(sync::SyncManager::start(app.handle().clone()));

            // Restore a previous session in the background, then kick a sync so
            // the first authenticated run happens as soon as credentials load.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                match auth::restore_sessions(&handle, &state).await {
                    Ok(0) => log::info!("no stored accounts"),
                    Ok(n) => {
                        log::info!("restored {n} account(s)");
                        handle.state::<sync::SyncManager>().kick();
                    }
                    Err(e) => log::warn!("failed to restore accounts: {e}"),
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // Keep running in the tray on window close so background sync
            // continues; the tray "Quit" item performs a real exit. If there is
            // no tray, close normally so the app remains usable/quittable.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" && TRAY_AVAILABLE.load(Ordering::Relaxed) {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Try to create the system tray. Building it loads libayatana-appindicator on
/// Linux; if that library is missing the C binding *panics*, so we isolate the
/// attempt with `catch_unwind` and simply run without a tray on failure.
fn setup_tray(app: &tauri::AppHandle) {
    let handle = app.clone();
    let outcome =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || build_tray(&handle)));
    match outcome {
        Ok(Ok(())) => TRAY_AVAILABLE.store(true, Ordering::Relaxed),
        Ok(Err(e)) => log::warn!("tray setup failed: {e}"),
        Err(_) => log::warn!(
            "system tray unavailable — install 'libayatana-appindicator'. \
             Running without a tray; the window will close/quit normally."
        ),
    }
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let open_item = MenuItem::with_id(app, "open", "Open Cirrust", true, None::<&str>)?;
    let sync_item = MenuItem::with_id(app, "sync", "Sync now", true, None::<&str>)?;
    let autostart_item = CheckMenuItem::with_id(
        app,
        "autostart",
        "Start on login",
        true,
        app.autolaunch().is_enabled().unwrap_or(false),
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open_item, &sync_item, &autostart_item, &quit_item])?;
    let autostart_check = autostart_item.clone();

    TrayIconBuilder::with_id("main-tray")
        .tooltip("Cirrust")
        .icon(
            tray_badge::base_icon()
                .unwrap_or_else(|| app.default_window_icon().unwrap().clone()),
        )
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            "sync" => {
                if let Some(manager) = app.try_state::<sync::SyncManager>() {
                    manager.kick();
                }
            }
            "autostart" => {
                let launcher = app.autolaunch();
                let enabled = launcher.is_enabled().unwrap_or(false);
                let result = if enabled { launcher.disable() } else { launcher.enable() };
                if let Err(e) = result {
                    log::warn!("autostart toggle failed: {e}");
                }
                let _ = autostart_check.set_checked(launcher.is_enabled().unwrap_or(false));
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

pub(crate) fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
