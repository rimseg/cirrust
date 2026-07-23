//! iCalendar (VEVENT) ⇄ display model. Reading extracts the fields the UI shows;
//! writing either builds a fresh VCALENDAR or patches an existing one in place,
//! preserving every property we don't touch (RRULE, VALARM, ATTENDEE, X-*) via
//! the [`contentline`](super::contentline) codec.
//!
//! Time handling (v1, no IANA tz database bundled): all-day events use
//! `VALUE=DATE`; timed events are read as wall-clock (UTC `Z` values are
//! converted to the machine's local time for display) and written back as
//! *floating* local time. For a single-timezone user this round-trips exactly;
//! cross-timezone precision would need `chrono-tz` (a deliberate future add).

use super::contentline::{self, Component, Line};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// A calendar event as shown/edited in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalEvent {
    pub uid: String,
    /// DAV path relative to `/remote.php/dav/` — the object we PUT/DELETE.
    pub href: String,
    pub etag: String,
    pub calendar_id: String,
    pub summary: String,
    pub description: Option<String>,
    pub location: Option<String>,
    /// `YYYY-MM-DD` when `all_day`, else `YYYY-MM-DDTHH:MM:SS` (local wall-clock).
    pub start: String,
    pub end: Option<String>,
    pub all_day: bool,
    pub rrule: Option<String>,
    pub status: Option<String>,
}

/// Event fields coming back from the editor UI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventInput {
    pub summary: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    /// `YYYY-MM-DD` (all-day) or `YYYY-MM-DDTHH:MM[:SS]` (timed).
    pub start: String,
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub all_day: bool,
}

/// Parse the (possibly multi-VEVENT) VCALENDAR body of one CalDAV object into a
/// single display event, taking the master component (no `RECURRENCE-ID`).
pub fn parse_event(
    ics: &str,
    calendar_id: &str,
    href: &str,
    etag: &str,
) -> Option<CalEvent> {
    let roots = contentline::parse(ics);
    let vcal = roots.iter().find(|c| c.name.eq_ignore_ascii_case("VCALENDAR"))?;
    let vevent = vcal
        .children
        .iter()
        .filter(|c| c.name.eq_ignore_ascii_case("VEVENT"))
        .find(|c| c.line("RECURRENCE-ID").is_none())
        .or_else(|| vcal.children.iter().find(|c| c.name.eq_ignore_ascii_case("VEVENT")))?;

    let (start, all_day) = vevent.line("DTSTART").map(to_display).unwrap_or_default();
    let end = vevent.line("DTEND").map(|l| to_display(l).0).or_else(|| {
        // Fall back to DTSTART + DURATION-less default handled in the UI.
        None
    });

    Some(CalEvent {
        uid: vevent.value("UID").unwrap_or_default().to_string(),
        href: href.to_string(),
        etag: etag.to_string(),
        calendar_id: calendar_id.to_string(),
        summary: text(vevent, "SUMMARY").unwrap_or_default(),
        description: text(vevent, "DESCRIPTION").filter(|s| !s.is_empty()),
        location: text(vevent, "LOCATION").filter(|s| !s.is_empty()),
        start,
        end,
        all_day,
        rrule: vevent.value("RRULE").map(|s| s.to_string()),
        status: vevent.value("STATUS").map(|s| s.to_string()),
    })
}

fn text(c: &Component, name: &str) -> Option<String> {
    c.value(name).map(contentline::unescape_text)
}

/// Build a brand-new VCALENDAR/VEVENT for `input`. Returns `(uid, ics_text)`.
pub fn build_new(input: &EventInput, uid: &str) -> String {
    let mut vevent = Component::new("VEVENT");
    vevent.set(Line::new("UID", uid));
    vevent.set(Line::new("DTSTAMP", super::now_utc()));
    vevent.set(Line::new("CREATED", super::now_utc()));
    vevent.set(Line::new("SEQUENCE", "0"));
    apply_input(&mut vevent, input);

    let mut vcal = Component::new("VCALENDAR");
    vcal.set(Line::new("VERSION", "2.0"));
    vcal.set(Line::new("PRODID", "-//Cirrust//Cirrust CalDAV//EN"));
    vcal.set(Line::new("CALSCALE", "GREGORIAN"));
    vcal.children.push(vevent);
    contentline::serialize(&[vcal])
}

/// Patch an existing CalDAV object in place, preserving unknown properties.
/// Returns the new ics text, or `None` if the body has no VEVENT to edit.
pub fn apply_edit(existing_ics: &str, input: &EventInput) -> Option<String> {
    let mut roots = contentline::parse(existing_ics);
    let vcal = roots.iter_mut().find(|c| c.name.eq_ignore_ascii_case("VCALENDAR"))?;
    // Position-then-index avoids overlapping mutable borrows of `children`.
    let idx = vcal
        .children
        .iter()
        .position(|c| c.name.eq_ignore_ascii_case("VEVENT") && c.line("RECURRENCE-ID").is_none())
        .or_else(|| {
            vcal.children.iter().position(|c| c.name.eq_ignore_ascii_case("VEVENT"))
        })?;
    let vevent = &mut vcal.children[idx];

    // Bump SEQUENCE and refresh modification stamps so servers/other clients
    // recognize this as a newer revision.
    let seq = vevent.value("SEQUENCE").and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    vevent.set(Line::new("SEQUENCE", (seq + 1).to_string()));
    vevent.set(Line::new("DTSTAMP", super::now_utc()));
    vevent.set(Line::new("LAST-MODIFIED", super::now_utc()));
    apply_input(vevent, input);
    Some(contentline::serialize(&roots))
}

