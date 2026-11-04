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
    (status, render(&ErrorPage { title: title.into(), message: message.into(), user: None, csrf: String::new() })).into_response()
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
            WebError::NotFound => error_page(StatusCode::NOT_FOUND, "Not found", "This page does not exist."),
            WebError::Forbidden => error_page(StatusCode::FORBIDDEN, "Not allowed", "This page is for admins."),
            WebError::Api(e @ ApiError::Down(_)) => error_page(StatusCode::BAD_GATEWAY, "Unavailable", &e.message()),
            WebError::Api(ApiError::Api { status, message, .. }) => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
                error_page(status, "Something went wrong", &message)
            }
        }
    }
}

pub type WebResult<T> = Result<T, WebError>;
