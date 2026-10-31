//! Pure view logic: turns state, registry and update results into rows for the UI.

use crate::archive::DetectedAddon;
use crate::registry::{Registry, VerifiedAddon};
use crate::scan::InstalledAddon;
use crate::state::{Reconciled, Source, State, TOC_PREFIX};
use crate::updates::UpdateStatus;
use forge_core::trust::Trust;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Badge {
    Verified,
    GitHub,
    Zip,
    Unmanaged,
}

impl Badge {
    pub fn label(self) -> &'static str {
        match self {
            Badge::Verified => "Verified",
            Badge::GitHub => "GitHub",
            Badge::Zip => "Zip",
            Badge::Unmanaged => "Unmanaged",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowState {
    Unchecked,
    UpToDate,
    UpdateAvailable(String),
    Manual,
    CheckFailed(String),
    Missing,
    Unmanaged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Entry id, or `folder:<name>` for an unmanaged folder.
    pub id: String,
    pub name: String,
    pub author: String,
    pub version: String,
    pub badge: Badge,
    pub state: RowState,
    pub description: String,
    pub repo: Option<String>,
    pub out_of_date: bool,
    pub folders: Vec<String>,
}

pub const UNMANAGED_PREFIX: &str = "folder:";

/// `toc:1.0` -> `1.0`, `zip:file.zip` -> `file.zip`, a 40-char SHA -> its first 7 characters.
pub fn display_version(version: &str) -> String {
    if let Some(v) = version.strip_prefix(TOC_PREFIX) {
        return if v.is_empty() { "unknown".into() } else { v.into() };
    }
    if let Some(file) = version.strip_prefix("zip:") {
        return file.into();
    }
    // A bare SHA or a tag ending in one (`build-<sha>`) keeps 7 characters of the SHA.
    let sha_start = version.len().saturating_sub(40);
    let tail = &version[sha_start..];
    if tail.len() == 40 && tail.chars().all(|c| c.is_ascii_hexdigit()) && (sha_start == 0 || version[..sha_start].ends_with('-')) {
        return format!("{}{}", &version[..sha_start], &tail[..7]);
    }
    version.into()
}

pub fn installed_rows(
    state: &State,
    reconciled: &Reconciled,
    scan: &[InstalledAddon],
    registry: &Registry,
    statuses: &HashMap<String, UpdateStatus>,
) -> Vec<Row> {
    let mut rows: Vec<Row> = state
        .entries
        .iter()
        .map(|entry| {
            let toc = entry
                .folders
                .iter()
                .find_map(|f| scan.iter().find(|s| s.folder.eq_ignore_ascii_case(f)))
                .map(|s| &s.toc);
            let (badge, repo, author, description) = match &entry.source {
                Source::Verified { registry_id } => {
                    let addon = registry.get(registry_id);
                    (
                        Badge::Verified,
                        addon.map(|a| a.repo.clone()),
                        addon.map(|a| a.author.clone()).unwrap_or_default(),
                        addon.map(|a| a.description.clone()).unwrap_or_default(),
                    )
                }
                Source::GitHub { repo, .. } => (
                    Badge::GitHub,
                    Some(repo.clone()),
                    repo.split('/').next().unwrap_or_default().to_string(),
                    toc.and_then(|t| t.notes.clone()).unwrap_or_default(),
                ),
                Source::Zip { .. } => (
                    Badge::Zip,
                    None,
                    toc.and_then(|t| t.author.clone()).unwrap_or_default(),
                    toc.and_then(|t| t.notes.clone()).unwrap_or_default(),
                ),
            };
            let state = if reconciled.missing.contains(&entry.id) {
                RowState::Missing
            } else {
                match statuses.get(&entry.id) {
                    None => RowState::Unchecked,
                    Some(UpdateStatus::UpToDate) => RowState::UpToDate,
                    Some(UpdateStatus::UpdateAvailable(r)) => RowState::UpdateAvailable(r.label.clone()),
                    Some(UpdateStatus::Manual) => RowState::Manual,
                    Some(UpdateStatus::CheckFailed(m)) => RowState::CheckFailed(m.clone()),
                }
            };
            Row {
                id: entry.id.clone(),
                name: entry.name.clone(),
                author,
                version: display_version(&entry.version),
                badge,
                state,
                description,
                repo,
                out_of_date: toc.is_some_and(|t| t.is_out_of_date()),
                folders: entry.folders.clone(),
            }
        })
        .collect();
    rows.extend(reconciled.unmanaged.iter().map(|addon| Row {
        id: format!("{UNMANAGED_PREFIX}{}", addon.folder),
        name: addon.toc.title.clone(),
        author: addon.toc.author.clone().unwrap_or_default(),
        version: addon.toc.version.clone().unwrap_or_default(),
        badge: Badge::Unmanaged,
        state: RowState::Unmanaged,
        description: addon.toc.notes.clone().unwrap_or_default(),
        repo: None,
        out_of_date: addon.toc.is_out_of_date(),
        folders: vec![addon.folder.clone()],
    }));
    rows.sort_by_key(|r| r.name.to_lowercase());
    rows
}

/// A game version as the platform lists it; `sort_key` orders them.
#[derive(Debug, Clone, PartialEq)]
pub struct GameVersion {
    pub name: String,
    pub sort_key: i32,
}

/// Why installing a platform addon asks the user first.
#[derive(Debug, Clone, PartialEq)]
pub enum Warning {
    /// The platform has no checked file for it.
    NoHash,
    Unscanned,
    Flagged { critical: u32, warn: u32 },
    Incompatible { game: String },
}

/// The warnings for installing `addon`'s newest version. The plain verified list (no platform
/// data) has none, so a launcher without the platform behaves as before.
pub fn install_warnings(addon: &VerifiedAddon, game: Option<&str>, games: &[GameVersion]) -> Vec<Warning> {
    if addon.verified_author.is_none() {
        return Vec::new();
    }
    let Some(latest) = &addon.latest else { return vec![Warning::NoHash] };
    let mut warnings = Vec::new();
    if latest.sha256.is_none() {
        warnings.push(Warning::NoHash);
    }
    match latest.trust {
        Trust::Unscanned => warnings.push(Warning::Unscanned),
        Trust::Flagged => warnings.push(Warning::Flagged { critical: latest.findings.critical, warn: latest.findings.warn }),
        _ => {}
    }
    let key = |name: &Option<String>| name.as_ref().and_then(|n| games.iter().find(|g| &g.name == n)).map(|g| g.sort_key);
    if let Some(game) = game.and_then(|g| games.iter().find(|v| v.name == g))
        && forge_core::compat::compatible(key(&latest.game_min), key(&latest.game_max), game.sort_key) == Some(false)
    {
        warnings.push(Warning::Incompatible { game: game.name.clone() });
    }
    warnings
}

/// Title, text and whether the dialog is red, for the confirmation before an install.
pub fn warning_dialog(name: &str, warnings: &[Warning]) -> (String, String, bool) {
    let danger = warnings.iter().any(|w| matches!(w, Warning::Flagged { .. }));
    let lines: Vec<String> = warnings
        .iter()
        .map(|w| match w {
            Warning::NoHash => "The platform has no checked file for this version.".to_string(),
            Warning::Unscanned => "This version was not checked automatically yet.".to_string(),
            Warning::Flagged { critical, warn } => {
                format!("The automatic check found {} serious and {} other suspicious things. Read them on the addon's page first.", critical, warn)
            }
            Warning::Incompatible { game } => format!("This version does not state support for your game version {game}."),
        })
        .collect();
    (format!("Install {name}?"), lines.join("\n\n") + "\n\nOnly continue if you trust the author.", danger)
}

/// Splits update ids into the ones "Update all" may install and the ones that need a confirmation.
pub fn split_confirmations(
    ids: Vec<String>,
    registry: &Registry,
    state: &State,
    game: Option<&str>,
    games: &[GameVersion],
) -> (Vec<String>, Vec<String>) {
    ids.into_iter().partition(|id| {
        let registry_id = match state.get(id).map(|e| &e.source) {
            Some(Source::Verified { registry_id }) => registry_id.as_str(),
            _ => id.as_str(),
        };
        registry.get(registry_id).is_none_or(|a| install_warnings(a, game, games).is_empty())
    })
}

/// What installing a platform addon brings along.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DependencyPlan {
    /// Required addons that are not installed yet, dependencies first.
    pub install: Vec<String>,
    /// Required addons the platform cannot deliver: unknown, blocked or changed.
    pub unavailable: Vec<String>,
    /// Names of addons that work with it but are not needed.
    pub optional: Vec<String>,
}

fn is_installed(registry_id: &str, registry: &Registry, state: &State) -> bool {
    state.get(registry_id).is_some()
        || state.entries.iter().any(|e| matches!(&e.source, Source::Verified { registry_id: r } if r == registry_id))
        || registry.get(registry_id).is_some_and(|a| a.folders.iter().any(|f| state.owner_of(f).is_some()))
}

pub fn dependency_plan(id: &str, registry: &Registry, state: &State) -> DependencyPlan {
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for addon in &registry.addons {
        let required = addon.dependencies.iter().filter(|d| d.required).map(|d| d.id.clone()).collect();
        edges.insert(addon.id.clone(), required);
    }
    let mut plan = DependencyPlan::default();
    for dep in forge_core::deps::install_order(&id.to_string(), &edges) {
        match registry.get(&dep) {
            Some(addon) if addon.latest.as_ref().is_none_or(|l| l.trust.installable()) => {
                if !is_installed(&dep, registry, state) {
                    plan.install.push(dep);
                }
            }
            _ => plan.unavailable.push(dep),
        }
    }
    if let Some(addon) = registry.get(id) {
        plan.optional = addon
            .dependencies
            .iter()
            .filter(|d| !d.required)
            .map(|d| registry.get(&d.id).map(|a| a.name.clone()).unwrap_or(d.id.clone()))
            .collect();
    }
    plan
}

/// The label of a platform trust level, the same words the website uses.
pub fn trust_label(trust: Trust, verified_author: bool) -> &'static str {
    match trust {
        Trust::Verified => "Verified",
        Trust::Scanned if verified_author => "Verified author",
        Trust::Scanned => "Checked",
        Trust::Flagged => "Flagged",
        Trust::Unscanned => "Not checked",
        Trust::Blocked => "Blocked",
        Trust::SourceChanged => "File changed",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrowseRow {
    pub id: String,
    pub name: String,
    pub author: String,
    pub description: String,
    pub category: String,
    pub repo: String,
    pub installed: bool,
    /// The platform's trust label of the newest version; `None` from the plain verified list.
    pub trust: Option<&'static str>,
    /// False when the platform blocks the newest version.
    pub installable: bool,
}

pub fn browse_rows(registry: &Registry, state: &State) -> Vec<BrowseRow> {
    registry
        .addons
        .iter()
        .map(|a| BrowseRow {
            id: a.id.clone(),
            name: a.name.clone(),
            author: a.author.clone(),
            description: a.description.clone(),
            category: a.category.clone(),
            repo: a.repo.clone(),
            // Also counts as installed when its folders came from a zip or another repo.
            installed: state.get(&a.id).is_some() || a.folders.iter().any(|f| state.owner_of(f).is_some()),
            trust: a.latest.as_ref().map(|l| trust_label(l.trust, a.verified_author == Some(true))),
            installable: a.latest.as_ref().is_none_or(|l| l.trust.installable()),
        })
        .collect()
}

/// A category of the platform; `parent` for subcategories (Classes -> Warrior).
#[derive(Debug, Clone, PartialEq)]
pub struct Category {
    pub name: String,
    pub parent: Option<String>,
}

impl Category {
    pub fn label(&self) -> String {
        match &self.parent {
            Some(parent) => format!("{parent}: {}", self.name),
            None => self.name.clone(),
        }
    }
}

/// The platform's categories in its order, from `/v1/categories`.
pub fn parse_categories(json: &str) -> Vec<Category> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).unwrap_or_default();
    list.iter()
        .filter_map(|c| Some(Category { name: c["name"].as_str()?.to_string(), parent: c["parent"].as_str().map(str::to_string) }))
        .collect()
}

