//! A tiny, lossless codec for the iCalendar (RFC 5545) / vCard (RFC 6350)
//! content-line grammar. Both formats share the exact same line syntax:
//!
//! ```text
//! name *(";" param-name "=" param-value) ":" value CRLF
//! ```
//!
//! plus *line folding* (a CRLF followed by a space/tab continues the previous
//! line). We parse a document into a tree of components (`BEGIN:X … END:X`),
//! each holding its content lines **verbatim** (params + value kept as raw
//! strings). That verbatimness is the whole point: when the user edits an
//! event's summary we rewrite only that one line and re-emit everything else —
//! including properties we don't understand (RRULE, VALARM, ATTENDEE, X-*) —
//! byte-for-byte, so a round-trip never loses data.

/// One `name;params:value` line, kept close to its on-the-wire form.
#[derive(Debug, Clone)]
pub struct Line {
    pub name: String,
    /// Everything between the name and the `:` — i.e. the raw parameter text,
    /// including each leading `;` (empty when there are no params).
    pub params: String,
    /// The raw (still-escaped for TEXT values) value, after unfolding.
    pub value: String,
}

impl Line {
    /// A plain line with no parameters.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Line { name: name.into(), params: String::new(), value: value.into() }
    }

    /// A line carrying raw parameter text (must start with `;`, e.g. `;VALUE=DATE`).
    pub fn with_params(
        name: impl Into<String>,
        params: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Line { name: name.into(), params: params.into(), value: value.into() }
    }

    /// Case-insensitive property-name match (iCal/vCard names are case-insensitive).
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }

    /// Value of a single parameter (case-insensitive name), unquoted, or `None`.
    pub fn param(&self, name: &str) -> Option<String> {
        for part in self.params.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let (k, v) = part.split_once('=')?;
            if k.trim().eq_ignore_ascii_case(name) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
        None
    }
}

/// A `BEGIN:NAME … END:NAME` block: its own lines plus nested components.
#[derive(Debug, Clone, Default)]
pub struct Component {
    pub name: String,
    pub lines: Vec<Line>,
    pub children: Vec<Component>,
}

impl Component {
    pub fn new(name: impl Into<String>) -> Self {
        Component { name: name.into(), lines: Vec::new(), children: Vec::new() }
    }

    /// First child component with the given (case-insensitive) name.
    #[cfg(test)]
    pub fn child(&self, name: &str) -> Option<&Component> {
        self.children.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// First line with the given (case-insensitive) property name.
    pub fn line(&self, name: &str) -> Option<&Line> {
        self.lines.iter().find(|l| l.is(name))
    }

    /// The raw value of a property, or `None` if absent.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.line(name).map(|l| l.value.as_str())
    }

    /// Replace the first line named `name`, or append one if none exists.
    pub fn set(&mut self, line: Line) {
        if let Some(existing) = self.lines.iter_mut().find(|l| l.is(&line.name)) {
            *existing = line;
        } else {
            self.lines.push(line);
        }
    }

    /// Remove every line with the given (case-insensitive) name.
    pub fn remove(&mut self, name: &str) {
        self.lines.retain(|l| !l.is(name));
    }
}

/// Unfold a raw document into logical lines (RFC 5545 §3.1). A line break
/// immediately followed by a space or tab is a continuation of the prior line.
fn unfold(input: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix(' ').or_else(|| line.strip_prefix('\t')) {
            if let Some(last) = out.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        // Skip blank separators between components but keep content lines.
        if line.is_empty() && out.last().map(|l| l.is_empty()).unwrap_or(true) {
            continue;
        }
        out.push(line.to_string());
    }
    out
}

/// Split a logical line into `(name, params, value)`. The `:` that ends the
/// name+params is the first one that is not inside a quoted parameter value.
fn split_line(line: &str) -> Option<(String, String, String)> {
    let bytes = line.as_bytes();
    let mut in_quote = false;
    let mut colon = None;
    let mut semi = None; // first (unquoted) ';' — where params begin
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'"' => in_quote = !in_quote,
            b';' if !in_quote && semi.is_none() && colon.is_none() => semi = Some(i),
            b':' if !in_quote => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let value = line[colon + 1..].to_string();
    match semi {
        Some(s) if s < colon => {
            let name = line[..s].to_string();
            let params = line[s..colon].to_string(); // includes leading ';'
            Some((name, params, value))
        }
        _ => Some((line[..colon].to_string(), String::new(), value)),
    }
}

