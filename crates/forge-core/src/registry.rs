//! Types of the addon registry (`registry/verified.json`, schema 1), shared by launcher and server.

use crate::trust::Trust;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;

/// Where a repo's updates come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SourceSpec {
    /// The asset matching `asset` (`*` wildcard) of the release marked "latest", or of the
    /// newest pre-release that has one when no release is marked "latest".
    Release { asset: String },
    /// The source zip of the newest tag.
    Tag,
    /// The source zip of the default branch's newest commit.
    Branch,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedAddon {
    pub id: String,
    pub name: String,
    pub author: String,
    pub repo: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: String,
    pub folders: Vec<String>,
    pub source: SourceSpec,
    /// Platform fields from here on. Launchers that read only schema 1 ignore them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<Trust>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_author: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<LatestVersion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<DependencyRef>,
    /// The square icon; its file name is the hash of the picture, so it never changes in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// Downloads of every version as GitHub counts them; none for addons from tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads: Option<i64>,
}

/// The version the launcher installs, as the platform scanned it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatestVersion {
    pub tag: String,
    pub asset_url: String,
    /// Hex SHA-256 of the scanned file; the launcher installs only a file with this hash.
    pub sha256: Option<String>,
    pub size: Option<i64>,
    pub trust: Trust,
    pub findings: FindingCounts,
    pub game_min: Option<String>,
    pub game_max: Option<String>,
    /// When the release was published on GitHub, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct FindingCounts {
    pub critical: u32,
    pub warn: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DependencyRef {
    /// The `id` of the addon depended on.
    pub id: String,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Registry {
    pub schema: u32,
    pub addons: Vec<VerifiedAddon>,
}

impl Registry {
    pub fn parse(text: &str) -> Result<Registry> {
        let registry: Registry = serde_json::from_str(text).map_err(|e| Error::Parse(e.to_string()))?;
        if registry.schema != SCHEMA {
            return Err(Error::Parse(format!("unknown schema version {}", registry.schema)));
        }
        let mut ids = std::collections::HashSet::new();
        let mut folders = std::collections::HashSet::new();
        for addon in &registry.addons {
            if !ids.insert(addon.id.to_lowercase()) {
                return Err(Error::Parse(format!("duplicate id {}", addon.id)));
            }
            if addon.folders.is_empty() {
                return Err(Error::Parse(format!("{} has no folders", addon.id)));
            }
            for folder in &addon.folders {
                if !folders.insert(folder.to_lowercase()) {
                    return Err(Error::Parse(format!("folder {folder} is used twice")));
                }
            }
        }
        Ok(registry)
    }

    pub fn get(&self, id: &str) -> Option<&VerifiedAddon> {
        self.addons.iter().find(|a| a.id == id)
    }

    pub fn by_repo(&self, repo: &str) -> Option<&VerifiedAddon> {
        self.addons.iter().find(|a| a.repo.eq_ignore_ascii_case(repo))
    }
}

/// Case-insensitive match where `*` stands for any run of characters.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let name = name.to_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name.ends_with(last) || name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(i) => rest = &rest[i + middle.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_schema() {
        assert!(Registry::parse(r#"{"schema": 2, "addons": []}"#).is_err());
        assert!(Registry::parse("not json").is_err());
    }

    #[test]
    fn rejects_duplicate_ids_and_folders() {
        let addon = |id: &str, folder: &str| {
            format!(r#"{{"id":"{id}","name":"n","author":"a","repo":"o/r","folders":["{folder}"],"source":{{"type":"tag"}}}}"#)
        };
        let json = |a: String, b: String| format!(r#"{{"schema":1,"addons":[{a},{b}]}}"#);
        assert!(Registry::parse(&json(addon("a", "A"), addon("a", "B"))).is_err());
        assert!(Registry::parse(&json(addon("a", "A"), addon("b", "a"))).is_err());
        assert!(Registry::parse(&json(addon("a", "A"), addon("b", "B"))).is_ok());
    }

    #[test]
    fn asset_glob_matching() {
        assert!(glob_match("SurvivorUI-v*.zip", "SurvivorUI-v3.0.36.zip"));
        assert!(!glob_match("SurvivorUI-v*.zip", "SurvivorUI-Installer.zip"));
        assert!(glob_match("AetherHUD.zip", "aetherhud.zip"));
        assert!(!glob_match("AetherHUD.zip", "AetherHUD2.zip"));
        assert!(glob_match("a*b*c", "a-x-b-y-c"));
        assert!(!glob_match("ab*ba", "aba"));
    }
}
