//! CalDAV: calendar discovery, event fetch, and two-way event editing, exposed
//! as Tauri commands. Data is cached per account on disk (see [`super::store`]);
//! a refresh re-reads only calendars whose CTag changed.

use super::dav::{href_to_dav_path, parse_multistatus};
use super::ical::{self, CalEvent, EventInput};
use super::{account_id, rand_id, store};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::webdav::WebDavClient;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};

const CAL_CT: &str = "text/calendar; charset=utf-8";

/// A calendar collection the user can browse/edit.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarInfo {
    /// Last path segment of the collection (stable per account), e.g. `personal`.
    pub id: String,
    /// DAV path (relative to `/remote.php/dav/`) of the collection.
    pub href: String,
    pub display_name: String,
    pub color: Option<String>,
    /// Collection tag — changes whenever any event in it changes.
    pub ctag: String,
}

/// PROPFIND the calendar home and return the writable event calendars.
async fn discover(client: &WebDavClient) -> AppResult<Vec<CalendarInfo>> {
    let home = format!("calendars/{}", client.user());
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:ic="http://apple.com/ns/ical/">
  <d:prop>
    <d:resourcetype/>
    <d:displayname/>
    <cs:getctag/>
    <ic:calendar-color/>
    <c:supported-calendar-component-set/>
  </d:prop>
</d:propfind>"#;
    let xml = client.dav_propfind_raw(&home, "1", body.to_string()).await?;
    let responses = parse_multistatus(&xml)?;

    let mut out = Vec::new();
    for r in responses {
        if !r.is("calendar") {
            continue; // the home collection + non-calendar resources
        }
        // Only calendars that hold events (skip VTODO/VJOURNAL-only ones).
        if !r.comps.is_empty() && !r.comps.iter().any(|c| c.eq_ignore_ascii_case("VEVENT")) {
            continue;
        }
        let Some(dav_path) = href_to_dav_path(&r.href) else { continue };
        let id = dav_path.rsplit('/').next().unwrap_or(&dav_path).to_string();
        if id.is_empty() {
            continue;
        }
        out.push(CalendarInfo {
            id: id.clone(),
            href: dav_path,
            display_name: r.prop("displayname").filter(|s| !s.is_empty()).unwrap_or(&id).to_string(),
            color: r.prop("calendar-color").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            ctag: r.prop("getctag").unwrap_or_default().to_string(),
        });
    }
    Ok(out)
}

/// REPORT every VEVENT object in a calendar, parsed into display events.
async fn fetch_events(client: &WebDavClient, cal: &CalendarInfo) -> AppResult<Vec<CalEvent>> {
    let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">
  <d:prop><d:getetag/><c:calendar-data/></d:prop>
  <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter>
</c:calendar-query>"#;
    let xml = client.dav_report_raw(&cal.href, "1", body.to_string()).await?;
    let responses = parse_multistatus(&xml)?;

    let mut events = Vec::new();
    for r in responses {
        let Some(data) = r.prop("calendar-data") else { continue };
        let Some(dav_path) = href_to_dav_path(&r.href) else { continue };
        let etag = r.prop("getetag").unwrap_or_default().to_string();
        if let Some(ev) = ical::parse_event(data, &cal.id, &dav_path, &etag) {
            events.push(ev);
        }
    }
    Ok(events)
}

async fn active(app: &AppHandle, state: &AppState) -> AppResult<(String, WebDavClient)> {
    let id = account_id(state).await?;
    let client = state.client().await?;
    let _ = app; // app used by callers for cache
    Ok((id, client))
}