/// What the Browse filter offers: the platform's categories, or without them (offline) the ones
/// the addon list uses.
pub fn category_choices(platform: &[Category], registry: &Registry) -> Vec<Category> {
    if !platform.is_empty() {
        return platform.to_vec();
    }
    let mut names: Vec<String> = registry.addons.iter().map(|a| a.category.clone()).filter(|c| !c.is_empty()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    names.into_iter().map(|name| Category { name, parent: None }).collect()
}

/// How each choice reads in the open category list, as on the website: a category with
/// subcategories heads its group (1), the subcategories go below it by their own name (2), the rest
/// stay plain (0).
pub fn list_entries(choices: &[Category]) -> Vec<(String, i32)> {
    choices
        .iter()
        .map(|c| match &c.parent {
            Some(_) => (c.name.clone(), 2),
            None if choices.iter().any(|o| o.parent.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(&c.name))) => (c.name.clone(), 1),
            None => (c.name.clone(), 0),
        })
        .collect()
}

/// An addon of `category` passes the filter `wanted` when it is that category or one of its
/// subcategories.
pub fn in_category(category: &str, wanted: &str, categories: &[Category]) -> bool {
    category.eq_ignore_ascii_case(wanted)
        || categories.iter().any(|c| c.name.eq_ignore_ascii_case(category) && c.parent.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(wanted)))
}

