//! vCard (RFC 6350 / 3.0) ⇄ display model for contacts. Same lossless strategy
//! as [`ical`](super::ical): reading pulls the common fields; writing patches an
//! existing card (preserving PHOTO, ADR, BDAY, CATEGORIES, X-* …) or builds a
//! fresh vCard 3.0 (the version Nextcloud Contacts interoperates with widely).

use super::contentline::{self, Component, Line};
use serde::{Deserialize, Serialize};

/// A `TYPE`d value — an email address or phone number with its label.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypedValue {
    /// e.g. `home`, `work`, `cell` (lower-cased for display).
    pub label: String,
    pub value: String,
}

/// A contact as shown/edited in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub uid: String,
    pub href: String,
    pub etag: String,
    pub addressbook_id: String,
    pub full_name: String,
    pub emails: Vec<TypedValue>,
    pub phones: Vec<TypedValue>,
    pub org: Option<String>,
    pub title: Option<String>,
    pub note: Option<String>,
}

/// Contact fields from the editor UI.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactInput {
    pub full_name: String,
    #[serde(default)]
    pub emails: Vec<TypedValue>,
    #[serde(default)]
    pub phones: Vec<TypedValue>,
    #[serde(default)]
    pub org: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Parse one CardDAV object (a single VCARD) into a display contact.
pub fn parse_contact(
    vcf: &str,
    addressbook_id: &str,
    href: &str,
    etag: &str,
) -> Option<Contact> {
    let roots = contentline::parse(vcf);
    let card = roots.iter().find(|c| c.name.eq_ignore_ascii_case("VCARD"))?;

    let full_name = card
        .value("FN")
        .map(contentline::unescape_text)
        .filter(|s| !s.is_empty())
        .or_else(|| card.value("N").map(name_from_n))
        .unwrap_or_default();

    let emails = typed_values(card, "EMAIL");
    let phones = typed_values(card, "TEL");
    let org = card
        .value("ORG")
        .map(|v| contentline::unescape_text(&v.replace(';', " · ")).trim().to_string())
        .filter(|s| !s.is_empty());

    Some(Contact {
        uid: card.value("UID").unwrap_or_default().to_string(),
        href: href.to_string(),
        etag: etag.to_string(),
        addressbook_id: addressbook_id.to_string(),
        full_name,
        emails,
        phones,
        org,
        title: card.value("TITLE").map(contentline::unescape_text).filter(|s| !s.is_empty()),
        note: card.value("NOTE").map(contentline::unescape_text).filter(|s| !s.is_empty()),
    })
}

/// Collect all `EMAIL`/`TEL` lines with their `TYPE` label.
fn typed_values(card: &Component, name: &str) -> Vec<TypedValue> {
    card.lines
        .iter()
        .filter(|l| l.is(name))
        .map(|l| TypedValue {
            label: l.param("TYPE").unwrap_or_default().to_ascii_lowercase(),
            value: contentline::unescape_text(&l.value),
        })
        .filter(|t| !t.value.is_empty())
        .collect()
}

/// Derive a display name from a structured `N:Last;First;Middle;Prefix;Suffix`.
fn name_from_n(n: &str) -> String {
    let parts: Vec<&str> = n.split(';').collect();
    let last = parts.first().copied().unwrap_or("").trim();
    let first = parts.get(1).copied().unwrap_or("").trim();
    format!("{first} {last}").trim().to_string()
}

/// Build a fresh vCard 3.0 for `input`. Returns the vcf text.
pub fn build_new(input: &ContactInput, uid: &str) -> String {
    let mut card = Component::new("VCARD");
    card.set(Line::new("VERSION", "3.0"));
    card.set(Line::new("PRODID", "-//Cirrust//Cirrust CardDAV//EN"));
    card.set(Line::new("UID", uid));
    apply_input(&mut card, input);
    card.set(Line::new("REV", super::now_utc()));
    contentline::serialize(&[card])
}

