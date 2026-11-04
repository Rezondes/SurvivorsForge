//! CSRF protection: every visitor gets a random token in an encrypted cookie. Requests that change
//! something must send it back, as the `X-CSRF-Token` header (htmx does that for every request) or
//! as the form field `csrf`, and must come from our own origin.

use crate::app::AppState;
use crate::session::{CSRF_COOKIE, cookie};
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::PrivateCookieJar;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The visitor's CSRF token, for templates.
#[derive(Clone, Debug)]
pub struct CsrfToken(pub String);

const MAX_FORM: usize = 1024 * 1024;

fn origin_ok(request: &Request, web_url: &str) -> bool {
    let headers = request.headers();
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        return origin == web_url;
    }
    headers
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|r| r == web_url || r.starts_with(&format!("{web_url}/")))
}

/// The site's page, not bare text; htmx reloads the page for a fresh form instead.
fn forbidden() -> Response {
    let message = "The form was open too long or came from another site. Go back, reload the page and send it again.";
    let mut response = crate::error::error_page(StatusCode::FORBIDDEN, "Form expired", message);
    response.headers_mut().insert("hx-refresh", axum::http::HeaderValue::from_static("true"));
    response
}

fn too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE, "The form is too large. Send fewer or smaller files at once.").into_response()
}

/// The new addon form with an icon and ten screenshots of at most 5 MB each.
pub const MAX_UPLOAD_FORM: usize = 60 * 1024 * 1024;

/// The text of field `name` in a multipart body; the browser sends fields in page order, and the
/// token comes first. Read with multer itself: axum's `Multipart` would stop at its default 2 MB,
/// as this body is not the route's, which allows `MAX_UPLOAD_FORM`.
async fn multipart_field(content_type: Option<header::HeaderValue>, body: axum::body::Bytes, name: &str) -> Option<String> {
    let boundary = multer::parse_boundary(content_type?.to_str().ok()?).ok()?;
    let mut multipart = multer::Multipart::new(Body::from(body).into_data_stream(), boundary);
    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() == Some(name) {
            return field.text().await.ok();
        }
    }
    None
}

fn form_field(body: &[u8], name: &str) -> Option<String> {
    let url = reqwest::Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(body))).ok()?;
    url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
}

pub async fn middleware(State(state): State<AppState>, jar: PrivateCookieJar, mut request: Request, next: Next) -> Response {
    let (jar, token) = match jar.get(CSRF_COOKIE) {
        Some(c) => (jar.clone(), c.value().to_string()),
        None => {
            let token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
            (jar.add(cookie(CSRF_COOKIE, token.clone(), state.secure_cookies())), token)
        }
    };
    let safe = matches!(*request.method(), Method::GET | Method::HEAD | Method::OPTIONS);
    if !safe {
        if !origin_ok(&request, &state.config.public_web_url) {
            return forbidden();
        }
        let header_token = request.headers().get("x-csrf-token").and_then(|v| v.to_str().ok()).map(str::to_string);
        let sent = match header_token {
            Some(t) => Some(t),
            None if request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded")) =>
            {
                let (parts, body) = request.into_parts();
                let Ok(bytes) = to_bytes(body, MAX_FORM).await else { return too_large() };
                let field = form_field(&bytes, "csrf");
                request = Request::from_parts(parts, Body::from(bytes));
                field
            }
            // Forms with file inputs (publishing with images) send the token as a multipart field.
            None if request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|c| c.starts_with("multipart/form-data")) =>
            {
                let (parts, body) = request.into_parts();
                let Ok(bytes) = to_bytes(body, MAX_UPLOAD_FORM).await else { return too_large() };
                let field = multipart_field(parts.headers.get(header::CONTENT_TYPE).cloned(), bytes.clone(), "csrf").await;
                request = Request::from_parts(parts, Body::from(bytes));
                field
            }
            None => None,
        };
        if sent.as_deref() != Some(token.as_str()) {
            return forbidden();
        }
    }
    request.extensions_mut().insert(CsrfToken(token));
    (jar, next.run(request).await).into_response()
}
