//! Error classification (TODO 16c): maps raw error text to a friendlier
//! localized reason message while the raw text stays in `details`.
//!
//! The API layer surfaces errors as anyhow `Display` strings — reqwest status
//! lines (`xCloud request failed with 401 Unauthorized: …`), transport io
//! errors (`Connection refused (os error 111)`), timeouts (`timed out`) and
//! MS auth/XSTS payloads. Classification is therefore substring-based by
//! design; a structured `ApiError` carrying status + code would be cleaner
//! but touches every call site (deliberately deferred). Classes are
//! source-agnostic: one "network failed" phrasing serves sign-in, list
//! loading and stream start alike.
//!
//! Wired in exactly one place — `App::localized_error` — so every error
//! screen (sign-in, titles, consoles, stream) benefits without changing the
//! call sites. Patterns must never match our own detail strings (e.g.
//! "connection closed by the console" is deliberately NOT classified as a
//! network failure).

/// Returns an overriding `error-class-*` reason key when `raw` matches a
/// known failure class, or `None` to keep the caller's reason key.
pub(crate) fn classify_error(raw: &str) -> Option<&'static str> {
    let text = raw.to_ascii_lowercase();

    // Explicit HTTP status codes first (status lines are the most specific
    // signal; "504 gateway timeout" stays a server problem, not a timeout).
    for (needle, key) in [
        ("401", "error-class-auth"),
        ("403", "error-class-auth"),
        ("429", "error-class-ratelimit"),
        ("500", "error-class-server"),
        ("502", "error-class-server"),
        ("503", "error-class-server"),
        ("504", "error-class-server"),
    ] {
        if has_status_code(&text, needle) {
            return Some(key);
        }
    }

    // Auth/XSTS payload failures that arrive with a 200 shell.
    for needle in ["unauthorized", "forbidden", "xsts", "xauth", "token expired"] {
        if text.contains(needle) {
            return Some("error-class-auth");
        }
    }

    if text.contains("rate limit") || text.contains("too many requests") {
        return Some("error-class-ratelimit");
    }
    if text.contains("internal server error")
        || text.contains("bad gateway")
        || text.contains("service unavailable")
    {
        return Some("error-class-server");
    }
    if text.contains("timed out") || text.contains("timeout") {
        return Some("error-class-timeout");
    }
    for needle in [
        "dns error",
        "name or service not known",
        "name resolution",
        "getaddrinfo",
        "nodename nor servname",
    ] {
        if text.contains(needle) {
            return Some("error-class-dns");
        }
    }
    for needle in [
        "connection refused",
        "connection reset",
        "connection aborted",
        "broken pipe",
        "network unreachable",
        "no route to host",
        "network down",
        "connection closed before",
        "error connecting",
    ] {
        if text.contains(needle) {
            return Some("error-class-network");
        }
    }
    None
}

/// Substring test for an HTTP status code that is not part of a longer
/// number (so "1403 ms" or a session id containing 401 doesn't match).
fn has_status_code(text: &str, code: &str) -> bool {
    let mut from = 0;
    while let Some(at) = text[from..].find(code) {
        let start = from + at;
        let end = start + code.len();
        let before_ok = start == 0 || !text[..start].ends_with(char::is_numeric);
        let after_ok = end >= text.len() || !text[end..].starts_with(char::is_numeric);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_lines_classify() {
        assert_eq!(
            classify_error(
                "xCloud request failed with 401 Unauthorized: {\"error\":\"XERR\"}"
            ),
            Some("error-class-auth")
        );
        assert_eq!(
            classify_error("xCloud request failed with 429 Too Many Requests: busy"),
            Some("error-class-ratelimit")
        );
        assert_eq!(
            classify_error("xCloud request failed with 503 Service Unavailable: {}"),
            Some("error-class-server")
        );
    }

    #[test]
    fn transport_errors_classify() {
        assert_eq!(
            classify_error("xCloud HTTP request failed: Connection refused (os error 111)"),
            Some("error-class-network")
        );
        assert_eq!(
            classify_error("request timed out after 10s: deadline exceeded"),
            Some("error-class-timeout")
        );
        assert_eq!(
            classify_error("failed to lookup address information: Name or service not known"),
            Some("error-class-dns")
        );
    }

    #[test]
    fn console_close_detail_is_not_network() {
        // Our own teardown detail string must keep its dedicated reason key.
        assert_eq!(classify_error("connection closed by the console"), None);
    }

    #[test]
    fn embedded_codes_do_not_match() {
        // A status code inside a longer number or a session id is not a status.
        assert_eq!(classify_error("poll failed for session 14029 x 4012"), None);
        assert_eq!(classify_error("waited 5023 ms for provisioning"), None);
    }

    #[test]
    fn unknown_text_keeps_caller_reason() {
        assert_eq!(classify_error("xCloud exchange failed: provisioning"), None);
        assert_eq!(classify_error(""), None);
    }
}