/// Write the editable fields onto a VEVENT component.
fn apply_input(vevent: &mut Component, input: &EventInput) {
    vevent.set(Line::new("SUMMARY", contentline::escape_text(input.summary.trim())));

    match input.description.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(d) => vevent.set(Line::new("DESCRIPTION", contentline::escape_text(d))),
        None => vevent.remove("DESCRIPTION"),
    }
    match input.location.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(l) => vevent.set(Line::new("LOCATION", contentline::escape_text(l))),
        None => vevent.remove("LOCATION"),
    }

    let (sp, sv) = to_ical(&input.start, input.all_day);
    vevent.set(Line::with_params("DTSTART", sp, sv));

    let end_line = compute_end(input);
    vevent.set(end_line);
}

/// Produce the DTEND line, defaulting sensibly when the UI omits an end.
fn compute_end(input: &EventInput) -> Line {
    if input.all_day {
        // All-day DTEND is exclusive; store the day *after* the last day.
        let last = parse_date(input.end.as_deref().unwrap_or(&input.start))
            .or_else(|| parse_date(&input.start));
        let excl = last.map(|d| d + Duration::days(1));
        let v = excl.map(|d| d.format("%Y%m%d").to_string()).unwrap_or_default();
        Line::with_params("DTEND", ";VALUE=DATE", v)
    } else {
        let end = input
            .end
            .as_deref()
            .and_then(parse_datetime)
            .or_else(|| parse_datetime(&input.start).map(|d| d + Duration::hours(1)));
        let v = end.map(|d| d.format("%Y%m%dT%H%M%S").to_string()).unwrap_or_default();
        Line::new("DTEND", v)
    }
}

/// iCal date/time value → display string + all-day flag.
fn to_display(line: &Line) -> (String, bool) {
    let v = line.value.trim();
    let is_date = line.param("VALUE").as_deref() == Some("DATE")
        || (v.len() == 8 && !v.contains('T'));
    if is_date {
        if v.len() >= 8 {
            return (format!("{}-{}-{}", &v[0..4], &v[4..6], &v[6..8]), true);
        }
        return (v.to_string(), true);
    }
    if let Some(naive) = parse_ical_naive(v) {
        if v.ends_with('Z') {
            let local = Utc.from_utc_datetime(&naive).with_timezone(&Local).naive_local();
            return (local.format("%Y-%m-%dT%H:%M:%S").to_string(), false);
        }
        return (naive.format("%Y-%m-%dT%H:%M:%S").to_string(), false);
    }
    (v.to_string(), false)
}

/// Display string → `(params, value)` for a DTSTART/DTEND line.
fn to_ical(display: &str, all_day: bool) -> (String, String) {
    if all_day {
        let v = parse_date(display).map(|d| d.format("%Y%m%d").to_string()).unwrap_or_default();
        (";VALUE=DATE".to_string(), v)
    } else {
        let v = parse_datetime(display)
            .map(|d| d.format("%Y%m%dT%H%M%S").to_string())
            .unwrap_or_default();
        (String::new(), v)
    }
}

fn parse_ical_naive(v: &str) -> Option<NaiveDateTime> {
    let trimmed = v.trim_end_matches('Z');
    NaiveDateTime::parse_from_str(trimmed, "%Y%m%dT%H%M%S").ok()
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .or_else(|| NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d").ok())
}

fn parse_datetime(s: &str) -> Option<NaiveDateTime> {
    let s = s.trim();
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .or_else(|| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M").ok())
        // Bare date (someone toggled all-day off but sent only a date).
        .or_else(|| parse_date(s).map(|d| d.and_hms_opt(0, 0, 0).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_timed_and_allday() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Meeting\r\nDTSTART:20260708T140000\r\nDTEND:20260708T150000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let ev = parse_event(ics, "personal", "calendars/a/personal/1.ics", "e1").unwrap();
        assert_eq!(ev.summary, "Meeting");
        assert!(!ev.all_day);
        assert_eq!(ev.start, "2026-07-08T14:00:00");

        let ad = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:2\r\nSUMMARY:Trip\r\nDTSTART;VALUE=DATE:20260708\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let ev = parse_event(ad, "personal", "x", "e2").unwrap();
        assert!(ev.all_day);
        assert_eq!(ev.start, "2026-07-08");
    }

    #[test]
    fn edit_preserves_unknown_and_bumps_sequence() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:1\r\nSEQUENCE:2\r\nSUMMARY:Old\r\nDTSTART:20260708T140000\r\nRRULE:FREQ=DAILY\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let input = EventInput {
            summary: "New".into(),
            description: Some("hi".into()),
            location: None,
            start: "2026-07-09T10:00".into(),
            end: Some("2026-07-09T11:00".into()),
            all_day: false,
        };
        let out = apply_edit(ics, &input).unwrap();
        assert!(out.contains("SUMMARY:New"));
        assert!(out.contains("DESCRIPTION:hi"));
        assert!(out.contains("RRULE:FREQ=DAILY")); // preserved
        assert!(out.contains("BEGIN:VALARM")); // preserved
        assert!(out.contains("SEQUENCE:3")); // bumped
        assert!(out.contains("DTSTART:20260709T100000"));
    }

    #[test]
    fn builds_allday_with_exclusive_end() {
        let input = EventInput {
            summary: "Holiday".into(),
            description: None,
            location: None,
            start: "2026-12-24".into(),
            end: Some("2026-12-26".into()),
            all_day: true,
        };
        let ics = build_new(&input, "uid@cirrust");
        assert!(ics.contains("DTSTART;VALUE=DATE:20261224"));
        assert!(ics.contains("DTEND;VALUE=DATE:20261227")); // last day + 1
    }
}