/// Patch an existing vCard, preserving properties we don't manage.
pub fn apply_edit(existing_vcf: &str, input: &ContactInput) -> Option<String> {
    let mut roots = contentline::parse(existing_vcf);
    let card = roots.iter_mut().find(|c| c.name.eq_ignore_ascii_case("VCARD"))?;
    apply_input(card, input);
    card.set(Line::new("REV", super::now_utc()));
    Some(contentline::serialize(&roots))
}

/// Write the editable fields onto a VCARD component.
fn apply_input(card: &mut Component, input: &ContactInput) {
    let fn_ = input.full_name.trim();
    card.set(Line::new("FN", contentline::escape_text(fn_)));
    card.set(Line::new("N", structured_name(fn_)));

    // Rebuild the multi-valued EMAIL/TEL sets from the input.
    card.remove("EMAIL");
    card.remove("TEL");
    for e in input.emails.iter().filter(|e| !e.value.trim().is_empty()) {
        card.lines.push(typed_line("EMAIL", &e.label, &e.value));
    }
    for p in input.phones.iter().filter(|p| !p.value.trim().is_empty()) {
        card.lines.push(typed_line("TEL", &p.label, &p.value));
    }

    set_or_remove(card, "ORG", input.org.as_deref());
    set_or_remove(card, "TITLE", input.title.as_deref());
    set_or_remove(card, "NOTE", input.note.as_deref());
}

fn typed_line(name: &str, label: &str, value: &str) -> Line {
    let label = label.trim();
    let params = if label.is_empty() {
        String::new()
    } else {
        format!(";TYPE={}", label.to_ascii_uppercase())
    };
    Line::with_params(name, params, contentline::escape_text(value.trim()))
}

fn set_or_remove(card: &mut Component, name: &str, value: Option<&str>) {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        Some(v) => card.set(Line::new(name, contentline::escape_text(v))),
        None => card.remove(name),
    }
}

/// `Full Name` → `Last;First;;;` (best-effort split on the last space).
fn structured_name(full: &str) -> String {
    let full = full.trim();
    match full.rsplit_once(' ') {
        Some((first, last)) => {
            format!("{};{};;;", contentline::escape_text(last), contentline::escape_text(first))
        }
        None => format!(";{};;;", contentline::escape_text(full)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_common_fields() {
        let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Ada Lovelace\r\nEMAIL;TYPE=WORK:ada@x.com\r\nTEL;TYPE=CELL:+1555\r\nORG:Analytical;Engines\r\nEND:VCARD\r\n";
        let c = parse_contact(vcf, "contacts", "addressbooks/a/contacts/u1.vcf", "e1").unwrap();
        assert_eq!(c.full_name, "Ada Lovelace");
        assert_eq!(c.emails[0].value, "ada@x.com");
        assert_eq!(c.emails[0].label, "work");
        assert_eq!(c.phones[0].value, "+1555");
        assert_eq!(c.org.as_deref(), Some("Analytical · Engines"));
    }

    #[test]
    fn edit_preserves_photo_and_rebuilds_emails() {
        let vcf = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:u1\r\nFN:Old Name\r\nPHOTO;ENCODING=b:AAAA\r\nEMAIL:old@x.com\r\nEND:VCARD\r\n";
        let input = ContactInput {
            full_name: "New Name".into(),
            emails: vec![TypedValue { label: "home".into(), value: "new@x.com".into() }],
            phones: vec![],
            org: Some("Acme".into()),
            title: None,
            note: None,
        };
        let out = apply_edit(vcf, &input).unwrap();
        assert!(out.contains("FN:New Name"));
        assert!(out.contains("N:Name;New;;;"));
        assert!(out.contains("PHOTO;ENCODING=b:AAAA")); // preserved
        assert!(out.contains("EMAIL;TYPE=HOME:new@x.com"));
        assert!(!out.contains("old@x.com")); // old email replaced
        assert!(out.contains("ORG:Acme"));
    }
}