/// Parse a full document into its top-level components (usually one VCALENDAR,
/// or a sequence of VCARDs). Lines outside any `BEGIN/END` are ignored.
pub fn parse(input: &str) -> Vec<Component> {
    let mut roots: Vec<Component> = Vec::new();
    let mut stack: Vec<Component> = Vec::new();
    for logical in unfold(input) {
        if logical.is_empty() {
            continue;
        }
        let Some((name, params, value)) = split_line(&logical) else { continue };
        if name.eq_ignore_ascii_case("BEGIN") {
            stack.push(Component::new(value.trim()));
        } else if name.eq_ignore_ascii_case("END") {
            if let Some(done) = stack.pop() {
                match stack.last_mut() {
                    Some(parent) => parent.children.push(done),
                    None => roots.push(done),
                }
            }
        } else if let Some(current) = stack.last_mut() {
            current.lines.push(Line { name, params, value });
        }
    }
    roots
}

/// Fold a single emitted line to <=75 octets per RFC 5545 §3.1 (split on byte
/// boundaries that don't cut a UTF-8 sequence), joining with CRLF + space.
fn fold(line: &str, out: &mut String) {
    let bytes = line.as_bytes();
    if bytes.len() <= 75 {
        out.push_str(line);
        out.push_str("\r\n");
        return;
    }
    let mut start = 0;
    let mut first = true;
    while start < bytes.len() {
        // Leave room for the leading space on continuation lines.
        let budget = if first { 75 } else { 74 };
        let mut end = (start + budget).min(bytes.len());
        // Back off so we never split inside a multi-byte UTF-8 char.
        while end > start && (bytes[end - 1] & 0xC0) == 0x80 {
            end -= 1;
        }
        // Also don't leave a lone lead byte at the boundary.
        while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
            end += 1;
        }
        if !first {
            out.push(' ');
        }
        out.push_str(&line[start..end]);
        out.push_str("\r\n");
        start = end;
        first = false;
    }
}

/// Serialize components back to a CRLF-delimited, folded document.
pub fn serialize(roots: &[Component]) -> String {
    let mut out = String::new();
    for c in roots {
        write_component(c, &mut out);
    }
    out
}

fn write_component(c: &Component, out: &mut String) {
    fold(&format!("BEGIN:{}", c.name), out);
    for l in &c.lines {
        fold(&format!("{}{}:{}", l.name, l.params, l.value), out);
    }
    for child in &c.children {
        write_component(child, out);
    }
    fold(&format!("END:{}", c.name), out);
}

/// Escape a TEXT value (RFC 5545 §3.3.11 / RFC 6350 §3.4): backslash, newline,
/// comma, semicolon. Used when writing user-entered text into a value.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Inverse of [`escape_text`] — decode an escaped TEXT value for display.
pub fn unescape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_unknown_lines() {
        let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:abc\r\nSUMMARY:Hi\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let roots = parse(ics);
        assert_eq!(roots.len(), 1);
        let vevent = roots[0].child("VEVENT").unwrap();
        assert_eq!(vevent.value("UID"), Some("abc"));
        assert_eq!(vevent.value("RRULE"), Some("FREQ=WEEKLY"));
        // Re-emitting keeps the RRULE we never interpreted.
        let out = serialize(&roots);
        assert!(out.contains("RRULE:FREQ=WEEKLY"));
    }

    #[test]
    fn unfolds_continuations() {
        let ics = "BEGIN:VEVENT\r\nDESCRIPTION:hello \r\n world\r\nEND:VEVENT\r\n";
        let roots = parse(ics);
        assert_eq!(roots[0].value("DESCRIPTION"), Some("hello world"));
    }

    #[test]
    fn parses_params_and_colon_in_value() {
        let line = "DTSTART;TZID=Europe/Berlin:20260708T140000";
        let (name, params, value) = split_line(line).unwrap();
        assert_eq!(name, "DTSTART");
        assert_eq!(params, ";TZID=Europe/Berlin");
        assert_eq!(value, "20260708T140000");
        let l = Line::with_params(name, params, value);
        assert_eq!(l.param("TZID").as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn folds_long_lines_at_75_octets() {
        let long = "x".repeat(200);
        let mut out = String::new();
        fold(&format!("NOTE:{long}"), &mut out);
        for line in out.split("\r\n").filter(|l| !l.is_empty()) {
            assert!(line.len() <= 75, "line too long: {}", line.len());
        }
        // Unfolding restores the original value.
        let roots_line = out.replace("\r\n ", "");
        assert!(roots_line.contains(&long));
    }

    #[test]
    fn escape_round_trip() {
        let s = "a, b; c\\d\ne";
        assert_eq!(unescape_text(&escape_text(s)), s);
    }
}
