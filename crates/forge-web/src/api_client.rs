//! The website's client of forge-api. The website knows nothing but this public API.

use reqwest::{Method, StatusCode};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// The token is missing, expired or revoked.
    Unauthorized,
    /// The API answered with an error of its own.
    Api { status: u16, code: String, message: String },
    /// The API could not be reached.
    Down(String),
}

impl ApiError {
    pub fn code(&self) -> &str {
        match self {
            ApiError::Unauthorized => "unauthorized",
            ApiError::Api { code, .. } => code,
            ApiError::Down(_) => "api_down",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ApiError::Unauthorized => "Please log in again.".into(),
            ApiError::Api { message, .. } => message.clone(),
            ApiError::Down(_) => "The platform is not reachable right now. Try again in a minute.".into(),
        }
    }
}

#[derive(Clone)]
pub struct ApiClient {
    http: reqwest::Client,
    /// Same settings, but hands redirects back instead of following them.
    no_redirect: reqwest::Client,
    base: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Names the visitor for the API's rate limit; outside a request (healthcheck) nothing is added.
fn forwarded(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    // The page's trace goes on in the API.
    let mut trace = reqwest::header::HeaderMap::new();
    forge_trace::inject(&mut trace);
    let request = request.headers(trace);
    match crate::rate_limit::CLIENT_IP.try_with(|ip| *ip) {
        Ok(ip) => request.header("x-forwarded-for", ip.to_string()),
        Err(_) => request,
    }
}

impl ApiClient {
    pub fn new(base: &str) -> ApiClient {
        let builder = || reqwest::Client::builder().timeout(std::time::Duration::from_secs(30));
        ApiClient {
            http: builder().build().expect("HTTP client"),
            no_redirect: builder().redirect(reqwest::redirect::Policy::none()).build().expect("HTTP client"),
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub async fn healthy(&self) -> bool {
        matches!(self.http.get(format!("{}/healthz", self.base)).send().await, Ok(r) if r.status().is_success())
    }

    /// Calls the API; a `2xx` answer comes back as JSON (`Null` for an empty body).
    pub async fn call(&self, method: Method, path: &str, token: Option<&str>, body: Option<&Value>) -> ApiResult<Value> {
        let mut request = forwarded(self.http.request(method, format!("{}{path}", self.base)));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| ApiError::Down(e.to_string()))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| ApiError::Down(e.to_string()))?;
        if status == StatusCode::UNAUTHORIZED {
            return Err(ApiError::Unauthorized);
        }
        let json: Value = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        if !status.is_success() {
            return Err(ApiError::Api {
                status: status.as_u16(),
                code: json["error"].as_str().unwrap_or("error").to_string(),
                message: json["message"].as_str().unwrap_or("Something went wrong").to_string(),
            });
        }
        Ok(json)
    }

    /// A GET whose answer may be a redirect: `Err(location)` for one, else the JSON body.
    pub async fn get_or_redirect(&self, path: &str) -> ApiResult<Result<Value, String>> {
        let response = forwarded(self.no_redirect.get(format!("{}{path}", self.base))).send().await.map_err(|e| ApiError::Down(e.to_string()))?;
        if response.status().is_redirection() {
            let location = response.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("/");
            return Ok(Err(location.to_string()));
        }
        let status = response.status();
        let json: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(ApiError::Api {
                status: status.as_u16(),
                code: json["error"].as_str().unwrap_or("error").to_string(),
                message: json["message"].as_str().unwrap_or("Something went wrong").to_string(),
            });
        }
        Ok(Ok(json))
    }

    pub async fn get(&self, path: &str, token: Option<&str>) -> ApiResult<Value> {
        self.call(Method::GET, path, token, None).await
    }

    pub async fn post(&self, path: &str, token: Option<&str>, body: &Value) -> ApiResult<Value> {
        self.call(Method::POST, path, token, Some(body)).await
    }

    /// Sends a multipart form, for image uploads.
    pub async fn post_multipart(&self, path: &str, token: &str, form: reqwest::multipart::Form) -> ApiResult<Value> {
        let response = forwarded(self.http.post(format!("{}{path}", self.base)))
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| ApiError::Down(e.to_string()))?;
        let status = response.status();
        let json: Value = response.json().await.unwrap_or(Value::Null);
        match status {
            StatusCode::UNAUTHORIZED => Err(ApiError::Unauthorized),
            s if s.is_success() => Ok(json),
            s => Err(ApiError::Api {
                status: s.as_u16(),
                code: json["error"].as_str().unwrap_or("error").to_string(),
                message: json["message"].as_str().unwrap_or("Something went wrong").to_string(),
            }),
        }
    }
}
