//! Personal Information Management: CalDAV (calendars/events) and CardDAV
//! (address books/contacts) sync for Nextcloud, layered on the same
//! authenticated [`WebDavClient`](crate::webdav::WebDavClient) the file browser
//! uses. Data is cached per account on disk and edited two-way, preserving any
//! iCal/vCard properties the client doesn't model.

pub mod caldav;
pub mod carddav;
pub mod contentline;
pub mod dav;
pub mod ical;
pub mod store;
pub mod vcard;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// The active account's id, or `NotAuthenticated` when nothing is connected.
pub(crate) async fn account_id(state: &AppState) -> AppResult<String> {
    state.active_id().await.ok_or(AppError::NotAuthenticated)
}

/// Basic-format UTC timestamp `YYYYMMDDThhmmssZ` — the format shared by iCal
/// `DTSTAMP`/`CREATED`/`LAST-MODIFIED` and vCard `REV`.
pub(crate) fn now_utc() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// 32 hex chars from `/dev/urandom` — a UID for a newly created event/contact.
pub(crate) fn rand_id() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
