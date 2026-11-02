//! What the publish form can fill in from a repo: name, address, short description, where the
//! downloads come from and the addon folders, read from the newest release or tag.

use crate::addons::{MAX_NAME_LEN, MAX_SUMMARY_LEN};
use crate::github::types::{Asset, Release, Repo, Tag};
use forge_core::archive::analyze_reader;
use serde::Serialize;
use std::io::Cursor;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    pub name: String,
    pub slug: String,
    pub summary: String,
    /// `release` or `tag`.
    pub source_type: String,
    pub asset_pattern: String,
    pub folders: Vec<String>,
    /// What the suggestion was read from, e.g. `release v0.7.0, LuckyOdds-v0.7.0.zip`.
    pub read_from: String,
}

/// The address on the site: lowercase letters, digits and single dashes, as `validate_slug` wants.
pub fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(49).collect();
    let slug = slug.trim_end_matches('-').to_string();
    if slug.len() < 2 { format!("{slug}-addon") } else { slug }
}

/// The release zip name with the version as `*`: `LuckyOdds-v0.7.0.zip` of tag `v0.7.0` gives
/// `LuckyOdds-v*.zip`. A name without the version stays as it is.
pub fn asset_pattern(asset: &str, tag: &str) -> String {
    let version = tag.trim_start_matches(['v', 'V']);
    for needle in [version, tag] {
        if !needle.is_empty() && asset.contains(needle) {
            return asset.replacen(needle, "*", 1);
        }
    }
    asset.to_string()
}

/// The newest stable release with a zip, else the newest pre-release with one. `releases` is
/// newest first, as GitHub sends it.
pub fn pick_release(releases: &[Release]) -> Option<(&Release, &Asset)> {
    let with_zip = |r: &&Release| !r.draft && r.assets.iter().any(|a| a.name.to_ascii_lowercase().ends_with(".zip"));
    let release = releases.iter().filter(|r| !r.prerelease).find(with_zip).or_else(|| releases.iter().find(with_zip))?;
    let zips: Vec<&Asset> = release.assets.iter().filter(|a| a.name.to_ascii_lowercase().ends_with(".zip")).collect();
    // A repo that ships its source as an extra zip: take the other one.
    let asset = zips.iter().find(|a| !a.name.to_ascii_lowercase().contains("source")).or(zips.first())?;
    Some((release, asset))
}

/// The tag with the highest version (`v4.10` above `v4.9`); GitHub does not sort them so.
pub fn newest_tag(tags: &[Tag]) -> Option<&Tag> {
    tags.iter().max_by_key(|t| forge_core::github_web::version_key(&t.name))
}