/// Whether `addon` can go into an addon pack: the same rule as the platform's, a checked or
/// verified current version with a known SHA-256.
pub fn packable(addon: &VerifiedAddon) -> bool {
    addon.latest.as_ref().is_some_and(|l| matches!(l.trust, Trust::Scanned | Trust::Verified) && l.sha256.is_some())
}

/// What the detail view tells about an addon besides its row; empty where nothing is known.
#[derive(Debug, Default, PartialEq)]
pub struct Details {
    pub category: String,
    /// What the check of the newest version means for the user.
    pub safety: String,
    pub game: String,
    pub folders: String,
    pub needs: String,
}

/// The facts for the detail view: from the platform's entry when there is one, else only the
/// installed `folders` (GitHub and zip addons).
pub fn details(addon: Option<&VerifiedAddon>, registry: &Registry, folders: &[String]) -> Details {
    let Some(addon) = addon else { return Details { folders: folders.join(", "), ..Details::default() } };
    let latest = addon.latest.as_ref();
    let safety = match latest.map(|l| l.trust) {
        Some(Trust::Verified) => "An admin checked this exact file by hand.",
        Some(Trust::Scanned) if addon.verified_author == Some(true) => {
            "An admin verified an earlier version of this author; this one was checked automatically."
        }
        Some(Trust::Scanned) => "The automatic check found nothing. No admin has looked at it by hand.",
        Some(Trust::Flagged) => "The automatic check found something suspicious. You are asked before it is installed.",
        Some(Trust::Unscanned) => "The automatic check has not run yet. You are asked before it is installed.",
        Some(Trust::Blocked) => "An admin blocked this version. It cannot be installed.",
        Some(Trust::SourceChanged) => "The file on GitHub changed after it was checked. It cannot be installed until an admin looks at it.",
        None => "",
    };
    let game = match latest.map(|l| (l.game_min.as_deref(), l.game_max.as_deref())) {
        Some((Some(min), Some(max))) => format!("{min} to {max}"),
        Some((Some(min), None)) => format!("{min} and later"),
        Some((None, Some(max))) => format!("up to {max}"),
        Some((None, None)) => "Every version".into(),
        None => String::new(),
    };
    let needs: Vec<String> = addon
        .dependencies
        .iter()
        .map(|d| {
            let name = registry.get(&d.id).map_or(d.id.as_str(), |a| a.name.as_str());
            format!("{name} ({})", if d.required { "required" } else { "optional" })
        })
        .collect();
    Details { category: addon.category.clone(), safety: safety.into(), game, folders: addon.folders.join(", "), needs: needs.join(", ") }
}

