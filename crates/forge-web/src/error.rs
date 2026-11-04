//! Errors of the website. A lost API session sends the visitor to the login, anything else
//! renders the error page.

use crate::api_client::ApiError;
use crate::session::expired_session_header;
use crate::templates::{ErrorPage, render};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

#[derive(Debug)]
pub enum WebError {
    Api(ApiError),
    NotFound,
    /// A page that needs a login, opened without one.
    LoginRequired,
    /// Logged in, but not allowed here.
    Forbidden,
}

impl From<ApiError> for WebError {
    fn from(e: ApiError) -> WebError {
        match e {
            ApiError::Api { status: 404, .. } => WebError::NotFound,
            other => WebError::Api(other),
        }
    }
}

pub fn error_page(status: StatusCode, title: &str, message: &str) -> Response {
    error_page_with(status, title, message, None)
}

/// The error page with `retry` (address, label) as its main action.
pub fn error_page_with(status: StatusCode, title: &str, message: &str, retry: Option<(&str, &str)>) -> Response {
    let page = ErrorPage {
        title: title.into(),
        message: message.into(),
        user: crate::app::seen_visitor(),
        csrf: String::new(),
        status: status.as_u16(),
        retry: retry.map(|(href, label)| (href.to_string(), label.to_string())),
    };
    (status, render(&page)).into_response()
}

/// "Try again" on the page asked for, when it was a page (a GET).
fn again() -> Option<String> {
    crate::app::this_page()
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        match self {
            WebError::Api(ApiError::Unauthorized) | WebError::LoginRequired => {
                let mut response = Redirect::to(&crate::app::login_href()).into_response();
                if let Ok(value) = HeaderValue::from_str(&expired_session_header()) {
                    response.headers_mut().insert(header::SET_COOKIE, value);
                }
                response
            }
            WebError::NotFound => error_page_with(StatusCode::NOT_FOUND, "Not found", "This page does not exist. It may have been renamed or removed.", Some(("/addons", "Browse addons"))),
            WebError::Forbidden => error_page(StatusCode::FORBIDDEN, "Admins only", "Your account has no rights for this page."),
            WebError::Api(e @ ApiError::Down(_)) => {
                let page = again();
                error_page_with(StatusCode::BAD_GATEWAY, "Unavailable", &e.message(), page.as_deref().map(|p| (p, "Try again")))
            }
            WebError::Api(ApiError::Api { status, message, .. }) if status >= 500 => {
                tracing::warn!(status, message, "the API failed");
                let page = again();
                let text = "The platform hit an error. Try again in a minute; if it keeps happening, report it on the Issues page.";
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                error_page_with(status, "Server error", text, page.as_deref().map(|p| (p, "Try again")))
            }
            WebError::Api(ApiError::Api { status, message, .. }) => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
                error_page(status, "Request not accepted", &message)
            }
        }
    }
}

pub type WebResult<T> = Result<T, WebError>;
