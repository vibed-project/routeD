// SPDX-License-Identifier: Apache-2.0
//! `X-Routed-*` request header handling.

use routed_decision::RequestHints;

/// Prefix of every routeD request and response header (case-insensitive).
pub const HEADER_PREFIX: &str = "x-routed-";

/// Whether a header name belongs to routeD and must be stripped from inbound
/// requests before forwarding (prevents spoofing of decision headers).
#[must_use]
pub fn is_routed_header(name: &str) -> bool {
    name.as_bytes()
        .get(..HEADER_PREFIX.len())
        .is_some_and(|b| b.eq_ignore_ascii_case(HEADER_PREFIX.as_bytes()))
}

/// Parsed request headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestHeaders {
    /// `X-Routed-Tenant`.
    pub tenant: Option<String>,
    /// `X-Routed-Agent`.
    pub agent: Option<String>,
    /// Restriction-only hints.
    pub hints: RequestHints,
    /// `X-Routed-Path` (decision API only: the path the gateway will serve).
    pub path: Option<String>,
    /// Headers that were ignored (unknown `X-Routed-*` names, malformed values).
    pub ignored: Vec<String>,
}

fn clean(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() || v.len() > 256 || v.chars().any(char::is_control) {
        return None;
    }
    Some(v.to_owned())
}

/// Extract routeD headers from an iterator of `(name, value)` pairs.
///
/// Duplicate `X-Routed-Data-Class` values are all kept (the engine merges them
/// by maximum rank, so duplicates can only tighten). A duplicate tenant, agent
/// or policy keeps the first value and records the rest as ignored. Values
/// with control characters or longer than 256 bytes are ignored.
pub fn extract_headers<'a, I, N, V>(headers: I) -> RequestHeaders
where
    I: IntoIterator<Item = (N, V)>,
    N: AsRef<str> + 'a,
    V: AsRef<str> + 'a,
{
    let mut out = RequestHeaders::default();
    for (name, value) in headers {
        let name = name.as_ref();
        if !is_routed_header(name) {
            continue;
        }
        let Some(key) = name.get(HEADER_PREFIX.len()..).map(str::to_ascii_lowercase) else {
            out.ignored.push(format!("{name}: malformed name"));
            continue;
        };
        let Some(v) = clean(value.as_ref()) else {
            out.ignored.push(format!("{name}: malformed value"));
            continue;
        };
        match key.as_str() {
            "tenant" => set_once(&mut out.tenant, v, name, &mut out.ignored),
            "agent" => set_once(&mut out.agent, v, name, &mut out.ignored),
            "policy" => set_once(&mut out.hints.policy, v, name, &mut out.ignored),
            "path" => set_once(&mut out.path, v, name, &mut out.ignored),
            "data-class" => {
                for part in v.split(',') {
                    if let Some(p) = clean(part) {
                        out.hints.data_classes.push(p.to_ascii_lowercase());
                    }
                }
            }
            "dry-run" => {
                if v.eq_ignore_ascii_case("true") || v == "1" {
                    out.hints.dry_run = true;
                } else if !(v.eq_ignore_ascii_case("false") || v == "0") {
                    out.ignored.push(format!("{name}: expected true or false"));
                }
            }
            // Response-only / unknown names are never honoured inbound.
            _ => out.ignored.push(format!("{name}: not an input header")),
        }
    }
    out.hints.data_classes.sort();
    out.hints.data_classes.dedup();
    out
}

fn set_once(slot: &mut Option<String>, value: String, name: &str, ignored: &mut Vec<String>) {
    if slot.is_some() {
        ignored.push(format!("{name}: duplicate header, first value kept"));
    } else {
        *slot = Some(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_headers_case_insensitively() {
        let h = extract_headers([
            ("X-Routed-Tenant", "acme"),
            ("x-routed-agent", "bot-1"),
            ("X-ROUTED-DATA-CLASS", "Personal"),
            ("X-Routed-Data-Class", "internal, public"),
            ("X-Routed-Policy", "strict"),
            ("X-Routed-Dry-Run", "TRUE"),
            ("Content-Type", "application/json"),
        ]);
        assert_eq!(h.tenant.as_deref(), Some("acme"));
        assert_eq!(h.agent.as_deref(), Some("bot-1"));
        assert_eq!(h.hints.data_classes, vec!["internal", "personal", "public"]);
        assert_eq!(h.hints.policy.as_deref(), Some("strict"));
        assert!(h.hints.dry_run);
        assert!(h.ignored.is_empty());
    }

    #[test]
    fn spoofed_decision_headers_are_ignored_and_flagged_for_stripping() {
        let h = extract_headers([
            ("X-Routed-Decision", "eyJ..."),
            ("X-Routed-Tier", "us-cheap"),
            ("X-Routed-Outcome", "ROUTE"),
        ]);
        assert_eq!(h.ignored.len(), 3);
        assert!(is_routed_header("X-Routed-Decision"));
        assert!(is_routed_header("x-routed-tier"));
        assert!(!is_routed_header("x-route"));
        assert!(!is_routed_header("authorization"));
    }

    #[test]
    fn malformed_and_duplicate_values() {
        let h = extract_headers([
            ("X-Routed-Tenant", "a"),
            ("X-Routed-Tenant", "b"),
            ("X-Routed-Policy", "bad\r\nvalue"),
            ("X-Routed-Dry-Run", "maybe"),
            ("X-Routed-Agent", &"x".repeat(300)),
        ]);
        assert_eq!(h.tenant.as_deref(), Some("a"));
        assert!(h.hints.policy.is_none());
        assert!(!h.hints.dry_run);
        assert!(h.agent.is_none());
        assert_eq!(h.ignored.len(), 4);
    }
}