/// The icons of one page of `/v1/addons` as (slug, URL), and whether another page follows.
pub fn icon_page(json: &str) -> Option<(Vec<(String, String)>, bool)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let icons = v["addons"]
        .as_array()?
        .iter()
        .filter_map(|a| Some((a["slug"].as_str()?.to_string(), a["icon_url"].as_str()?.to_string())))
        .collect();
    let (total, page, per_page) = (v["total"].as_i64()?, v["page"].as_i64()?, v["per_page"].as_i64()?);
    Some((icons, page * per_page < total))
}

/// What the launcher shows of an addon's page on the platform (`/v1/addons/{slug}`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AddonInfo {
    /// The long description as Markdown.
    pub about: String,
    pub icon: Option<String>,
    /// Screenshots as (URL, caption) in display order.
    pub shots: Vec<(String, String)>,
}

impl AddonInfo {
    /// `https://host` of the platform's images: images in the description load only from there, as on
    /// the website.
    pub fn image_origin(&self) -> Option<String> {
        let url = self.icon.as_deref().or(self.shots.first().map(|(u, _)| u.as_str()))?;
        let (scheme, rest) = url.split_once("://")?;
        Some(format!("{scheme}://{}", rest.split('/').next()?))
    }
}

pub fn addon_info(json: &str) -> Option<AddonInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    Some(AddonInfo {
        about: text(&v["description_md"]),
        icon: v["icon_url"].as_str().map(str::to_string),
        shots: v["images"].as_array().into_iter().flatten().filter(|i| i["url"].is_string()).map(|i| (text(&i["url"]), text(&i["caption"]))).collect(),
    })
}

/// The website's pack builder with `ids` ticked.
pub fn pack_link(web: &str, ids: &[String]) -> String {
    let query: Vec<String> = ids.iter().map(|id| format!("addon={id}")).collect();
    format!("{web}/packs/new?{}", query.join("&"))
}

pub fn matches(query: &str, name: &str, author: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || name.to_lowercase().contains(&query) || author.to_lowercase().contains(&query)
}

pub fn update_all_ids(statuses: &HashMap<String, UpdateStatus>) -> Vec<String> {
    let mut ids: Vec<String> = statuses
        .iter()
        .filter(|(_, s)| matches!(s, UpdateStatus::UpdateAvailable(_)))
        .map(|(id, _)| id.clone())
        .collect();
    ids.sort();
    ids
}

#[derive(Debug, Clone, PartialEq)]
pub struct ZipPlan {
    /// Entry id to install under: the existing zip entry on an update, a new one otherwise.
    pub id: String,
    pub name: String,
    pub addons: Vec<String>,
    /// True when this replaces an existing zip install of the same addon.
    pub is_update: bool,
    /// Entries from GitHub or the verified list that lose their folders to this zip.
    pub takes_over: Vec<(String, String)>,
    /// Folders on disk that SurvivorsForge does not manage yet and that get overwritten.
    pub overwrites_unmanaged: Vec<String>,
}

pub fn plan_zip_install(detected: &[DetectedAddon], state: &State, scan: &[InstalledAddon]) -> ZipPlan {
    let mut owners: Vec<(String, String, bool)> = Vec::new();
    let mut overwrites_unmanaged = Vec::new();
    for addon in detected {
        match state.owner_of(&addon.name) {
            Some(owner) if !owners.iter().any(|o| o.0 == owner.id) => {
                owners.push((owner.id.clone(), owner.name.clone(), matches!(owner.source, Source::Zip { .. })));
            }
            Some(_) => {}
            None if scan.iter().any(|s| s.folder.eq_ignore_ascii_case(&addon.name)) => {
                overwrites_unmanaged.push(addon.name.clone());
            }
            None => {}
        }
    }
    let zip_owner = match owners.as_slice() {
        [(id, name, true)] => Some((id.clone(), name.clone())),
        _ => None,
    };
    let first = &detected[0];
    let (id, name) = zip_owner
        .clone()
        .unwrap_or_else(|| (format!("zip-{}", first.name.to_lowercase()), first.toc.title.clone()));
    ZipPlan {
        id,
        name,
        addons: detected.iter().map(|a| a.name.clone()).collect(),
        is_update: zip_owner.is_some(),
        takes_over: if zip_owner.is_some() {
            vec![]
        } else {
            owners.into_iter().map(|(id, name, _)| (id, name)).collect()
        },
        overwrites_unmanaged,
    }
}

