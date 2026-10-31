//! `GET /v1/addons`: full-text search with prefixes, typo tolerance on the name, filters, sorting
//! and paging. Blocked and deleted addons never appear.

use super::game_versions::game_version_id;
use crate::app::AppState;
use crate::error::{ApiError, ApiResult};
use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MAX_PER_PAGE: i64 = 50;
const DEFAULT_PER_PAGE: i64 = 20;

#[derive(Default, Debug)]
pub struct SearchQuery {
    pub q: Option<String>,
    pub category: Option<String>,
    pub trust: Vec<String>,
    pub game: Option<String>,
    /// GitHub login of the owner, or the author name of an unclaimed addon; any case.
    pub author: Option<String>,
    pub sort: Option<String>,
    pub page: i64,
    pub per_page: i64,
}

/// Parses the query string by hand, because `trust` may repeat.
pub fn parse_query(raw: Option<&str>) -> SearchQuery {
    let mut query = SearchQuery { page: 1, per_page: DEFAULT_PER_PAGE, ..Default::default() };
    let pairs = reqwest::Url::parse(&format!("http://x/?{}", raw.unwrap_or_default())).map(|u| u.query_pairs().into_owned().collect::<Vec<_>>());
    for (key, value) in pairs.unwrap_or_default() {
        let value = value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "q" => query.q = Some(value),
            "category" => query.category = Some(value),
            "trust" => query.trust.extend(value.split(',').map(|t| t.trim().to_string())),
            "game" => query.game = Some(value),
            "author" => query.author = Some(value),
            "sort" => query.sort = Some(value),
            "page" => query.page = value.parse().unwrap_or(1).max(1),
            "per_page" => query.per_page = value.parse().unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE),
            _ => {}
        }
    }
    query
}

/// `lucky odds` becomes `lucky:* & odds:*`, so prefixes match; anything but letters and digits is dropped.
pub fn prefix_tsquery(q: &str) -> Option<String> {
    let words: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(8)
        .map(|w| format!("{}:*", w.to_lowercase()))
        .collect();
    (!words.is_empty()).then(|| words.join(" & "))
}

pub async fn list(State(state): State<AppState>, headers: HeaderMap, RawQuery(raw): RawQuery) -> ApiResult<Response> {
    let query = parse_query(raw.as_deref());
    let game = match &query.game {
        Some(name) => Some(game_version_id(&state.pool, name).await?),
        None => None,
    };
    let order = match query.sort.as_deref() {
        Some("downloads") | None if query.q.is_none() => "downloads DESC NULLS LAST, a.id DESC",
        None | Some("relevance") => "rank DESC, downloads DESC NULLS LAST, a.id DESC",
        Some("downloads") => "downloads DESC NULLS LAST, a.id DESC",
        Some("updated") => "updated_at DESC, a.id DESC",
        Some("name") => "lower(a.name), a.id",
        Some(_) => return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_sort", "sort is relevance, downloads, updated or name")),
    };
    let tsquery = query.q.as_deref().and_then(prefix_tsquery);
    let sql = format!(
        "WITH found AS (
             SELECT a.id, a.slug, a.name, a.summary, a.category, a.owner_id,
                    t.trust,
                    -- GitHub counts downloads of release files only; addons from tags have no count.
                    CASE WHEN a.source_type = 'tag' THEN NULL
                         ELSE (SELECT coalesce(sum(download_count), 0) FROM versions WHERE addon_id = a.id)::bigint END AS downloads,
                    greatest(a.updated_at, (SELECT max(published_at) FROM versions WHERE addon_id = a.id)) AS updated_at,
                    CASE WHEN $1::text IS NULL THEN 0
                         ELSE coalesce(ts_rank_cd(a.search, to_tsquery('simple', $2)), 0) * 10 + word_similarity($1, a.name) END AS rank
             FROM addons a
             LEFT JOIN version_trust t ON t.version_id = relevant_version(a.id)
             -- Blocked addons and addons whose version for players is blocked stay off the list.
             WHERE a.deleted_at IS NULL AND a.blocked_at IS NULL AND t.trust IS DISTINCT FROM 'blocked'
               AND ($1::text IS NULL OR ($2::text IS NOT NULL AND a.search @@ to_tsquery('simple', $2)) OR word_similarity($1, a.name) >= 0.3)
               AND ($3::text IS NULL OR a.category IN (SELECT name FROM categories WHERE name = $3::citext OR parent = $3::citext))
               AND (cardinality($4::text[]) = 0 OR t.trust = ANY($4))
               AND ($5::int IS NULL OR game_compatible(relevant_version(a.id), $5) IS NOT FALSE)
               AND ($9::text IS NULL OR lower(coalesce((SELECT login::text FROM users WHERE id = a.owner_id), a.author_name)) = lower($9))
         )
         SELECT json_build_object(
             'total', (SELECT count(*) FROM found),
             'page', $6::bigint, 'per_page', $7::bigint,
             'addons', coalesce((SELECT json_agg(item) FROM (
                 SELECT json_build_object(
                     'slug', a.slug, 'name', a.name, 'summary', a.summary, 'category', a.category, 'trust', a.trust,
                     'icon_url', (SELECT $8 || '/' || i.file FROM images i WHERE i.addon_id = a.id AND i.kind = 'icon'),
                     'owner', CASE WHEN u.id IS NULL THEN NULL ELSE json_build_object('login', u.login, 'avatar_url', u.avatar_url) END,
                     'author', coalesce(u.login::text, (SELECT author_name FROM addons WHERE id = a.id)),
                     'verified_author', EXISTS (SELECT 1 FROM verifications w JOIN versions wv ON wv.id = w.version_id WHERE wv.addon_id = a.id),
                     'downloads', a.downloads, 'updated_at', a.updated_at) AS item
                 FROM found a LEFT JOIN users u ON u.id = a.owner_id
                 ORDER BY {order}
                 LIMIT $7 OFFSET ($6 - 1) * $7) page), '[]'))"
    );
    let body: Value = sqlx::query_scalar(&sql)
        .bind(&query.q)
        .bind(&tsquery)
        .bind(&query.category)
        .bind(&query.trust)
        .bind(game)
        .bind(query.page)
        .bind(query.per_page)
        .bind(&state.config.public_image_url)
        .bind(&query.author)
        .fetch_one(&state.pool)
        .await?;
    let body = body.to_string();
    let etag = format!("\"{}\"", &hex::encode(Sha256::digest(body.as_bytes()))[..32]);
    let etag_value = HeaderValue::from_str(&etag).map_err(|_| ApiError::internal())?;
    if headers.get(header::IF_NONE_MATCH).is_some_and(|v| crate::cache::etag_matches(v, etag.as_bytes())) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag_value)]).into_response());
    }
    Ok((
        [(header::ETAG, etag_value), (header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parsing() {
        let q = parse_query(Some("q=lucky+odds&trust=verified&trust=scanned,flagged&per_page=500&page=0&x=1"));
        assert_eq!(q.q.as_deref(), Some("lucky odds"));
        assert_eq!(q.trust, ["verified", "scanned", "flagged"]);
        assert_eq!((q.page, q.per_page), (1, MAX_PER_PAGE));
        assert_eq!(parse_query(None).per_page, 20);
    }

    #[test]
    fn prefixes_and_sanitizing() {
        assert_eq!(prefix_tsquery("Lucky Odds").as_deref(), Some("lucky:* & odds:*"));
        assert_eq!(prefix_tsquery("a'; DROP|&!").as_deref(), Some("a:* & drop:*"));
        assert_eq!(prefix_tsquery("!!!"), None);
    }
}