/// Return calendars from cache, refreshing from the server if the cache is cold.
#[tauri::command]
pub async fn caldav_calendars(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<Vec<CalendarInfo>> {
    let id = account_id(&state).await?;
    match store::load::<Vec<CalendarInfo>>(&app, &id, "calendars")? {
        Some(cals) if !cals.is_empty() => Ok(cals),
        _ => caldav_refresh(app.clone(), state).await,
    }
}

/// Reconcile calendars + events against the server. Only calendars whose CTag
/// changed (or that have no cached events) are re-fetched.
#[tauri::command]
pub async fn caldav_refresh(
    app: AppHandle,
    state: State<'_, AppState>,
) -> AppResult<Vec<CalendarInfo>> {
    let (id, client) = active(&app, &state).await?;
    let calendars = discover(&client).await?;
    let previous = store::load::<Vec<CalendarInfo>>(&app, &id, "calendars")?.unwrap_or_default();

    for cal in &calendars {
        let unchanged = previous
            .iter()
            .find(|p| p.id == cal.id)
            .map(|p| !p.ctag.is_empty() && p.ctag == cal.ctag)
            .unwrap_or(false);
        let cache_name = store::safe_name("events", &cal.id);
        let has_cache = store::load::<Vec<CalEvent>>(&app, &id, &cache_name)?.is_some();
        if unchanged && has_cache {
            continue;
        }
        let events = fetch_events(&client, cal).await?;
        store::save(&app, &id, &cache_name, &events)?;
    }

    // Drop caches for calendars that disappeared server-side.
    store::save(&app, &id, "calendars", &calendars)?;
    Ok(calendars)
}

/// Events for the given calendars (or all cached calendars when `None`).
#[tauri::command]
pub async fn caldav_events(
    app: AppHandle,
    state: State<'_, AppState>,
    calendar_ids: Option<Vec<String>>,
) -> AppResult<Vec<CalEvent>> {
    let id = account_id(&state).await?;
    let calendars = store::load::<Vec<CalendarInfo>>(&app, &id, "calendars")?.unwrap_or_default();
    let wanted: Vec<&CalendarInfo> = match &calendar_ids {
        Some(ids) => calendars.iter().filter(|c| ids.contains(&c.id)).collect(),
        None => calendars.iter().collect(),
    };
    let mut all = Vec::new();
    for cal in wanted {
        let name = store::safe_name("events", &cal.id);
        if let Some(events) = store::load::<Vec<CalEvent>>(&app, &id, &name)? {
            all.extend(events);
        }
    }
    Ok(all)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveEventArgs {
    pub calendar_id: String,
    /// Present when editing an existing event; absent to create a new one.
    #[serde(default)]
    pub href: Option<String>,
    #[serde(default)]
    pub etag: Option<String>,
    pub event: EventInput,
}

/// Create or update an event, then refresh that calendar's cache.
#[tauri::command]
pub async fn caldav_save_event(
    app: AppHandle,
    state: State<'_, AppState>,
    args: SaveEventArgs,
) -> AppResult<CalEvent> {
    let (id, client) = active(&app, &state).await?;
    let calendars = store::load::<Vec<CalendarInfo>>(&app, &id, "calendars")?.unwrap_or_default();
    let cal = calendars
        .iter()
        .find(|c| c.id == args.calendar_id)
        .ok_or_else(|| AppError::msg("unknown calendar"))?;

    let (dav_path, ics) = match (&args.href, &args.etag) {
        (Some(href), Some(etag)) => {
            // Edit in place — fetch current body so unknown props survive.
            let (_, existing) = client.dav_get_item(href).await?;
            let ics = ical::apply_edit(&existing, &args.event)
                .ok_or_else(|| AppError::msg("event body has no VEVENT"))?;
            client.dav_put_update(href, CAL_CT, ics.clone(), etag).await?;
            (href.clone(), ics)
        }
        _ => {
            let uid = format!("{}@cirrust", rand_id());
            let dav_path = format!("{}/{}.ics", cal.href, uid);
            let ics = ical::build_new(&args.event, &uid);
            client.dav_put_new(&dav_path, CAL_CT, ics.clone()).await?;
            (dav_path, ics)
        }
    };

    let etag = client.dav_fetch_etag(&dav_path).await.unwrap_or_default();
    let event = ical::parse_event(&ics, &cal.id, &dav_path, &etag)
        .ok_or_else(|| AppError::msg("failed to parse saved event"))?;
    upsert_cache(&app, &id, &cal.id, &event.href, Some(event.clone()))?;
    Ok(event)
}

#[tauri::command]
pub async fn caldav_delete_event(
    app: AppHandle,
    state: State<'_, AppState>,
    calendar_id: String,
    href: String,
    etag: Option<String>,
) -> AppResult<()> {
    let (id, client) = active(&app, &state).await?;
    client.dav_delete_item(&href, etag.as_deref().unwrap_or("")).await?;
    upsert_cache(&app, &id, &calendar_id, &href, None)?;
    Ok(())
}

/// Insert/replace (or, when `value` is `None`, remove) one item in a calendar's
/// cached event list, keyed by href.
fn upsert_cache(
    app: &AppHandle,
    account: &str,
    calendar_id: &str,
    href: &str,
    value: Option<CalEvent>,
) -> AppResult<()> {
    let name = store::safe_name("events", calendar_id);
    let mut events = store::load::<Vec<CalEvent>>(app, account, &name)?.unwrap_or_default();
    events.retain(|e| e.href != href);
    if let Some(v) = value {
        events.push(v);
    }
    store::save(app, account, &name, &events)
}
