//! The parts of GitHub's REST answers the platform uses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};

/// The core request limit of the token the platform reads public data with.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct RateLimit {
    pub limit: i64,
    pub remaining: i64,
    pub reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Owner {
    /// The account's GitHub id; unlike the login it never changes.
    #[serde(default)]
    pub id: i64,
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Repo {
    pub id: i64,
    pub full_name: String,
    pub private: bool,
    pub owner: Owner,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Asset {
    pub id: i64,
    pub name: String,
    pub size: i64,
    pub browser_download_url: String,
    #[serde(default)]
    pub download_count: i64,
    /// Hex SHA-256 when GitHub sends `sha256:<hex>`.
    #[serde(default, deserialize_with = "sha256_digest")]
    pub digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Release {
    pub id: i64,
    pub tag_name: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Commit {
    pub sha: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Tag {
    pub name: String,
    pub commit: Commit,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
    #[serde(default)]
    pub avatar_url: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Installation {
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Permission {
    /// `admin`, `maintain`, `write`, `triage`, `read` or `none`.
    pub permission: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}

fn sha256_digest<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    let raw: Option<String> = Option::deserialize(d)?;
    Ok(raw.and_then(|r| {
        let hex = r.strip_prefix("sha256:")?;
        (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| hex.to_ascii_lowercase())
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(digest: serde_json::Value) -> Asset {
        serde_json::from_value(serde_json::json!({
            "id": 1, "name": "a.zip", "size": 3, "browser_download_url": "https://x", "digest": digest
        }))
        .unwrap()
    }

    #[test]
    fn asset_digest_parsed_or_none() {
        let hex = "AB".repeat(32);
        assert_eq!(asset(format!("sha256:{hex}").into()).digest, Some(hex.to_lowercase()));
        assert_eq!(asset(serde_json::Value::Null).digest, None);
        assert_eq!(asset("md5:abc".into()).digest, None);
        assert_eq!(asset("sha256:xyz".into()).digest, None);
        let no_field: Asset =
            serde_json::from_value(serde_json::json!({"id": 1, "name": "a", "size": 1, "browser_download_url": "u"})).unwrap();
        assert_eq!(no_field.digest, None);
    }
}
