//! Notices that come back in the address after a form (`?done=`, `?error=`, `?msg=`). The
//! redirect that sets one signs it (`sig`); an address with a notice nobody signed, such as a link
//! someone made up, opens without the notice, so the site never shows text a stranger wrote.

use crate::app::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use sha2::{Digest, Sha256};

const NOTICES: &[&str] = &["done", "error", "msg"];
const SIG: &str = "sig";
/// GitHub sends its own `error` back to the login callback.
const FOREIGN: &[&str] = &["/auth/callback"];

fn tag(key: &[u8], notices: &[(String, String)]) -> String {
    let mut hash = Sha256::new().chain_update(key);
    for (name, value) in notices {
        hash.update(format!("{name}={value}\n"));
    }
    hash.finalize()[..12].iter().map(|b| format!("{b:02x}")).collect()
}

/// The notices of an address (name, text) and its `sig`.
type Notices = (Vec<(String, String)>, Option<String>);

/// The notices of `url` and its `sig`, or `None` without notices.
fn notices(url: &reqwest::Url) -> Option<Notices> {
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    let found: Vec<(String, String)> = pairs.iter().filter(|(k, _)| NOTICES.contains(&k.as_str())).cloned().collect();
    (!found.is_empty()).then(|| (found, pairs.into_iter().find(|(k, _)| k == SIG).map(|(_, v)| v)))
}

/// `url` with its notices and `sig` taken out, as a local address.
fn without_notices(url: &reqwest::Url) -> String {
    let kept: Vec<(String, String)> = url.query_pairs().into_owned().filter(|(k, _)| !NOTICES.contains(&k.as_str()) && k != SIG).collect();
    let mut clean = url.clone();
    clean.set_query(None);
    if !kept.is_empty() {
        clean.query_pairs_mut().extend_pairs(kept);
    }
    local(&clean)
}

fn local(url: &reqwest::Url) -> String {
    let mut out = url.path().to_string();
    if let Some(q) = url.query() {
        out.push('?');
        out.push_str(q);
    }
    if let Some(f) = url.fragment() {
        out.push('#');
        out.push_str(f);
    }
    out
}

fn parse(local: &str) -> Option<reqwest::Url> {
    (local.starts_with('/') && !local.starts_with("//")).then(|| reqwest::Url::parse(&format!("http://site{local}")).ok()).flatten()
}

/// `target` with its notices signed by `state`, as a redirect of this site sends it.
pub fn sign(state: &AppState, target: &str) -> String {
    signed(state.key.signing(), target).unwrap_or_else(|| target.to_string())
}

/// Signs the notices of a local redirect target.
fn signed(key: &[u8], target: &str) -> Option<String> {
    let mut url = parse(target)?;
    let (found, sig) = notices(&url)?;
    if sig.is_some() {
        return None;
    }
    url.query_pairs_mut().append_pair(SIG, &tag(key, &found));
    Some(local(&url))
}

pub async fn middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let key = state.key.signing().to_vec();
    let path = request.uri().path().to_string();
    if request.method() == Method::GET
        && !FOREIGN.contains(&path.as_str())
        && let Some(url) = request.uri().path_and_query().and_then(|p| parse(p.as_str()))
        && let Some((found, sig)) = notices(&url)
        && sig.as_deref() != Some(tag(&key, &found).as_str())
    {
        return Redirect::to(&without_notices(&url)).into_response();
    }
    let mut response = next.run(request).await;
    let redirect = response.status().is_redirection() || response.status() == StatusCode::OK;
    for name in [header::LOCATION, HeaderName::from_static("hx-redirect")] {
        let target = response.headers().get(&name).and_then(|v| v.to_str().ok()).filter(|_| redirect).and_then(|t| signed(&key, t));
        if let Some(value) = target.and_then(|t| HeaderValue::from_str(&t).ok()) {
            response.headers_mut().insert(name, value);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_signed_notices_count() {
        let key = b"k";
        let target = signed(key, "/addons/x/comments?done=Posted.#comment-4").unwrap();
        assert!(target.starts_with("/addons/x/comments?done=Posted.&sig=") && target.ends_with("#comment-4"), "{target}");
        let url = parse(&target).unwrap();
        let (found, sig) = notices(&url).unwrap();
        assert_eq!(sig.unwrap(), tag(key, &found));
        assert_ne!(tag(key, &[("done".into(), "Other".into())]), tag(key, &found), "another text needs another sig");
        assert_eq!(without_notices(&parse("/packs/a?msg=x&sig=y&tab=2").unwrap()), "/packs/a?tab=2");
        assert_eq!(signed(key, "https://github.com/x?error=y"), None, "only local targets");
        assert_eq!(signed(key, "/packs/a?tab=2"), None, "nothing to sign");
    }
}
