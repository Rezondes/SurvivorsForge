//! Decides per managed addon whether a newer version is available.
//!
//! Tags are not uniform (`v3.0.36`, `v2.6.5-azs`, `build-<sha>`), so a remote id that differs
//! from the installed one counts as an update. No semver comparison.

use crate::github::{source_zip_url, GitHub, Release};
use crate::registry::{glob_match, Registry, SourceSpec, VerifiedAddon};
use crate::state::{Source, State, TOC_PREFIX};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteVersion {
    /// Tag or commit SHA; what gets stored as the installed version.
    pub id: String,
    /// Short form for display.
    pub label: String,
    pub download: String,
    /// Hex SHA-256 the platform scanned, for addons that come from the platform.
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateStatus {
    UpToDate,
    UpdateAvailable(RemoteVersion),
    /// Installed from a zip: no remote to check.
    Manual,
    CheckFailed(String),
}

pub fn resolve_remote(gh: &dyn GitHub, repo: &str, spec: &SourceSpec) -> Result<RemoteVersion> {
    match spec {
        SourceSpec::Release { asset } => {
            let release = match gh.latest_release(repo)? {
                Some(release) => release,
                None => newest_pre_release(gh, repo, asset)?,
            };
            let asset = release.assets.iter().find(|a| glob_match(asset, &a.name)).ok_or(Error::NoMatchingAsset)?;
            Ok(RemoteVersion { id: release.tag.clone(), label: release.tag.clone(), download: asset.url.clone(), sha256: None })
        }
        SourceSpec::Tag => {
            let tag = gh.tags(repo)?.into_iter().next().ok_or(Error::NoTags)?;
            Ok(RemoteVersion { sha256: None, download: source_zip_url(repo, &tag.name), label: tag.name.clone(), id: tag.name })
        }
        SourceSpec::Branch => {
            let branch = gh.default_branch(repo)?;
            let sha = gh.commit_sha(repo, &branch)?;
            Ok(RemoteVersion { sha256: None, download: source_zip_url(repo, &sha), label: sha.chars().take(7).collect(), id: sha })
        }
    }
}

/// Without a "latest" release only pre-releases exist: the newest tag whose release has a
/// matching asset. Tags without one (e.g. `upstream/...` snapshots) are skipped.
fn newest_pre_release(gh: &dyn GitHub, repo: &str, asset: &str) -> Result<Release> {
    for tag in gh.tags(repo)? {
        let release = gh.release(repo, &tag.name)?;
        if release.assets.iter().any(|a| glob_match(asset, &a.name)) {
            return Ok(release);
        }
    }
    Err(Error::NoRelease)
}

fn normalize(version: &str) -> String {
    version.trim().trim_start_matches(['v', 'V']).to_lowercase()
}

/// Compares the installed version with the remote one. For adopted addons (`toc:<version>`)
/// a matching version also returns the remote id to store from now on.
pub fn check(installed: &str, remote: RemoteVersion) -> (UpdateStatus, Option<String>) {
    if let Some(toc_version) = installed.strip_prefix(TOC_PREFIX) {
        if !toc_version.is_empty() && normalize(toc_version) == normalize(&remote.id) {
            return (UpdateStatus::UpToDate, Some(remote.id));
        }
        return (UpdateStatus::UpdateAvailable(remote), None);
    }
    if installed == remote.id {
        (UpdateStatus::UpToDate, None)
    } else {
        (UpdateStatus::UpdateAvailable(remote), None)
    }
}

/// The newest version as the platform scanned it, so no GitHub request is needed. `None` when the
/// registry has no platform data for the addon (the plain verified list).
pub fn platform_remote(addon: &VerifiedAddon) -> Option<Result<RemoteVersion>> {
    let latest = addon.latest.as_ref()?;
    if !latest.trust.installable() {
        let why = match latest.trust {
            forge_core::trust::Trust::Blocked => format!("{} {} is blocked by an admin", addon.name, latest.tag),
            _ => format!("{} {} changed on GitHub after it was checked", addon.name, latest.tag),
        };
        return Some(Err(Error::NotInstallable(why)));
    }
    Some(Ok(RemoteVersion {
        id: latest.tag.clone(),
        label: latest.tag.clone(),
        download: latest.asset_url.clone(),
        sha256: latest.sha256.clone(),
    }))
}

/// The repo and source spec of an entry; `None` for zip installs.
pub fn remote_of(source: &Source, registry: &Registry) -> Option<Result<(String, SourceSpec)>> {
    match source {
        Source::Zip { .. } => None,
        Source::GitHub { repo, spec } => Some(Ok((repo.clone(), spec.clone()))),
        Source::Verified { registry_id } => Some(
            registry
                .get(registry_id)
                .map(|a| (a.repo.clone(), a.source.clone()))
                .ok_or_else(|| Error::Parse(format!("{registry_id} is no longer in the verified list"))),
        ),
    }
}

/// Without the platform: the newest version on GitHub. When it is the version the platform scanned
/// last (the stored list's), that scan still holds; a newer one comes without a SHA-256, unchecked.
pub fn direct_remote(gh: &dyn GitHub, addon: &VerifiedAddon) -> Result<RemoteVersion> {
    let mut remote = resolve_remote(gh, &addon.repo, &addon.source)?;
    if addon.latest.as_ref().is_some_and(|l| l.tag == remote.id)
        && let Some(scanned) = platform_remote(addon)
    {
        // Blocked or changed after the scan: that stays so offline too.
        remote.sha256 = scanned?.sha256;
    }
    Ok(remote)
}

/// Checks every entry. One failing repo does not stop the others. Adopted versions that turn
/// out to be current are rewritten to the remote id in `state`.
pub fn check_all(gh: &dyn GitHub, state: &mut State, registry: &Registry) -> Vec<(String, UpdateStatus)> {
    check_entries(gh, state, registry, false)
}

/// `check_all` when the platform cannot be reached: verified addons too are asked on GitHub
/// (`direct_remote`), as the stored list's versions may be old.
pub fn check_all_direct(gh: &dyn GitHub, state: &mut State, registry: &Registry) -> Vec<(String, UpdateStatus)> {
    check_entries(gh, state, registry, true)
}

fn check_entries(gh: &dyn GitHub, state: &mut State, registry: &Registry, direct: bool) -> Vec<(String, UpdateStatus)> {
    let mut results = Vec::new();
    for entry in &mut state.entries {
        let platform = match &entry.source {
            Source::Verified { registry_id } if direct => registry.get(registry_id).map(|a| direct_remote(gh, a)),
            Source::Verified { registry_id } => registry.get(registry_id).and_then(platform_remote),
            _ => None,
        };
        if let Some(remote) = platform {
            let status = match remote {
                Err(e) => UpdateStatus::CheckFailed(e.to_string()),
                Ok(remote) => {
                    let (status, adopted) = check(&entry.version, remote);
                    if let Some(version) = adopted {
                        entry.version = version;
                    }
                    status
                }
            };
            results.push((entry.id.clone(), status));
            continue;
        }
        let status = match remote_of(&entry.source, registry) {
            None => UpdateStatus::Manual,
            Some(Err(e)) => UpdateStatus::CheckFailed(e.to_string()),
            Some(Ok((repo, spec))) => match resolve_remote(gh, &repo, &spec) {
                Err(e) => UpdateStatus::CheckFailed(e.to_string()),
                Ok(remote) => {
                    let (status, adopted) = check(&entry.version, remote);
                    if let Some(version) = adopted {
                        entry.version = version;
                    }
                    status
                }
            },
        };
        results.push((entry.id.clone(), status));
    }
    results
}

#[cfg(test)]
pub(crate) mod fake {
    use crate::github::{Asset, GitHub, Release, Tag};
    use crate::{Error, Result};
    use std::collections::HashMap;

    /// In-memory GitHub: repos missing from every map answer `NotFound`.
    #[derive(Default)]
    pub struct FakeGitHub {
        pub releases: HashMap<String, Release>,
        pub pre_releases: HashMap<String, Vec<Release>>,
        pub tags: HashMap<String, Vec<Tag>>,
        pub branches: HashMap<String, (String, String)>,
        pub failing: HashMap<String, Error>,
    }

    impl FakeGitHub {
        pub fn release(mut self, repo: &str, tag: &str, assets: &[&str]) -> Self {
            let assets = assets
                .iter()
                .map(|n| Asset { name: n.to_string(), url: format!("https://dl/{repo}/{tag}/{n}") })
                .collect();
            self.releases.insert(repo.into(), Release { tag: tag.into(), assets });
            self.branch(repo, "main", "0000000000000000000000000000000000000000")
        }

        /// Also adds the tag, so call it newest first.
        pub fn pre_release(mut self, repo: &str, tag: &str, assets: &[&str]) -> Self {
            let assets = assets
                .iter()
                .map(|n| Asset { name: n.to_string(), url: format!("https://dl/{repo}/{tag}/{n}") })
                .collect();
            self.pre_releases.entry(repo.into()).or_default().push(Release { tag: tag.into(), assets });
            self.tag(repo, tag)
        }

        pub fn tag(mut self, repo: &str, name: &str) -> Self {
            self.tags.entry(repo.into()).or_default().push(Tag { name: name.into(), sha: "abc".into() });
            self.branch(repo, "main", "0000000000000000000000000000000000000000")
        }

        pub fn branch(mut self, repo: &str, branch: &str, sha: &str) -> Self {
            self.branches.insert(repo.into(), (branch.into(), sha.into()));
            self
        }

        pub fn fail(mut self, repo: &str, error: Error) -> Self {
            self.failing.insert(repo.into(), error);
            self
        }

        fn exists(&self, repo: &str) -> Result<()> {
            if let Some(e) = self.failing.get(repo) {
                return Err(e.clone());
            }
            if self.branches.contains_key(repo) {
                Ok(())
            } else {
                Err(Error::NotFound)
            }
        }
    }

    impl GitHub for FakeGitHub {
        fn latest_release(&self, repo: &str) -> Result<Option<Release>> {
            self.exists(repo)?;
            Ok(self.releases.get(repo).cloned())
        }
        fn release(&self, repo: &str, tag: &str) -> Result<Release> {
            self.exists(repo)?;
            let found = self.pre_releases.get(repo).into_iter().flatten().chain(self.releases.get(repo)).find(|r| r.tag == tag);
            Ok(found.cloned().unwrap_or(Release { tag: tag.into(), assets: vec![] }))
        }
        fn tags(&self, repo: &str) -> Result<Vec<Tag>> {
            self.exists(repo)?;
            Ok(self.tags.get(repo).cloned().unwrap_or_default())
        }
        fn default_branch(&self, repo: &str) -> Result<String> {
            self.exists(repo)?;
            Ok(self.branches[repo].0.clone())
        }
        fn commit_sha(&self, repo: &str, _git_ref: &str) -> Result<String> {
            self.exists(repo)?;
            Ok(self.branches[repo].1.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGitHub;
    use super::*;
    use crate::state::Entry;

    fn remote(id: &str) -> RemoteVersion {
        RemoteVersion { id: id.into(), label: id.into(), download: "url".into(), sha256: None }
    }

    fn entry(id: &str, version: &str) -> Entry {
        Entry {
            id: id.into(),
            name: id.into(),
            source: Source::Verified { registry_id: id.into() },
            folders: vec![],
            version: version.into(),
            installed_at: 0,
        }
    }

    #[test]
    fn adopted_older_toc_needs_update() {
        assert_eq!(check("toc:0.4.0", remote("v0.6.0")).0, UpdateStatus::UpdateAvailable(remote("v0.6.0")));
        assert!(matches!(check("toc:", remote("v1")).0, UpdateStatus::UpdateAvailable(_)));
    }

    #[test]
    fn adopted_equal_toc_is_up_to_date() {
        assert_eq!(check("toc:3.0.36", remote("v3.0.36")), (UpdateStatus::UpToDate, Some("v3.0.36".into())));
    }

    #[test]
    fn differing_identifiers_need_update() {
        assert!(matches!(check("build-aaa", remote("build-bbb")).0, UpdateStatus::UpdateAvailable(_)));
        assert_eq!(check("v2.6.5-azs", remote("v2.6.5-azs")).0, UpdateStatus::UpToDate);
    }

    #[test]
    fn zip_source_is_manual() {
        let mut state = State::default();
        state.upsert(Entry { source: Source::Zip { file_name: "x.zip".into() }, ..entry("x", "zip:x.zip") });
        let results = check_all(&FakeGitHub::default(), &mut state, &crate::registry::embedded());
        assert_eq!(results, [("x".to_string(), UpdateStatus::Manual)]);
    }

    #[test]
    fn one_failure_does_not_block_others() {
        let gh = FakeGitHub::default()
            .fail("cadoky/LuckyOdds", Error::RateLimited)
            .release("kex0/AZSSpellMeter", "v1.31", &["AZSSpellMeter-1.31.zip"]);
        let mut state = State::default();
        state.upsert(entry("luckyodds", "toc:0.4.0"));
        state.upsert(entry("azsspellmeter", "toc:1.31"));
        let results = check_all(&gh, &mut state, &crate::registry::embedded());
        assert!(matches!(&results[0].1, UpdateStatus::CheckFailed(m) if m.contains("limiting requests")));
        assert_eq!(results[1].1, UpdateStatus::UpToDate);
        assert_eq!(state.get("azsspellmeter").unwrap().version, "v1.31");
    }

    #[test]
    fn missing_asset_is_reported() {
        let gh = FakeGitHub::default().release("cadoky/LuckyOdds", "v0.6.0", &["Something.zip"]);
        let spec = SourceSpec::Release { asset: "LuckyOdds-v*.zip".into() };
        assert_eq!(resolve_remote(&gh, "cadoky/LuckyOdds", &spec), Err(Error::NoMatchingAsset));
    }

    #[test]
    fn release_source_without_latest_uses_newest_pre_release_with_asset() {
        let gh = FakeGitHub::default()
            .tag("o/r", "upstream/mash-1.3.9-inspected")
            .pre_release("o/r", "v0.2.1-review.3", &["Kit-0.2.1-review.3.zip", "SHA256SUMS.txt"])
            .pre_release("o/r", "v0.1.0-review.3", &["Kit-0.1.0-review.3.zip"]);
        let r = resolve_remote(&gh, "o/r", &SourceSpec::Release { asset: "Kit-*.zip".into() }).unwrap();
        assert_eq!(r.id, "v0.2.1-review.3");
        assert_eq!(r.download, "https://dl/o/r/v0.2.1-review.3/Kit-0.2.1-review.3.zip");
        let none = resolve_remote(&gh, "o/r", &SourceSpec::Release { asset: "Other-*.zip".into() });
        assert_eq!(none, Err(Error::NoRelease));
    }

    #[test]
    fn tag_and_branch_sources() {
        let gh = FakeGitHub::default().tag("threenox/BordoUI", "v2.8.6");
        let r = resolve_remote(&gh, "threenox/BordoUI", &SourceSpec::Tag).unwrap();
        assert_eq!(r.id, "v2.8.6");
        assert_eq!(r.download, "https://codeload.github.com/threenox/BordoUI/zip/v2.8.6");
        let gh = FakeGitHub::default().branch("o/r", "main", "1234567890abcdef");
        let r = resolve_remote(&gh, "o/r", &SourceSpec::Branch).unwrap();
        assert_eq!((r.id.as_str(), r.label.as_str()), ("1234567890abcdef", "1234567"));
    }

    #[test]
    fn platform_latest_needs_no_github() {
        let mut registry = crate::registry::embedded();
        let addon = registry.addons.iter_mut().find(|a| a.id == "luckyodds").unwrap();
        addon.latest = Some(crate::registry::LatestVersion {
            tag: "v9".into(),
            asset_url: "https://github.com/x/LuckyOdds-v9.zip".into(),
            sha256: Some("ab".repeat(32)),
            size: Some(10),
            trust: forge_core::trust::Trust::Scanned,
            findings: Default::default(),
            game_min: None,
            game_max: None,
        });
        let mut state = State::default();
        state.upsert(crate::state::Entry {
            id: "luckyodds".into(),
            name: "LuckyOdds".into(),
            source: Source::Verified { registry_id: "luckyodds".into() },
            folders: vec!["LuckyOdds".into()],
            version: "v8".into(),
            installed_at: 0,
        });
        // Every GitHub call of this fake fails, so a passing check never asked GitHub.
        let mut gh = FakeGitHub::default();
        gh.failing.insert("cadoky/LuckyOdds".into(), Error::RateLimited);
        let results = check_all(&gh, &mut state, &registry);
        match &results[0].1 {
            UpdateStatus::UpdateAvailable(remote) => assert_eq!((remote.id.as_str(), remote.download.as_str()), ("v9", "https://github.com/x/LuckyOdds-v9.zip")),
            other => panic!("{other:?}"),
        }

        let addon = registry.addons.iter_mut().find(|a| a.id == "luckyodds").unwrap();
        addon.latest.as_mut().unwrap().trust = forge_core::trust::Trust::Blocked;
        let results = check_all(&gh, &mut state, &registry);
        assert!(matches!(&results[0].1, UpdateStatus::CheckFailed(m) if m.contains("blocked")), "{:?}", results[0].1);
    }

    #[test]
    fn without_the_platform_github_is_asked_directly() {
        // The stored list knows v9 as scanned. GitHub may still show v9 (then its scan holds) or
        // already a newer version, which no one checked: it comes without a SHA-256.
        let mut registry = crate::registry::embedded();
        let addon = registry.addons.iter_mut().find(|a| a.id == "luckyodds").unwrap();
        addon.latest = Some(crate::registry::LatestVersion {
            tag: "v9".into(),
            asset_url: "https://github.com/cadoky/LuckyOdds/releases/download/v9/LuckyOdds-v9.zip".into(),
            sha256: Some("ab".repeat(32)),
            size: Some(10),
            trust: forge_core::trust::Trust::Scanned,
            findings: Default::default(),
            game_min: None,
            game_max: None,
        });
        let mut state = State::default();
        state.upsert(crate::state::Entry {
            id: "luckyodds".into(),
            name: "LuckyOdds".into(),
            source: Source::Verified { registry_id: "luckyodds".into() },
            folders: vec!["LuckyOdds".into()],
            version: "v8".into(),
            installed_at: 0,
        });
        let remote = |gh: &FakeGitHub, registry: &Registry, state: &mut State| match check_all_direct(gh, state, registry).remove(0).1 {
            UpdateStatus::UpdateAvailable(remote) => remote,
            other => panic!("{other:?}"),
        };

        let same = FakeGitHub::default().release("cadoky/LuckyOdds", "v9", &["LuckyOdds-v9.zip"]);
        let r = remote(&same, &registry, &mut state);
        assert_eq!((r.id.as_str(), r.sha256), ("v9", Some("ab".repeat(32))), "the scanned version keeps its check");

        let newer = FakeGitHub::default().release("cadoky/LuckyOdds", "v10", &["LuckyOdds-v10.zip"]);
        let r = remote(&newer, &registry, &mut state);
        assert_eq!((r.id.as_str(), r.sha256), ("v10", None), "a version the platform never saw is unchecked");

        let addon = registry.addons.iter_mut().find(|a| a.id == "luckyodds").unwrap();
        addon.latest.as_mut().unwrap().trust = forge_core::trust::Trust::Blocked;
        let results = check_all_direct(&same, &mut state, &registry);
        assert!(matches!(&results[0].1, UpdateStatus::CheckFailed(m) if m.contains("blocked")), "a blocked version stays blocked: {:?}", results[0].1);
    }
}
