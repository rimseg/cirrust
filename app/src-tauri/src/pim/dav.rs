//! Generic WebDAV verbs (PROPFIND / REPORT / PUT / DELETE / GET) against
//! arbitrary `/remote.php/dav/...` paths, plus a small `multistatus` parser.
//! CalDAV and CardDAV are just WebDAV with extra namespaces, so both build on
//! these. Everything reuses the already-authenticated [`WebDavClient`].

use crate::error::{AppError, AppResult};
use crate::webdav::WebDavClient;
use reqwest::Method;
use std::collections::HashMap;

fn method(bytes: &[u8]) -> Method {
    Method::from_bytes(bytes).expect("valid HTTP method")
}

/// One `<d:response>` from a `multistatus` body: its href, the props that came
/// back with a 2xx status (keyed by lower-cased local tag name), and any
/// `<cal:comp name="…">` component names (for `supported-calendar-component-set`).
#[derive(Debug, Default, Clone)]
pub struct DavResponse {
    pub href: String,
    pub props: HashMap<String, String>,
    pub comps: Vec<String>,
    pub resourcetypes: Vec<String>,
}

impl DavResponse {
    pub fn prop(&self, name: &str) -> Option<&str> {
        self.props.get(&name.to_ascii_lowercase()).map(|s| s.as_str())
    }
    pub fn is(&self, resourcetype: &str) -> bool {
        self.resourcetypes.iter().any(|r| r.eq_ignore_ascii_case(resourcetype))
    }
}

impl WebDavClient {
    /// Issue a PROPFIND with an XML body, returning the raw `multistatus` text.
    pub async fn dav_propfind_raw(
        &self,
        dav_path: &str,
        depth: &str,
        body: String,
    ) -> AppResult<String> {
        self.dav_body_request(method(b"PROPFIND"), dav_path, depth, body).await
    }

    /// Issue a REPORT with an XML body, returning the raw `multistatus` text.
    pub async fn dav_report_raw(
        &self,
        dav_path: &str,
        depth: &str,
        body: String,
    ) -> AppResult<String> {
        self.dav_body_request(method(b"REPORT"), dav_path, depth, body).await
    }

    async fn dav_body_request(
        &self,
        m: Method,
        dav_path: &str,
        depth: &str,
        body: String,
    ) -> AppResult<String> {
        let resp = self
            .dav_request(m, dav_path)
            .header("Depth", depth)
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() && status.as_u16() != 207 {
            return Err(AppError::Server {
                status: status.as_u16(),
                body: text.chars().take(500).collect(),
            });
        }
        Ok(text)
    }

    /// GET an item, returning `(etag, body)`. The ETag identifies the version we
    /// read, so a later update can guard with `If-Match`.
    pub async fn dav_get_item(&self, dav_path: &str) -> AppResult<(String, String)> {
        let resp = self.dav_request(Method::GET, dav_path).send().await?;
        let status = resp.status();
        let etag = header(&resp, reqwest::header::ETAG);
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(AppError::Server {
                status: status.as_u16(),
                body: text.chars().take(300).collect(),
            });
        }
        Ok((etag, text))
    }

    /// Create a new item (`If-None-Match: *` so a UID clash can't overwrite an
    /// existing object). Returns the server's ETag for the created resource.
    pub async fn dav_put_new(
        &self,
        dav_path: &str,
        content_type: &str,
        body: String,
    ) -> AppResult<String> {
        self.dav_put(dav_path, content_type, body, Some("*"), true).await
    }

    /// Update an existing item, guarded by `If-Match: <etag>` so a concurrent
    /// change on the server surfaces as a 412 instead of silently clobbering.
    pub async fn dav_put_update(
        &self,
        dav_path: &str,
        content_type: &str,
        body: String,
        etag: &str,
    ) -> AppResult<String> {
        self.dav_put(dav_path, content_type, body, Some(etag), false).await
    }

    async fn dav_put(
        &self,
        dav_path: &str,
        content_type: &str,
        body: String,
        if_header: Option<&str>,
        is_new: bool,
    ) -> AppResult<String> {
        let mut req = self
            .dav_request(Method::PUT, dav_path)
            .header("Content-Type", content_type)
            .body(body);
        if let Some(v) = if_header {
            req = req.header(if is_new { "If-None-Match" } else { "If-Match" }, v);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let code = status.as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: code, body: text.chars().take(300).collect() });
        }
        // The ETag header may be absent (some setups omit it on PUT); the caller
        // re-reads it from a follow-up PROPFIND when we return empty.
        Ok(header(&resp, reqwest::header::ETAG))
    }

    /// Delete an item, guarded by `If-Match` unless the caller passes an empty
    /// etag (force delete).
    pub async fn dav_delete_item(&self, dav_path: &str, etag: &str) -> AppResult<()> {
        let mut req = self.dav_request(Method::DELETE, dav_path);
        if !etag.is_empty() {
            req = req.header("If-Match", etag);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let code = status.as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Server { status: code, body: text.chars().take(300).collect() });
        }
        Ok(())
    }

    /// Read the current ETag of an item via a Depth-0 PROPFIND — used after a PUT
    /// whose response omitted the `ETag` header, so callers get a version tag to
    /// guard the next update with.
    pub async fn dav_fetch_etag(&self, dav_path: &str) -> AppResult<String> {
        let body = r#"<?xml version="1.0"?><d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#;
        let xml = self.dav_propfind_raw(dav_path, "0", body.to_string()).await?;
        let responses = parse_multistatus(&xml)?;
        Ok(responses.first().and_then(|r| r.prop("getetag")).unwrap_or_default().to_string())
    }
}