/// Fills in from the repo and, if there is one, the zip players would get.
pub fn suggest(repo: &Repo, source_type: &str, pattern: &str, zip: Option<&[u8]>, read_from: String) -> Suggestion {
    let repo_name = repo.full_name.rsplit('/').next().unwrap_or(&repo.full_name);
    let addons = zip.and_then(|z| analyze_reader(Cursor::new(z)).ok()).unwrap_or_default();
    // The main addon: the one named like the repo, else the first.
    let main = addons.iter().find(|a| slug_of(&a.name) == slug_of(repo_name)).or(addons.first());
    let title = main.map(|a| a.toc.title.trim().to_string()).filter(|t| !t.is_empty());
    let notes = main.and_then(|a| a.toc.notes.clone());
    let summary = repo.description.clone().filter(|d| !d.trim().is_empty()).or(notes).unwrap_or_default();
    Suggestion {
        name: title.unwrap_or_else(|| repo_name.to_string()).chars().take(MAX_NAME_LEN).collect(),
        slug: slug_of(repo_name),
        summary: summary.trim().chars().take(MAX_SUMMARY_LEN).collect(),
        source_type: source_type.into(),
        asset_pattern: pattern.into(),
        folders: addons.iter().map(|a| a.name.clone()).collect(),
        read_from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::types::Owner;
    use std::io::Write;

    fn asset(name: &str) -> Asset {
        Asset { id: 1, name: name.into(), size: 1, browser_download_url: format!("https://github.com/{name}"), download_count: 0, digest: None }
    }

    fn release(tag: &str, prerelease: bool, assets: &[&str]) -> Release {
        Release { id: 1, tag_name: tag.into(), name: None, draft: false, prerelease, published_at: None, assets: assets.iter().map(|a| asset(a)).collect() }
    }

    #[test]
    fn newest_tag_by_version() {
        let tag = |name: &str| Tag { name: name.into(), commit: crate::github::types::Commit { sha: "a".into() } };
        let tags = [tag("v4.9"), tag("v4.10"), tag("v4.10-beta.1")];
        assert_eq!(newest_tag(&tags).unwrap().name, "v4.10");
        assert!(newest_tag(&[]).is_none());
    }

    #[test]
    fn slugs_from_repo_names() {
        assert_eq!(slug_of("LuckyOdds"), "luckyodds");
        assert_eq!(slug_of("KeijinUI_modern-as-ui"), "keijinui-modern-as-ui");
        assert_eq!(slug_of("--Azeroth  Field Kit!"), "azeroth-field-kit");
        assert_eq!(slug_of("x"), "x-addon");
        assert_eq!(slug_of(&"a".repeat(80)).len(), 49);
    }

    #[test]
    fn version_becomes_a_star() {
        assert_eq!(asset_pattern("LuckyOdds-v0.7.0.zip", "v0.7.0"), "LuckyOdds-v*.zip");
        assert_eq!(asset_pattern("SurvivorUI-3.0.42.zip", "v3.0.42"), "SurvivorUI-*.zip");
        assert_eq!(asset_pattern("Kit-build-7.zip", "build-7"), "Kit-*.zip");
        assert_eq!(asset_pattern("Addon.zip", "v1.0"), "Addon.zip", "same name every release");
    }

    #[test]
    fn stable_release_with_a_zip_first() {
        let releases = [
            release("v3", true, &["A-v3.zip"]),
            release("v2", false, &["notes.txt"]),
            release("v1", false, &["A-v1-source.zip", "A-v1.zip"]),
        ];
        let (r, a) = pick_release(&releases).unwrap();
        assert_eq!((r.tag_name.as_str(), a.name.as_str()), ("v1", "A-v1.zip"));
        let only_pre = [release("v3", true, &["A-v3.zip"])];
        assert_eq!(pick_release(&only_pre).unwrap().0.tag_name, "v3");
        assert!(pick_release(&[release("v1", false, &[])]).is_none());
    }

    #[test]
    fn fills_in_from_the_zip_and_the_repo() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("LuckyOdds/LuckyOdds.toc", options).unwrap();
        zip.write_all(b"## Title: |cffffd700Lucky|r Odds\n## Notes: Odds for rare cards\nLuckyOdds.lua\n").unwrap();
        zip.start_file("LuckyLib/LuckyLib.toc", options).unwrap();
        zip.write_all(b"## Title: Lib\n").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        let repo = Repo { id: 1, full_name: "cadoky/LuckyOdds".into(), private: false, owner: Owner { id: 1, login: "cadoky".into() }, description: None, has_issues: true };

        let s = suggest(&repo, "release", "LuckyOdds-v*.zip", Some(&bytes), "release v0.7.0".into());
        assert_eq!((s.name.as_str(), s.slug.as_str(), s.summary.as_str()), ("Lucky Odds", "luckyodds", "Odds for rare cards"));
        let mut folders = s.folders.clone();
        folders.sort();
        assert_eq!(folders, ["LuckyLib", "LuckyOdds"]);

        let described = Repo { description: Some("From GitHub".into()), ..repo.clone() };
        assert_eq!(suggest(&described, "tag", "", Some(&bytes), String::new()).summary, "From GitHub", "the repo description wins");
        let empty = suggest(&repo, "tag", "", None, String::new());
        assert_eq!((empty.name.as_str(), empty.folders.len()), ("LuckyOdds", 0), "without a zip: the repo name");
    }
}