impl ZipPlan {
    pub fn describe(&self) -> String {
        let mut text = format!(
            "{} {}: {}.",
            if self.is_update { "Update" } else { "Install" },
            self.name,
            self.addons.join(", ")
        );
        if !self.takes_over.is_empty() {
            let names: Vec<&str> = self.takes_over.iter().map(|(_, n)| n.as_str()).collect();
            text += &format!(
                "\n\nThis replaces {}. Its source switches to this zip and automatic updates stop.",
                names.join(", ")
            );
        }
        if !self.overwrites_unmanaged.is_empty() {
            text += &format!("\n\nExisting folders get replaced: {}.", self.overwrites_unmanaged.join(", "));
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_urls_come_from_a_page_of_the_addon_list() {
        let page = r#"{"total": 51, "page": 1, "per_page": 50, "addons": [
            {"slug": "lucky", "icon_url": "https://forge.test/img/a.webp"}, {"slug": "plain", "icon_url": null}]}"#;
        let (icons, more) = icon_page(page).unwrap();
        assert_eq!(icons, [("lucky".to_string(), "https://forge.test/img/a.webp".to_string())]);
        assert!(more, "51 addons do not fit on one page of 50");
        assert!(!icon_page(r#"{"total": 2, "page": 1, "per_page": 50, "addons": []}"#).unwrap().1);
        assert!(icon_page("not json").is_none());
    }

    #[test]
    fn addon_info_takes_description_icon_and_screenshots() {
        let json = r##"{"description_md": "# Hi", "icon_url": "https://forge.test/img/i.webp",
            "images": [{"url": "https://forge.test/img/1.webp", "caption": "Bars"}, {"url": "https://forge.test/img/2.webp", "caption": ""}]}"##;
        let info = addon_info(json).unwrap();
        assert_eq!(info.about, "# Hi");
        assert_eq!(info.icon.as_deref(), Some("https://forge.test/img/i.webp"));
        assert_eq!(info.shots, [("https://forge.test/img/1.webp".to_string(), "Bars".to_string()), ("https://forge.test/img/2.webp".to_string(), String::new())]);
        assert_eq!(info.image_origin().as_deref(), Some("https://forge.test"), "description images only from the platform's image host");
        assert_eq!(addon_info("{}").unwrap(), AddonInfo::default());
    }
    use crate::state::Entry;
    use crate::toc::Toc;
    use crate::updates::RemoteVersion;

    fn scanned(folder: &str) -> InstalledAddon {
        InstalledAddon { folder: folder.into(), toc: Toc { title: folder.into(), ..Toc::default() } }
    }

    fn detected(name: &str) -> DetectedAddon {
        DetectedAddon { name: name.into(), prefix: name.into(), toc: Toc { title: format!("{name} Title"), ..Toc::default() } }
    }

    fn entry(id: &str, folder: &str, source: Source) -> Entry {
        Entry { id: id.into(), name: id.into(), source, folders: vec![folder.into()], version: "v1".into(), installed_at: 0 }
    }

    fn zip_source() -> Source {
        Source::Zip { file_name: "a.zip".into() }
    }

    #[test]
    fn filter_matches_name_and_author_case_insensitive() {
        assert!(matches("lucky", "LuckyOdds", "cadoky"));
        assert!(matches("CADO", "LuckyOdds", "cadoky"));
        assert!(matches("  ", "x", "y"));
        assert!(!matches("bordo", "LuckyOdds", "cadoky"));
    }

    #[test]
    fn rows_from_state_have_correct_badges() {
        let mut state = State::default();
        state.upsert(entry("luckyodds", "LuckyOdds", Source::Verified { registry_id: "luckyodds".into() }));
        state.upsert(entry("gh:o/r", "R", Source::GitHub { repo: "o/r".into(), spec: crate::registry::SourceSpec::Tag }));
        state.upsert(entry("zip-z", "Z", zip_source()));
        let scan = [scanned("LuckyOdds"), scanned("R"), scanned("Z"), scanned("Tickfire")];
        let reconciled = state.reconcile(&scan);
        let mut statuses = HashMap::new();
        statuses.insert("luckyodds".to_string(), UpdateStatus::UpdateAvailable(RemoteVersion { sha256: None,
            id: "v0.6.0".into(),
            label: "v0.6.0".into(),
            download: String::new(),
        }));
        let rows = installed_rows(&state, &reconciled, &scan, &crate::registry::embedded(), &statuses);
        let summary: Vec<(&str, Badge)> = rows.iter().map(|r| (r.id.as_str(), r.badge)).collect();
        assert_eq!(summary, [
            ("gh:o/r", Badge::GitHub),
            ("luckyodds", Badge::Verified),
            ("folder:Tickfire", Badge::Unmanaged),
            ("zip-z", Badge::Zip),
        ]);
        assert_eq!(rows[1].state, RowState::UpdateAvailable("v0.6.0".into()));
        assert_eq!(rows[1].author, "cadoky");
        assert_eq!(rows[0].state, RowState::Unchecked);
    }

    #[test]
    fn missing_folder_shows_missing() {
        let mut state = State::default();
        state.upsert(entry("zip-z", "Z", zip_source()));
        let rows = installed_rows(&state, &state.reconcile(&[]), &[], &crate::registry::embedded(), &HashMap::new());
        assert_eq!(rows[0].state, RowState::Missing);
    }

    #[test]
    fn update_all_selects_only_update_available() {
        let remote = RemoteVersion { id: "v2".into(), label: "v2".into(), download: String::new(), sha256: None };
        let statuses = HashMap::from([
            ("b".to_string(), UpdateStatus::UpdateAvailable(remote.clone())),
            ("a".to_string(), UpdateStatus::UpdateAvailable(remote)),
            ("c".to_string(), UpdateStatus::UpToDate),
            ("d".to_string(), UpdateStatus::CheckFailed("x".into())),
        ]);
        assert_eq!(update_all_ids(&statuses), ["a", "b"]);
    }

    #[test]
    fn display_versions() {
        assert_eq!(display_version("toc:0.4.0"), "0.4.0");
        assert_eq!(display_version("toc:"), "unknown");
        assert_eq!(display_version("zip:a.zip"), "a.zip");
        assert_eq!(display_version("2030b8c58c6510b91c9835b02d22b416e7329e39"), "2030b8c");
        assert_eq!(display_version("build-3c7ce887d15aef46d8e0c17b9fe71142cb37c4ba"), "build-3c7ce88");
        assert_eq!(display_version("v1.0"), "v1.0");
    }

    #[test]
    fn zip_plan_new_addon() {
        let plan = plan_zip_install(&[detected("Tickfire")], &State::default(), &[]);
        assert_eq!(plan.id, "zip-tickfire");
        assert_eq!(plan.name, "Tickfire Title");
        assert!(!plan.is_update && plan.takes_over.is_empty() && plan.overwrites_unmanaged.is_empty());
    }

    #[test]
    fn zip_plan_updates_existing_zip_entry() {
        let mut state = State::default();
        state.upsert(entry("zip-a", "A", zip_source()));
        let plan = plan_zip_install(&[detected("A")], &state, &[scanned("A")]);
        assert_eq!(plan.id, "zip-a");
        assert!(plan.is_update);
        assert!(plan.takes_over.is_empty());
    }

    #[test]
    fn zip_plan_requires_confirmation_for_managed_source() {
        let mut state = State::default();
        state.upsert(entry("luckyodds", "LuckyOdds", Source::Verified { registry_id: "luckyodds".into() }));
        let plan = plan_zip_install(&[detected("LuckyOdds")], &state, &[scanned("LuckyOdds")]);
        assert_eq!(plan.takes_over, [("luckyodds".to_string(), "luckyodds".to_string())]);
        assert!(!plan.is_update);
        assert!(plan.describe().contains("automatic updates stop"));
    }

    #[test]
    fn zip_plan_reports_unmanaged_overwrite() {
        let plan = plan_zip_install(&[detected("Tickfire")], &State::default(), &[scanned("Tickfire")]);
        assert_eq!(plan.overwrites_unmanaged, ["Tickfire"]);
    }

    fn platform_registry(trust: Trust) -> Registry {
        let mut registry = crate::registry::embedded();
        for addon in &mut registry.addons {
            addon.latest = Some(crate::registry::LatestVersion {
                tag: "v1".into(),
                asset_url: "https://github.com/a.zip".into(),
                sha256: None,
                size: None,
                trust,
                findings: Default::default(),
                game_min: None,
                game_max: None,
            });
        }
        registry
    }

    #[test]
    fn badge_per_trust() {
        let cases = [
            (Trust::Verified, false, "Verified"),
            (Trust::Scanned, true, "Verified author"),
            (Trust::Scanned, false, "Checked"),
            (Trust::Flagged, false, "Flagged"),
            (Trust::Unscanned, false, "Not checked"),
            (Trust::Blocked, false, "Blocked"),
            (Trust::SourceChanged, false, "File changed"),
        ];
        for (trust, author, label) in cases {
            assert_eq!(trust_label(trust, author), label);
        }
        let rows = browse_rows(&platform_registry(Trust::Flagged), &State::default());
        assert!(rows.iter().all(|r| r.trust == Some("Flagged")));
        let plain = browse_rows(&crate::registry::embedded(), &State::default());
        assert!(plain.iter().all(|r| r.trust.is_none() && r.installable), "the plain verified list has no trust data");
    }

    #[test]
    fn blocked_rows_not_installable() {
        for trust in [Trust::Blocked, Trust::SourceChanged] {
            let rows = browse_rows(&platform_registry(trust), &State::default());
            assert!(rows.iter().all(|r| !r.installable));
        }
        assert!(browse_rows(&platform_registry(Trust::Flagged), &State::default()).iter().all(|r| r.installable));
    }

    fn latest(trust: Trust, sha: bool, min: Option<&str>, max: Option<&str>) -> VerifiedAddon {
        let mut addon = crate::registry::embedded().addons.remove(0);
        addon.verified_author = Some(false);
        addon.latest = Some(crate::registry::LatestVersion {
            tag: "v1".into(),
            asset_url: "https://github.com/a.zip".into(),
            sha256: sha.then(|| "ab".repeat(32)),
            size: None,
            trust,
            findings: forge_core::registry::FindingCounts { critical: 1, warn: 2 },
            game_min: min.map(str::to_string),
            game_max: max.map(str::to_string),
        });
        addon
    }

    #[test]
    fn details_of_an_addon() {
        let mut registry = crate::registry::embedded();
        let mut addon = latest(Trust::Flagged, true, Some("0.68"), None);
        addon.category = "Bags".into();
        addon.folders = vec!["Lucky".into(), "LuckyLib".into()];
        let mut lib = addon.clone();
        lib.id = "lib".into();
        lib.name = "Lib".into();
        addon.dependencies = vec![
            forge_core::registry::DependencyRef { id: "lib".into(), required: true },
            forge_core::registry::DependencyRef { id: "gone".into(), required: false },
        ];
        registry.addons = vec![addon.clone(), lib];

        let facts = details(Some(&addon), &registry, &[]);
        assert_eq!(facts.category, "Bags");
        assert!(facts.safety.contains("found something") && facts.safety.contains("asked"), "{}", facts.safety);
        assert_eq!(facts.game, "0.68 and later");
        assert_eq!(facts.folders, "Lucky, LuckyLib");
        assert_eq!(facts.needs, "Lib (required), gone (optional)");

        let mut both = latest(Trust::Verified, true, Some("0.68"), Some("0.69"));
        both.dependencies.clear();
        let facts = details(Some(&both), &registry, &[]);
        assert_eq!((facts.game.as_str(), facts.needs.as_str()), ("0.68 to 0.69", ""));
        assert!(facts.safety.contains("by hand"));
        assert_eq!(details(Some(&latest(Trust::Verified, true, None, Some("0.69"))), &registry, &[]).game, "up to 0.69");

        let own = details(None, &registry, &["Mine".to_string()]);
        assert_eq!((own.folders.as_str(), own.safety.as_str(), own.game.as_str()), ("Mine", "", ""), "a GitHub or zip addon: what the folder tells");
    }

    fn games() -> Vec<GameVersion> {
        vec![GameVersion { name: "0.68".into(), sort_key: 1 }, GameVersion { name: "0.69".into(), sort_key: 2 }]
    }

    #[test]
    fn warnings_per_trust_and_compat() {
        assert!(install_warnings(&latest(Trust::Scanned, true, None, None), None, &games()).is_empty());
        assert!(install_warnings(&latest(Trust::Verified, true, None, None), Some("0.69"), &games()).is_empty());
        assert_eq!(install_warnings(&latest(Trust::Unscanned, true, None, None), None, &games()), [Warning::Unscanned]);
        assert_eq!(install_warnings(&latest(Trust::Flagged, true, None, None), None, &games()), [Warning::Flagged { critical: 1, warn: 2 }]);
        assert_eq!(install_warnings(&latest(Trust::Scanned, false, None, None), None, &games()), [Warning::NoHash]);
        assert_eq!(
            install_warnings(&latest(Trust::Scanned, true, Some("0.69"), None), Some("0.68"), &games()),
            [Warning::Incompatible { game: "0.68".into() }]
        );
        assert!(install_warnings(&latest(Trust::Scanned, true, Some("0.68"), None), None, &games()).is_empty(), "no game chosen, no warning");
        let plain = crate::registry::embedded().addons.remove(0);
        assert!(install_warnings(&plain, Some("0.68"), &games()).is_empty(), "plain verified list: as before");

        let (_, text, danger) = warning_dialog("Lucky", &[Warning::Flagged { critical: 1, warn: 2 }]);
        assert!(danger && text.contains("1 serious"));
        assert!(!warning_dialog("Lucky", &[Warning::Unscanned]).2);
    }

    #[test]
    fn update_all_skips_confirmations() {
        let mut registry = crate::registry::embedded();
        let flagged = latest(Trust::Flagged, true, None, None);
        let mut clean = latest(Trust::Scanned, true, None, None);
        clean.id = "clean".into();
        registry.addons = vec![flagged.clone(), clean];
        let ids = vec![flagged.id.clone(), "clean".to_string(), "gh:o/r".to_string()];
        let (go, skipped) = split_confirmations(ids, &registry, &State::default(), None, &games());
        assert_eq!(go, ["clean", "gh:o/r"]);
        assert_eq!(skipped, [flagged.id]);
    }

    fn dep_registry() -> Registry {
        let mut registry = crate::registry::embedded();
        let template = latest(Trust::Scanned, true, None, None);
        let make = |id: &str, deps: &[(&str, bool)]| {
            let mut a = template.clone();
            a.id = id.into();
            a.name = id.to_uppercase();
            a.folders = vec![id.to_uppercase()];
            a.dependencies = deps.iter().map(|(d, r)| forge_core::registry::DependencyRef { id: d.to_string(), required: *r }).collect();
            a
        };
        registry.addons = vec![
            make("ui", &[("media", true), ("lib", true), ("extras", false)]),
            make("media", &[("lib", true)]),
            make("lib", &[("core", true)]),
            make("core", &[]),
            make("extras", &[]),
        ];
        registry
    }

    #[test]
    fn plan_only_missing_required_in_order() {
        let registry = dep_registry();
        let plan = dependency_plan("ui", &registry, &State::default());
        assert_eq!(plan.install, ["core", "lib", "media"]);
        let mut state = State::default();
        state.upsert(crate::state::Entry {
            id: "lib".into(),
            name: "LIB".into(),
            source: Source::Verified { registry_id: "lib".into() },
            folders: vec!["LIB".into()],
            version: "v1".into(),
            installed_at: 0,
        });
        assert_eq!(dependency_plan("ui", &registry, &state).install, ["core", "media"], "installed ones are skipped");
    }

    #[test]
    fn plan_collects_warnings() {
        let mut registry = dep_registry();
        registry.addons.iter_mut().find(|a| a.id == "lib").unwrap().latest.as_mut().unwrap().trust = Trust::Flagged;
        let plan = dependency_plan("ui", &registry, &State::default());
        let flagged: Vec<&String> = plan.install.iter().filter(|d| !install_warnings(registry.get(d).unwrap(), None, &[]).is_empty()).collect();
        assert_eq!(flagged, ["lib"]);
    }

    #[test]
    fn unavailable_dependency_blocks() {
        let mut registry = dep_registry();
        registry.addons.iter_mut().find(|a| a.id == "core").unwrap().latest.as_mut().unwrap().trust = Trust::Blocked;
        registry.addons.retain(|a| a.id != "media");
        let mut unavailable = dependency_plan("ui", &registry, &State::default()).unavailable;
        unavailable.sort();
        assert_eq!(unavailable, ["core", "media"]);
    }

    #[test]
    fn optional_dependencies_listed() {
        assert_eq!(dependency_plan("ui", &dep_registry(), &State::default()).optional, ["EXTRAS"]);
    }

    #[test]
    fn only_checked_versions_with_a_hash_go_into_packs() {
        let mut addon = crate::registry::embedded().addons.remove(0);
        assert!(!packable(&addon), "the plain verified list has no versions");
        let latest = |trust, sha256: Option<&str>| crate::registry::LatestVersion {
            tag: "v1".into(),
            asset_url: "https://github.com/a.zip".into(),
            sha256: sha256.map(str::to_string),
            size: None,
            trust,
            findings: Default::default(),
            game_min: None,
            game_max: None,
        };
        for (trust, sha, ok) in [(Trust::Scanned, Some("ab"), true), (Trust::Verified, Some("ab"), true), (Trust::Verified, None, false), (Trust::Flagged, Some("ab"), false)] {
            addon.latest = Some(latest(trust, sha));
            assert_eq!(packable(&addon), ok, "{trust:?} {sha:?}");
        }
        assert_eq!(pack_link("https://w", &["a".into(), "b".into()]), "https://w/packs/new?addon=a&addon=b");
    }

    #[test]
    fn categories_filter_with_their_subcategories() {
        let json = r#"[{"name":"Classes","parent":null},{"name":"Warrior","parent":"Classes"},{"name":"Combat","parent":null}]"#;
        let categories = parse_categories(json);
        assert_eq!(categories.iter().map(Category::label).collect::<Vec<_>>(), ["Classes", "Classes: Warrior", "Combat"]);
        assert!(in_category("Warrior", "Classes", &categories));
        assert!(in_category("warrior", "Warrior", &categories));
        assert!(!in_category("Combat", "Classes", &categories));
        assert!(!in_category("Warrior", "Combat", &categories));

        let registry = crate::registry::embedded();
        assert_eq!(category_choices(&categories, &registry), categories);
        let offline: Vec<String> = category_choices(&[], &registry).into_iter().map(|c| c.name).collect();
        assert_eq!(offline, ["Action Bars", "Combat", "Miscellaneous", "Unit Frames"], "from the built-in list");

        // In the open list a parent heads its group and the subcategories sit below it.
        let list = list_entries(&categories);
        assert_eq!(list, [("Classes".to_string(), 1), ("Warrior".to_string(), 2), ("Combat".to_string(), 0)]);
    }
}