fn header(resp: &reqwest::Response, name: reqwest::header::HeaderName) -> String {
    resp.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()
}

/// Parse a WebDAV `multistatus` body into per-resource responses. Only props
/// under a 2xx `propstat` are kept. Namespaces are ignored (we key by local
/// tag name), which is robust across Nextcloud's DAV/CalDAV/CardDAV/oc/nc/cs
/// namespace zoo.
pub fn parse_multistatus(xml: &str) -> AppResult<Vec<DavResponse>> {
    let doc = roxmltree::Document::parse(xml)
        .map_err(|e| AppError::msg(format!("invalid DAV XML: {e}")))?;
    let mut out = Vec::new();
    for response in doc.descendants().filter(|n| local(n) == "response") {
        let mut r = DavResponse::default();
        if let Some(href) = response.children().find(|n| local(n) == "href") {
            r.href = href.text().unwrap_or_default().trim().to_string();
        }
        for propstat in response.children().filter(|n| local(n) == "propstat") {
            let ok = propstat
                .children()
                .find(|n| local(n) == "status")
                .and_then(|n| n.text())
                .map(|s| s.contains("200"))
                .unwrap_or(false);
            if !ok {
                continue;
            }
            let Some(prop) = propstat.children().find(|n| local(n) == "prop") else { continue };
            for p in prop.children().filter(|n| n.is_element()) {
                let key = local(&p);
                match key.as_str() {
                    "resourcetype" => {
                        for rt in p.children().filter(|n| n.is_element()) {
                            r.resourcetypes.push(local(&rt));
                        }
                    }
                    "supported-calendar-component-set" => {
                        for comp in p.descendants().filter(|n| local(n) == "comp") {
                            if let Some(name) = comp.attribute("name") {
                                r.comps.push(name.to_string());
                            }
                        }
                    }
                    _ => {
                        let text = collect_text(&p);
                        r.props.insert(key, text);
                    }
                }
            }
        }
        if !r.href.is_empty() {
            out.push(r);
        }
    }
    Ok(out)
}

/// Convert a `multistatus` href (server-absolute and percent-encoded, e.g.
/// `/remote.php/dav/calendars/alice/personal/ev.ics`, or occasionally a full
/// URL) into a decoded path relative to `/remote.php/dav/`, suitable for
/// [`WebDavClient::dav_request`] (which re-encodes it). Returns `None` if the
/// href isn't under the DAV root.
pub fn href_to_dav_path(href: &str) -> Option<String> {
    let path = if let Ok(u) = url::Url::parse(href) {
        u.path().to_string()
    } else {
        href.to_string()
    };
    let rel = path.split("/remote.php/dav/").nth(1)?;
    let decoded = percent_encoding::percent_decode_str(rel).decode_utf8_lossy();
    Some(decoded.trim_end_matches('/').to_string())
}

/// Local (namespace-stripped, lower-cased) tag name of a node.
fn local(n: &roxmltree::Node) -> String {
    n.tag_name().name().to_ascii_lowercase()
}

/// Concatenated text of an element (handles both direct text and, for things
/// like `<cal:calendar-data>`, CDATA/text children). Only genuine text nodes are
/// collected — iterating `descendants()` also yields each *element* node, and
/// calling `.text()` on an element returns its first text child, which would
/// double-count every value (e.g. `displayname` → `"ContactsContacts"`).
fn collect_text(n: &roxmltree::Node) -> String {
    let mut s = String::new();
    for d in n.descendants() {
        if d.is_text() {
            if let Some(t) = d.text() {
                s.push_str(t);
            }
        }
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_are_not_doubled() {
        let xml = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:cs="http://calendarserver.org/ns/">
  <d:response>
    <d:href>/remote.php/dav/addressbooks/users/alice/contacts/</d:href>
    <d:propstat>
      <d:prop>
        <d:displayname>Contacts</d:displayname>
        <cs:getctag>xyz</cs:getctag>
        <d:resourcetype><d:collection/><card:addressbook xmlns:card="urn:ietf:params:xml:ns:carddav"/></d:resourcetype>
      </d:prop>
      <d:status>HTTP/1.1 200 OK</d:status>
    </d:propstat>
  </d:response>
</d:multistatus>"#;
        let responses = parse_multistatus(xml).unwrap();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].prop("displayname"), Some("Contacts"));
        assert_eq!(responses[0].prop("getctag"), Some("xyz"));
        assert!(responses[0].is("addressbook"));
        assert_eq!(
            href_to_dav_path(&responses[0].href).as_deref(),
            Some("addressbooks/users/alice/contacts")
        );
    }
}
