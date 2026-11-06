//! Security headers and CSRF protection, applied to every response.
//!
//! - Content-Security-Policy per area: dashboard (strict, no inline script), player
//!   (allows sandboxed inline HTML widgets), uploaded files (risky types are served
//!   as a sandboxed, opaque-origin document), API (no content at all).
//! - Permissions-Policy, Cross-Origin-Opener/Resource-Policy, nosniff, Referrer-Policy,
//!   frame-ancestors / X-Frame-Options, HSTS over HTTPS, no-store on API responses.
//! - CSRF: state-changing API calls authenticated by cookie must come from our own
//!   origin (Origin / Sec-Fetch-Site check), on top of SameSite=Strict cookies.

use axum::{
    body::Body,
    extract::Request,
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

const CSP_APP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; \
media-src 'self' blob:; font-src 'self'; connect-src 'self'; frame-src 'self'; worker-src 'self'; object-src 'none'; \
base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

// HTML widgets render in sandboxed srcdoc iframes, which inherit this policy, so
// inline script is allowed here; those iframes run with an opaque origin (no access
// to the player's storage or cookies).
const CSP_PLAYER: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline' https:; \
img-src 'self' data: blob: https:; media-src 'self' blob: https:; font-src 'self' data: https:; \
connect-src 'self' https://api.open-meteo.com ipc: http://ipc.localhost; frame-src 'self' https: http:; worker-src 'self'; object-src 'none'; \
base-uri 'none'; form-action 'none'; frame-ancestors 'self'";

// Uploaded HTML/SVG/XML: never able to run as our origin.
const CSP_FILE_ACTIVE: &str = "sandbox allow-scripts; default-src 'self' 'unsafe-inline' data: blob:; object-src 'none'; base-uri 'none'";

const CSP_API: &str = "default-src 'none'; frame-ancestors 'none'";

const PERMISSIONS: &str = "camera=(), microphone=(), geolocation=(), payment=(), usb=(), serial=(), \
fullscreen=(self), autoplay=(self), screen-wake-lock=(self)";

fn set(h: &mut HeaderMap, k: HeaderName, v: &'static str) {
    if !h.contains_key(&k) {
        h.insert(k, HeaderValue::from_static(v));
    }
}

fn is_active_file(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".html", ".htm", ".xhtml", ".svg", ".svgz", ".xml"].iter().any(|e| p.ends_with(e))
}

/// Host the browser thinks it is talking to (proxy-aware).
fn request_host(h: &HeaderMap) -> Option<String> {
    h.get("x-forwarded-host").or_else(|| h.get(header::HOST)).and_then(|v| v.to_str().ok()).map(|s| s.to_ascii_lowercase())
}

fn csrf_exempt(path: &str) -> bool {
    ["/api/hooks/", "/api/alert-hooks/", "/api/public/", "/api/player/"].iter().any(|p| path.starts_with(p))
}

fn csrf_ok(req: &Request) -> bool {
    let m = req.method();
    if matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let path = req.uri().path();
    if !path.starts_with("/api/") || csrf_exempt(path) {
        return true;
    }
    let h = req.headers();
    // Bearer-token API clients don't carry ambient credentials.
    if h.contains_key(header::AUTHORIZATION) {
        return true;
    }
    if let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin == "null" {
            return false;
        }
        let origin_host = origin.split("://").nth(1).unwrap_or("").trim_end_matches('/').to_ascii_lowercase();
        return request_host(h).map(|host| host == origin_host).unwrap_or(false);
    }
    // No Origin header (some same-origin requests): rely on Fetch Metadata when present.
    !matches!(h.get("sec-fetch-site").and_then(|v| v.to_str().ok()), Some("cross-site") | Some("same-site"))
}

pub async fn security(req: Request, next: Next) -> Response {
    if !csrf_ok(&req) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "cross-site request blocked" }))).into_response();
    }
    let path = req.uri().path().to_string();
    let https = crate::session::is_https(req.headers());
    let mut res: Response<Body> = next.run(req).await;
    let h = res.headers_mut();

    let (csp, frame) = if path.starts_with("/api/") || path.starts_with("/ws/") {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        (CSP_API, "DENY")
    } else if path.starts_with("/files/") {
        if is_active_file(&path) { (CSP_FILE_ACTIVE, "SAMEORIGIN") } else { ("default-src 'none'; img-src 'self'; media-src 'self'; style-src 'unsafe-inline'; frame-ancestors 'self'", "SAMEORIGIN") }
    } else if path.starts_with("/player") || path.starts_with("/shared/") {
        (CSP_PLAYER, "SAMEORIGIN")
    } else {
        (CSP_APP, "SAMEORIGIN")
    };
    // PDFs need the browser's built-in viewer, which a CSP on the document would block.
    let is_pdf = h.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(|c| c.starts_with("application/pdf")).unwrap_or(false);
    if !is_pdf {
        set(h, header::CONTENT_SECURITY_POLICY, csp);
    }
    set(h, header::X_FRAME_OPTIONS, frame);
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(h, header::REFERRER_POLICY, "strict-origin-when-cross-origin");
    set(h, HeaderName::from_static("permissions-policy"), PERMISSIONS);
    set(h, HeaderName::from_static("cross-origin-opener-policy"), "same-origin");
    set(h, HeaderName::from_static("cross-origin-resource-policy"), "same-origin");
    set(h, HeaderName::from_static("x-permitted-cross-domain-policies"), "none");
    if https {
        set(h, header::STRICT_TRANSPORT_SECURITY, "max-age=31536000; includeSubDomains");
    }
    res
}

/// RFC 9116 security.txt so researchers know how to report issues.
pub async fn security_txt(headers: HeaderMap) -> impl IntoResponse {
    let host = request_host(&headers).unwrap_or_else(|| "localhost".into());
    let contact = std::env::var("QIVO_SECURITY_CONTACT").ok().filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| format!("mailto:security@{}", host.split(':').next().unwrap_or("localhost").trim_start_matches("app.")));
    let expires = (chrono::Utc::now() + chrono::Duration::days(180)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let scheme = if crate::session::is_https(&headers) { "https" } else { "http" };
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        format!("Contact: {contact}\nExpires: {expires}\nPreferred-Languages: en, fr, ar\nCanonical: {scheme}://{host}/.well-known/security.txt\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn active_files() {
        assert!(is_active_file("/files/abc.SVG"));
        assert!(is_active_file("/files/abc.html"));
        assert!(!is_active_file("/files/abc.mp4"));
    }
}
