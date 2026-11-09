//! Pure view logic: turns state, registry and update results into rows for the UI.

use crate::archive::DetectedAddon;
use crate::registry::{Registry, VerifiedAddon};
use crate::scan::InstalledAddon;
use crate::state::{Reconciled, Source, State, TOC_PREFIX};
use crate::updates::{RemoteVersion, UpdateStatus};
use forge_core::trust::{Tone, Trust};
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
    /// Its folders wait in `scan::disabled_dir`: the game does not load it.
    pub disabled: bool,
}

pub const UNMANAGED_PREFIX: &str = "folder:";

/// `toc:1.0` -> `1.0`, `zip:file.zip` -> `file.zip`, a 40-char SHA -> its first 7 characters.
/// Why quitting now would break something, or `None` when nothing is running. A game update
/// stopped between files leaves the game half updated.
pub fn quit_warning(game_updating: bool, self_updating: bool, addons_busy: usize) -> Option<String> {
    if game_updating {
        Some("The game update is still running. Quitting now stops it halfway and the game may not start.".into())
    } else if self_updating {
        Some("SurvivorsForge is updating itself. Quitting now stops the update.".into())
    } else if addons_busy > 0 {
        Some(format!("{addons_busy} addon install{} still running. Quitting now stops {}.", if addons_busy == 1 { " is" } else { "s are" }, if addons_busy == 1 { "it" } else { "them" }))
    } else {
        None
    }
}

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
                disabled: entry.folders.iter().any(|f| scan.iter().any(|s| s.disabled && s.folder.eq_ignore_ascii_case(f))),
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
        disabled: addon.disabled,
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

/// Every addon `id` needs, the ones it needs first; without `id` itself.
pub fn required_addons(id: &str, registry: &Registry) -> Vec<String> {
    let edges: HashMap<String, Vec<String>> =
        registry.addons.iter().map(|a| (a.id.clone(), a.dependencies.iter().filter(|d| d.required).map(|d| d.id.clone()).collect())).collect();
    forge_core::deps::install_order(&id.to_string(), &edges).into_iter().filter(|d| d != id).collect()
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

/// Installed puts the addons with an update on top, each part in the order it had; one kept at an
/// older version on purpose stays with the rest.
pub fn updates_first<R: std::borrow::Borrow<Row>>(rows: &mut [R], held: &[String]) {
    rows.sort_by_key(|r| {
        let r = r.borrow();
        !(matches!(r.state, RowState::UpdateAvailable(_)) && !held.contains(&r.id))
    });
}

/// `message` of a finished change to the addon folders; a running game keeps the files it loaded.
pub fn note_running_game(message: String, game_running: bool) -> String {
    if game_running { format!("{message}. It takes effect the next time the game starts.") } else { message }
}

/// Names of the installed platform addons that need `registry_id` to work: removing it breaks them.
pub fn dependents(registry_id: &str, registry: &Registry, state: &State) -> Vec<String> {
    registry
        .addons
        .iter()
        .filter(|a| a.id != registry_id && a.dependencies.iter().any(|d| d.required && d.id == registry_id))
        .filter(|a| is_installed(&a.id, registry, state))
        .map(|a| a.name.clone())
        .collect()
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
    /// The platform's trust tag of the newest version and its colour, as on the website; `None`
    /// from the plain verified list.
    pub trust: Option<(&'static str, Tone)>,
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
            trust: a.latest.as_ref().map(|l| (l.trust.label(a.verified_author == Some(true)), l.trust.tone(a.verified_author == Some(true)))),
            installable: a.latest.as_ref().is_none_or(|l| l.trust.installable()),
        })
        .collect()
}

/// The order of Browse and Installed, as the website offers it; most downloads first by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Downloads,
    Updated,
    Name,
}

/// The sort dropdown's choices, in `Sort`'s order.
pub const SORT_LABELS: [&str; 3] = ["Most downloads", "Recently updated", "Name"];

impl Sort {
    pub fn from_index(index: i32) -> Sort {
        match index {
            1 => Sort::Updated,
            2 => Sort::Name,
            _ => Sort::Downloads,
        }
    }
}

/// What a list sorts an addon by; GitHub and zip addons have no downloads or release date.
pub struct SortKey<'a> {
    pub name: &'a str,
    pub downloads: Option<i64>,
    /// When its newest version came out, as the platform writes it (sorts as text).
    pub updated: Option<&'a str>,
}

/// Unknown values go last; ties by name, ignoring case.
pub fn compare(sort: Sort, a: &SortKey, b: &SortKey) -> std::cmp::Ordering {
    let by_name = || a.name.to_lowercase().cmp(&b.name.to_lowercase());
    let first = match sort {
        Sort::Downloads => b.downloads.unwrap_or(-1).cmp(&a.downloads.unwrap_or(-1)),
        Sort::Updated => b.updated.cmp(&a.updated),
        Sort::Name => std::cmp::Ordering::Equal,
    };
    first.then_with(by_name)
}

/// The safety check dropdown's choices, as the website's filter offers them.
pub const SAFETY_LABELS: [&str; 5] = ["Any safety check", "Verified", "Checked", "Flagged", "Not checked"];

/// The trust level the dropdown at `index` keeps; `None` for any.
pub fn safety_filter(index: i32) -> Option<Trust> {
    match index {
        1 => Some(Trust::Verified),
        2 => Some(Trust::Scanned),
        3 => Some(Trust::Flagged),
        4 => Some(Trust::Unscanned),
        _ => None,
    }
}

/// Whether an addon whose newest version has `level` passes the filter.
pub fn passes_safety(filter: Option<Trust>, level: Option<Trust>) -> bool {
    filter.is_none_or(|f| level == Some(f))
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
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AddonInfo {
    /// The long description as Markdown.
    pub about: String,
    pub icon: Option<String>,
    /// Screenshots as (URL, caption) in display order.
    pub shots: Vec<(String, String)>,
    /// The owner turned the Issues tab on: bugs may be reported on the platform (see `bug_report`).
    #[serde(default)]
    pub issues_on: bool,
    /// The owner turned the Comments tab on.
    #[serde(default)]
    pub comments_on: bool,
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
        issues_on: v["issues_enabled"] == true,
        comments_on: v["comments_enabled"] == true,
    })
}

/// `/v1/addons/{slug}/issues/frame`: whether the Issues tab is on (`issues_here`) and whether the
/// platform takes bug reports (`ready`: the tab is on and the App may write issues in the repo).
pub fn issue_frame(json: &str) -> Option<(bool, bool)> {
    let v = serde_json::from_str::<serde_json::Value>(json).ok()?;
    Some((v["issues_here"].as_bool()?, v["ready"].as_bool()?))
}

/// The bug button of an addon, as on the website: the platform's form once everything works,
/// GitHub's own form otherwise and while that is not known (offline, or not asked yet).
pub fn bug_report(web: &str, slug: &str, repo: &str, issues_on: bool, ready: Option<bool>) -> (&'static str, String) {
    if issues_on && ready == Some(true) {
        ("Report a bug", format!("{web}/addons/{slug}/issues/new?kind=bug"))
    } else {
        ("Report on GitHub", format!("https://github.com/{repo}/issues/new"))
    }
}

/// The pages of an addon on the website the detail view links to.
pub fn addon_link(web: &str, slug: &str, page: &str) -> Option<String> {
    let path = match page {
        "website" => String::new(),
        "issues" => "/issues".into(),
        "comments" => "/comments".into(),
        "abuse" => "/report".into(),
        // One issue of the Issues tab.
        p if p.strip_prefix("issues/").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) => format!("/{p}"),
        _ => return None,
    };
    Some(format!("{web}/addons/{slug}{path}"))
}

/// The GitHub page of release `tag` of `repo` ("owner/name"), where its release notes are.
/// What helps after an update failed, offered beside it in the results of "Update all".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fix {
    None,
    Retry,
    /// GitHub limits requests without a login.
    SignIn,
    /// The folder cannot be written: look at it.
    OpenFolder,
    /// The platform holds the version back: its details say why.
    Details,
    /// The update asks for a confirmation first.
    Review,
}

impl Fix {
    pub fn label(self) -> &'static str {
        match self {
            Fix::None => "",
            Fix::Retry => "Retry",
            Fix::SignIn => "Sign in",
            Fix::OpenFolder => "Open folder",
            Fix::Details => "Details",
            Fix::Review => "Review",
        }
    }
}

/// How one addon of "Update all" ended.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateOutcome {
    pub id: String,
    pub name: String,
    /// "1.1 to 1.2" when it updated, else why not.
    pub line: String,
    pub failed: bool,
    pub fix: Fix,
    /// The release page of the new version.
    pub notes: Option<String>,
}

/// Why an update failed, in words, and what helps.
pub fn fix_for(error: &crate::Error, game_running: bool) -> (String, Fix) {
    use crate::Error;
    let why = error.to_string();
    match error {
        Error::Io(_) if game_running => (format!("{why}. Close the game and try again."), Fix::Retry),
        Error::Io(_) => (why, Fix::OpenFolder),
        Error::RateLimited => (why, Fix::SignIn),
        Error::HashMismatch { .. } | Error::NotInstallable(_) | Error::ForbiddenHost(_) | Error::NoMatchingAsset | Error::NoRelease | Error::NoAddonFound => {
            (why, Fix::Details)
        }
        _ => (why, Fix::Retry),
    }
}

/// The release page of version `tag`; none for a commit, which has no release.
pub fn notes_page(repo: &str, tag: &str) -> Option<String> {
    let commit = tag.len() == 40 && tag.bytes().all(|b| b.is_ascii_hexdigit());
    (!tag.is_empty() && !commit).then(|| release_page(repo, tag))
}

pub fn release_page(repo: &str, tag: &str) -> String {
    let tag: String = tag
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("https://github.com/{repo}/releases/tag/{tag}")
}

/// An open issue on the detail view's Issues tab.
#[derive(Debug, Clone, PartialEq)]
pub struct IssueLine {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub comments: u64,
    /// The day it was opened, `YYYY-MM-DD`.
    pub date: String,
}

/// The open issues of `/v1/addons/{slug}/issues` and how many are open; none while the tab is off.
pub fn issue_lines(json: &str) -> Option<(Vec<IssueLine>, u64)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let issues = v["issues"].as_array().into_iter().flatten().filter_map(|i| {
        Some(IssueLine {
            number: i["number"].as_u64()?,
            title: i["title"].as_str()?.to_string(),
            author: i["author"]["login"].as_str().unwrap_or_default().to_string(),
            comments: i["comments"].as_u64().unwrap_or_default(),
            date: i["created_at"].as_str().unwrap_or_default().chars().take(10).collect(),
        })
    });
    Some((issues.collect(), v["counts"]["open"].as_u64().unwrap_or_default()))
}

/// The kinds of a new issue as the platform takes them, with their names in the form.
pub const ISSUE_KINDS: [(&str, &str); 3] = [("bug", "Bug report"), ("feature", "Feature request"), ("question", "Question")];

/// The body of `POST /v1/addons/{slug}/issues`; what is missing is refused before any request.
pub fn issue_body(kind: &str, title: &str, body: &str) -> Result<serde_json::Value, &'static str> {
    if !ISSUE_KINDS.iter().any(|(k, _)| *k == kind) {
        return Err("Choose a bug report, a feature request or a question");
    }
    let (title, body) = (title.trim(), body.trim());
    if title.is_empty() {
        return Err("Give the issue a title");
    }
    if body.is_empty() {
        return Err("Describe the issue");
    }
    Ok(serde_json::json!({ "kind": kind, "title": title, "body": body }))
}

/// One issue with its replies, as `/v1/addons/{slug}/issues/{number}` gives it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IssuePage {
    pub number: u64,
    pub title: String,
    /// `open` or `closed`.
    pub state: String,
    pub author: String,
    pub date: String,
    pub body: String,
    pub locked: bool,
    pub replies: Vec<IssueReply>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct IssueReply {
    pub author: String,
    pub date: String,
    pub body: String,
}

pub fn issue_page(json: &str) -> Option<IssuePage> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let day = |v: &serde_json::Value| v.as_str().unwrap_or_default().chars().take(10).collect::<String>();
    let i = &v["issue"];
    let replies = v["replies"].as_array().into_iter().flatten().map(|r| IssueReply { author: text(&r["author"]["login"]), date: day(&r["created_at"]), body: text(&r["body"]) });
    Some(IssuePage {
        number: i["number"].as_u64()?,
        title: text(&i["title"]),
        state: text(&i["state"]),
        author: text(&i["author"]["login"]),
        date: day(&i["created_at"]),
        body: text(&i["body"]),
        locked: i["locked"] == true,
        replies: replies.collect(),
    })
}

/// A version on the detail view's Versions tab.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionLine {
    pub tag: String,
    /// The release name, the tag without one.
    pub name: String,
    /// The day it was published, `YYYY-MM-DD`.
    pub date: String,
    pub trust: Trust,
    pub downloads: u64,
    pub prerelease: bool,
    /// What installing exactly this version takes; `None` when the platform blocks it or its file is
    /// gone from GitHub.
    pub install: Option<RemoteVersion>,
    /// What the automatic check found in it.
    pub findings: forge_core::registry::FindingCounts,
}

/// The warnings for installing exactly `version`, as `install_warnings` gives them for the newest.
pub fn version_warnings(version: &VersionLine) -> Vec<Warning> {
    let mut warnings = Vec::new();
    if version.install.as_ref().is_some_and(|r| r.sha256.is_none()) {
        warnings.push(Warning::NoHash);
    }
    match version.trust {
        Trust::Unscanned => warnings.push(Warning::Unscanned),
        Trust::Flagged => warnings.push(Warning::Flagged { critical: version.findings.critical, warn: version.findings.warn }),
        _ => {}
    }
    warnings
}

/// How a version is named in a dialog: release names often start with the addon's name already.
pub fn version_title(addon: &str, version: &str) -> String {
    if version.to_lowercase().starts_with(&addon.to_lowercase()) { version.to_string() } else { format!("{addon} {version}") }
}

/// Every version of `/v1/addons/{slug}/versions`, newest first as the platform sends them.
pub fn version_lines(json: &str) -> Option<Vec<VersionLine>> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    Some(
        list.iter()
            .filter_map(|v| {
                let tag = v["tag"].as_str()?.to_string();
                let name = v["name"].as_str().filter(|n| !n.is_empty()).map_or_else(|| tag.clone(), str::to_string);
                let trust: Trust = serde_json::from_value(v["trust"].clone()).unwrap_or(Trust::Unscanned);
                let install = (trust.installable() && v["source"] == "available")
                    .then(|| v["asset_url"].as_str())
                    .flatten()
                    .map(|url| RemoteVersion { id: tag.clone(), label: name.clone(), download: url.to_string(), sha256: v["sha256"].as_str().map(str::to_string) });
                Some(VersionLine {
                    name,
                    date: v["published_at"].as_str().unwrap_or_default().chars().take(10).collect(),
                    trust,
                    install,
                    downloads: v["downloads"].as_u64().unwrap_or_default(),
                    prerelease: v["prerelease"] == true,
                    findings: serde_json::from_value(v["findings"].clone()).unwrap_or_default(),
                    tag,
                })
            })
            .collect(),
    )
}

/// Comments the API sends on one page; it sends one more when older ones exist.
pub const COMMENTS_PER_PAGE: usize = 30;
/// Replies deeper than this are drawn at this depth.
pub const MAX_COMMENT_DEPTH: usize = 3;

/// One comment of an addon's discussion (`/v1/addons/{slug}/comments`), as the launcher shows it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Comment {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub author: String,
    pub avatar_url: String,
    /// `2026-11-01`
    pub date: String,
    pub edited: bool,
    pub deleted: bool,
    pub body: String,
    /// 0 for a comment of its own, up to `MAX_COMMENT_DEPTH` for replies.
    pub depth: usize,
}

/// A page of comments, newest first, and the id to ask older ones `before`, if any.
/// The logged-in user as `/v1/me` names them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Me {
    pub login: String,
    pub avatar_url: String,
}

pub fn parse_me(json: &str) -> Option<Me> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let login = v["login"].as_str().filter(|l| !l.is_empty())?.to_string();
    Some(Me { login, avatar_url: v["avatar_url"].as_str().unwrap_or_default().to_string() })
}

/// The body of `POST /v1/addons/{slug}/comments`; an empty text is refused before any request.
pub fn comment_body(text: &str, parent: Option<i64>) -> Result<serde_json::Value, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Write something first");
    }
    let mut body = serde_json::json!({ "body": text });
    if let Some(parent) = parent {
        body["parent_id"] = parent.into();
    }
    Ok(body)
}

pub fn comments_page(json: &str) -> Option<(Vec<Comment>, Option<i64>)> {
    let list: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    let comments: Vec<Comment> = list
        .iter()
        .take(COMMENTS_PER_PAGE)
        .map(|c| Comment {
            id: c["id"].as_i64().unwrap_or_default(),
            parent_id: c["parent_id"].as_i64(),
            author: text(&c["author"]),
            avatar_url: text(&c["avatar_url"]),
            date: text(&c["created_at"]).chars().take(10).collect(),
            edited: c["edited_at"].is_string(),
            deleted: c["deleted"] == true,
            body: text(&c["body"]),
            depth: 0,
        })
        .collect();
    let older = (list.len() > COMMENTS_PER_PAGE).then(|| comments.last().map(|c| c.id)).flatten();
    Some((comments, older))
}

/// Comments in reading order, as on the website: newest first, each followed by its replies,
/// oldest first. A reply whose comment is not among `comments` stands on its own.
pub fn comment_threads(comments: Vec<Comment>) -> Vec<Comment> {
    let ids: std::collections::HashSet<i64> = comments.iter().map(|c| c.id).collect();
    let mut replies: HashMap<i64, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for (i, c) in comments.iter().enumerate() {
        match c.parent_id.filter(|p| ids.contains(p)) {
            Some(parent) => replies.entry(parent).or_default().push(i),
            None => roots.push(i),
        }
    }
    let mut order = Vec::with_capacity(comments.len());
    let mut stack: Vec<(usize, usize)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        order.push((i, depth));
        if let Some(children) = replies.get(&comments[i].id) {
            stack.extend(children.iter().map(|&c| (c, depth + 1)));
        }
    }
    let mut slots: Vec<Option<Comment>> = comments.into_iter().map(Some).collect();
    order.into_iter().filter_map(|(i, depth)| slots[i].take().map(|c| Comment { depth: depth.min(MAX_COMMENT_DEPTH), ..c })).collect()
}

/// What a page that waits on the platform shows: nothing in the first 150 ms (a quick answer does
/// not flash a placeholder), placeholders, and from 5 s a word that the connection is slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waiting {
    Quiet,
    Placeholders,
    Slow,
}

pub const QUIET_FOR: std::time::Duration = std::time::Duration::from_millis(150);
pub const SLOW_AFTER: std::time::Duration = std::time::Duration::from_secs(5);
/// Small platform answers (comments, the issues frame) give up after this; downloads wait longer.
pub const QUICK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub fn waiting(elapsed: std::time::Duration) -> Waiting {
    if elapsed < QUIET_FOR {
        Waiting::Quiet
    } else if elapsed < SLOW_AFTER {
        Waiting::Placeholders
    } else {
        Waiting::Slow
    }
}

/// The version label of an update of a verified addon that the platform did not scan: found on
/// GitHub directly while the server could not be reached. Installing it needs the user's consent.
pub fn unchecked_update(state: &State, statuses: &HashMap<String, UpdateStatus>, id: &str) -> Option<String> {
    let verified = matches!(state.get(id).map(|e| &e.source), Some(Source::Verified { .. }));
    match statuses.get(id) {
        Some(UpdateStatus::UpdateAvailable(remote)) if verified && remote.sha256.is_none() => Some(remote.label.clone()),
        _ => None,
    }
}

/// What the platform last showed, kept across starts: icon URLs and the detail pages by addon id.
/// The pictures themselves sit in `cache/images`; with this the launcher finds them offline too.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlatformCache {
    pub icons: HashMap<String, String>,
    pub info: HashMap<String, AddonInfo>,
    /// Whether the platform takes bug reports, by addon id (`issues_ready`).
    #[serde(default)]
    pub issues_ready: HashMap<String, bool>,
}

impl PlatformCache {
    const FILE: &'static str = "platform.json";

    /// The stored cache in `dir`; empty when there is none or it cannot be read.
    pub fn load(dir: &std::path::Path) -> PlatformCache {
        std::fs::read(dir.join(Self::FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        crate::write_atomic(&dir.join(Self::FILE), &serde_json::to_vec(self).expect("cache serializes"))
    }
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

/// The updates to install on their own: of the addons switched on (`all`, or `picked` by entry
/// id), only versions the platform checked and found nothing in (scanned or verified, with the
/// hash it checked), and only those "Update all" would install without asking. A flagged
/// version waits for the user; a newer one that is not flagged goes again.
pub fn auto_update_ids(
    statuses: &HashMap<String, UpdateStatus>,
    registry: &Registry,
    state: &State,
    all: bool,
    picked: &[String],
    game: Option<&str>,
    games: &[GameVersion],
) -> Vec<String> {
    let checked = |id: &String| {
        let Some(Source::Verified { registry_id }) = state.get(id).map(|e| &e.source) else { return false };
        let clean = registry.get(registry_id).and_then(|a| a.latest.as_ref()).is_some_and(|l| matches!(l.trust, Trust::Scanned | Trust::Verified));
        clean && matches!(statuses.get(id), Some(UpdateStatus::UpdateAvailable(remote)) if remote.sha256.is_some())
    };
    let ids = update_all_ids(statuses, &[]).into_iter().filter(|id| all || picked.contains(id)).filter(checked).collect();
    split_confirmations(ids, registry, state, game, games).0
}

/// What keeps the quiet update check from starting now.
#[derive(Debug, Clone, Copy)]
pub struct CheckBlock {
    /// A check runs already.
    pub checking: bool,
    /// Installs or a game update run: a check would mark their rows "Checking...".
    pub busy: bool,
    pub offline: bool,
}

/// Whether the update check that runs on its own while the launcher is open is due: switched on,
/// `minutes` since the `last` check (by hand or on its own), and nothing in its way.
pub fn check_due(on: bool, minutes: u8, last: Option<std::time::Instant>, now: std::time::Instant, block: CheckBlock) -> bool {
    let Some(last) = last else { return false };
    on && !block.checking && !block.busy && !block.offline && now.duration_since(last) >= std::time::Duration::from_secs(u64::from(minutes) * 60)
}

/// The updates to install or count: all of `updates` but the ones of `disabled` addons while
/// their updates wait (`update_disabled` off).
pub fn without_waiting(updates: Vec<String>, disabled: &[String], update_disabled: bool) -> Vec<String> {
    if update_disabled {
        return updates;
    }
    updates.into_iter().filter(|id| !disabled.contains(id)).collect()
}

/// Whether the platform addon `id` is new since the launcher last closed: not among `seen`. On the
/// first start nothing was seen, and nothing is new.
pub fn is_new(id: &str, seen: &[String]) -> bool {
    !seen.is_empty() && !seen.iter().any(|s| s == id)
}

/// How many of the updates `after` a check were not there `before` it.
pub fn new_updates(before: &[String], after: &[String]) -> usize {
    after.iter().filter(|id| !before.contains(id)).count()
}

/// The addons "Update all" installs: those with an update, except the ones `held` at an older
/// version on purpose (they update only by hand).
pub fn update_all_ids(statuses: &HashMap<String, UpdateStatus>, held: &[String]) -> Vec<String> {
    let mut ids: Vec<String> = statuses
        .iter()
        .filter(|(id, s)| matches!(s, UpdateStatus::UpdateAvailable(_)) && !held.contains(id))
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

/// Where an addon's install or update stands while it runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Progress {
    /// "Update all" gets to it after the ones before it.
    Waiting,
    /// `total` is `None` when the server sends no size (GitHub's source zips).
    Downloading { done: u64, total: Option<u64> },
    /// The download is checked against the scanned file and put in place.
    Installing,
}

/// What a row says while it runs, and how full its bar is (`-1` for no bar).
pub fn progress_label(progress: Progress) -> (String, f32) {
    match progress {
        Progress::Waiting => ("Waiting".into(), -1.0),
        Progress::Downloading { done, total: Some(total) } if total > 0 => {
            let part = (done as f64 / total as f64).min(1.0);
            (format!("Downloading {}%", (part * 100.0).floor()), part as f32)
        }
        Progress::Downloading { done, .. } => (format!("Downloading {}", megabytes(done)), -1.0),
        Progress::Installing => ("Checking and installing".into(), 1.0),
    }
}

/// Bytes as megabytes with one decimal, the way the status texts show them.
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn progress_says_what_runs_and_how_far() {
        use super::{Progress, progress_label};
        assert_eq!(progress_label(Progress::Waiting), ("Waiting".to_string(), -1.0));
        assert_eq!(progress_label(Progress::Downloading { done: 450, total: Some(1000) }), ("Downloading 45%".to_string(), 0.45));
        assert_eq!(progress_label(Progress::Downloading { done: 2000, total: Some(1000) }).1, 1.0, "never over full");
        assert_eq!(progress_label(Progress::Downloading { done: 1_258_291, total: None }), ("Downloading 1.2 MB".to_string(), -1.0));
        assert_eq!(progress_label(Progress::Downloading { done: 0, total: Some(0) }).0, "Downloading 0.0 MB");
        assert_eq!(progress_label(Progress::Installing), ("Checking and installing".to_string(), 1.0));
    }

    use super::*;

    #[test]
    fn new_is_what_was_not_there_at_the_last_close() {
        let seen = vec!["lucky".to_string(), "plates".to_string()];
        assert!(is_new("dragon", &seen));
        assert!(!is_new("lucky", &seen));
        assert!(!is_new("dragon", &[]), "nothing is new on the first start");
    }

    #[test]
    fn updates_of_disabled_addons_wait_when_switched_off() {
        let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let disabled = ids(&["plates"]);
        assert_eq!(without_waiting(ids(&["lucky", "plates"]), &disabled, true), ids(&["lucky", "plates"]));
        assert_eq!(without_waiting(ids(&["lucky", "plates"]), &disabled, false), ids(&["lucky"]));
    }

    #[test]
    fn a_disabled_addon_says_so_in_its_row() {
        let state = State { entries: vec![crate::state::Entry {
            id: "lucky".into(),
            name: "Lucky".into(),
            source: Source::Zip { file_name: "l.zip".into() },
            folders: vec!["Lucky".into()],
            version: "zip:l.zip".into(),
            installed_at: 0,
        }] };
        let off = |folder: &str| InstalledAddon { folder: folder.into(), disabled: true, ..Default::default() };
        let scan = vec![off("Lucky"), off("Loose")];
        let reconciled = state.reconcile(&scan);
        assert!(reconciled.missing.is_empty(), "a disabled addon is not missing");
        let rows = installed_rows(&state, &reconciled, &scan, &crate::registry::embedded(), &HashMap::new());
        assert_eq!(rows.iter().map(|r| (r.id.as_str(), r.disabled)).collect::<Vec<_>>(), [("folder:Loose", true), ("lucky", true)]);
    }

    #[test]
    fn an_addon_needs_its_dependencies_first() {
        let registry = dep_registry();
        assert_eq!(required_addons("ui", &registry), ["core", "lib", "media"], "only required ones, deepest first");
        assert!(required_addons("core", &registry).is_empty());
    }

    #[test]
    fn a_failed_update_says_why_and_what_helps() {
        use crate::Error;
        let fix = |e: Error| fix_for(&e, false).1;
        assert_eq!(fix(Error::Network("timed out".into())), Fix::Retry);
        assert_eq!(fix(Error::Http(502)), Fix::Retry);
        assert_eq!(fix(Error::RateLimited), Fix::SignIn);
        assert_eq!(fix(Error::Io("access denied".into())), Fix::OpenFolder);
        assert_eq!(fix(Error::HashMismatch { expected: "a".into(), actual: "b".into() }), Fix::Details);
        assert_eq!(fix(Error::NotInstallable("blocked".into())), Fix::Details);
        assert_eq!(fix(Error::NoMatchingAsset), Fix::Details);
        let (why, fix) = fix_for(&Error::Io("access denied".into()), true);
        assert_eq!(fix, Fix::Retry, "a running game may hold the files");
        assert!(why.contains("Close the game"), "{why}");
        assert_eq!(fix_for(&Error::Network("timed out".into()), false).0, "Network error: timed out");
        assert_eq!([Fix::None, Fix::Retry, Fix::SignIn, Fix::OpenFolder, Fix::Details, Fix::Review].map(Fix::label), ["", "Retry", "Sign in", "Open folder", "Details", "Review"]);
    }

    #[test]
    fn release_notes_only_for_a_tag() {
        assert_eq!(notes_page("o/lucky", "v1.2").as_deref(), Some("https://github.com/o/lucky/releases/tag/v1.2"));
        assert_eq!(notes_page("o/lucky", "0123456789abcdef0123456789abcdef01234567"), None, "a commit has no release");
        assert_eq!(notes_page("o/lucky", ""), None);
    }

    #[test]
    fn the_quiet_check_waits_for_its_time_and_for_quiet() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let later = |min: u64| start + Duration::from_secs(min * 60);
        let free = CheckBlock { checking: false, busy: false, offline: false };
        assert!(!check_due(true, 30, Some(start), later(29), free), "not yet");
        assert!(check_due(true, 30, Some(start), later(30), free));
        assert!(!check_due(false, 30, Some(start), later(90), free), "switched off");
        assert!(!check_due(true, 30, None, later(90), free), "the first check is the one at the start");
        for block in [CheckBlock { checking: true, ..free }, CheckBlock { busy: true, ..free }, CheckBlock { offline: true, ..free }] {
            assert!(!check_due(true, 30, Some(start), later(90), block), "{block:?}");
        }
    }

    #[test]
    fn only_updates_found_since_the_last_check_are_news() {
        let before = vec!["a".to_string(), "b".to_string()];
        assert_eq!(new_updates(&before, &["b".to_string(), "c".to_string()]), 1);
        assert_eq!(new_updates(&before, &before), 0);
    }

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
    fn an_update_without_a_scan_of_a_verified_addon_is_unchecked() {
        let mut state = State::default();
        for (id, source) in [
            ("lucky", Source::Verified { registry_id: "lucky".into() }),
            ("mine", Source::GitHub { repo: "me/mine".into(), spec: crate::registry::SourceSpec::Tag }),
        ] {
            state.upsert(Entry { id: id.into(), name: id.into(), source, folders: vec![id.into()], version: "v1".into(), installed_at: 0 });
        }
        let update = |sha256: Option<String>| UpdateStatus::UpdateAvailable(RemoteVersion { id: "v2".into(), label: "v2".into(), download: String::new(), sha256 });
        let mut statuses = HashMap::from([("lucky".to_string(), update(None)), ("mine".to_string(), update(None))]);
        assert_eq!(unchecked_update(&state, &statuses, "lucky").as_deref(), Some("v2"));
        assert_eq!(unchecked_update(&state, &statuses, "mine"), None, "an own GitHub addon was never checked by the platform");
        statuses.insert("lucky".into(), update(Some("ab".repeat(32))));
        assert_eq!(unchecked_update(&state, &statuses, "lucky"), None, "a scanned version is checked");
        assert_eq!(unchecked_update(&state, &statuses, "gone"), None);
    }

    #[test]
    fn what_the_platform_showed_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(PlatformCache::load(dir.path()), PlatformCache::default(), "nothing stored yet");
        let mut cache = PlatformCache::default();
        cache.icons.insert("lucky".into(), "https://forge.test/img/i.webp".into());
        cache.info.insert("lucky".into(), AddonInfo { about: "# Hi".into(), icon: None, shots: vec![("https://forge.test/img/1.webp".into(), "Bars".into())], ..AddonInfo::default() });
        cache.save(dir.path()).unwrap();
        assert_eq!(PlatformCache::load(dir.path()), cache);
        std::fs::write(dir.path().join("platform.json"), "not json").unwrap();
        assert_eq!(PlatformCache::load(dir.path()), PlatformCache::default(), "a broken file is as good as none");
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
        InstalledAddon { folder: folder.into(), toc: Toc { title: folder.into(), ..Toc::default() }, disabled: false }
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

    /// An installed platform addon with an update to the platform's newest version, checked as `trust`.
    fn auto_case(trust: Trust) -> (Registry, State, HashMap<String, UpdateStatus>, String) {
        let mut registry = platform_registry(trust);
        registry.addons[0].verified_author = Some(false);
        registry.addons[0].latest.as_mut().unwrap().sha256 = Some("ab".repeat(32));
        let id = registry.addons[0].id.clone();
        let mut state = State::default();
        state.upsert(entry(&id, "Lucky", Source::Verified { registry_id: id.clone() }));
        state.upsert(entry("gh:o/r", "R", Source::GitHub { repo: "o/r".into(), spec: crate::registry::SourceSpec::Tag }));
        let mut statuses = HashMap::new();
        let remote = |sha256: Option<String>| UpdateStatus::UpdateAvailable(RemoteVersion { id: "v2".into(), label: "v2".into(), download: String::new(), sha256 });
        statuses.insert(id.clone(), remote(Some("ab".repeat(32))));
        statuses.insert("gh:o/r".to_string(), remote(None));
        (registry, state, statuses, id)
    }

    #[test]
    fn auto_updates_follow_the_setting() {
        let (registry, state, statuses, id) = auto_case(Trust::Scanned);
        let chosen = |all: bool, picked: &[String]| auto_update_ids(&statuses, &registry, &state, all, picked, None, &[]);
        assert!(chosen(false, &[]).is_empty(), "off by default");
        assert_eq!(chosen(false, std::slice::from_ref(&id)), [id.as_str()], "one addon switched on");
        assert_eq!(chosen(true, &[]), [id.as_str()], "all switched on");
    }

    #[test]
    fn auto_updates_skip_flagged_and_unchecked_versions() {
        for trust in [Trust::Flagged, Trust::Unscanned, Trust::Blocked, Trust::SourceChanged] {
            let (registry, state, statuses, _) = auto_case(trust);
            assert!(auto_update_ids(&statuses, &registry, &state, true, &[], None, &[]).is_empty(), "{trust:?}");
        }
        let (registry, state, statuses, id) = auto_case(Trust::Verified);
        assert_eq!(auto_update_ids(&statuses, &registry, &state, true, &[], None, &[]), [id.as_str()], "verified goes");
        // Found on GitHub while the server was away: not checked, so the user decides.
        let mut statuses = statuses;
        statuses.insert(id.clone(), UpdateStatus::UpdateAvailable(RemoteVersion { id: "v3".into(), label: "v3".into(), download: String::new(), sha256: None }));
        assert!(auto_update_ids(&statuses, &registry, &state, true, &[], None, &[]).is_empty());
    }

    #[test]
    fn auto_updates_leave_updates_that_ask_first() {
        let (mut registry, state, statuses, _) = auto_case(Trust::Scanned);
        registry.addons[0].latest.as_mut().unwrap().game_max = Some("0.68".into());
        let games = [GameVersion { name: "0.68".into(), sort_key: 1 }, GameVersion { name: "0.69".into(), sort_key: 2 }];
        assert!(auto_update_ids(&statuses, &registry, &state, true, &[], Some("0.69"), &games).is_empty(), "a compatibility warning needs the user");
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
    fn an_older_version_warns_like_the_newest() {
        let remote = RemoteVersion { id: "v1".into(), label: "v1".into(), download: String::new(), sha256: None };
        let line = VersionLine {
            tag: "v1".into(),
            name: "v1".into(),
            date: String::new(),
            trust: Trust::Flagged,
            downloads: 0,
            prerelease: false,
            install: Some(remote),
            findings: forge_core::registry::FindingCounts { critical: 1, warn: 2 },
        };
        assert_eq!(version_warnings(&line), [Warning::NoHash, Warning::Flagged { critical: 1, warn: 2 }]);
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
        assert_eq!(update_all_ids(&statuses, &[]), ["a", "b"]);
        assert_eq!(update_all_ids(&statuses, &["a".to_string()]), ["b"], "a version kept on purpose stays");
    }

    #[test]
    fn quitting_asks_only_while_work_runs() {
        assert_eq!(quit_warning(false, false, 0), None);
        assert!(quit_warning(true, false, 2).unwrap().starts_with("The game update"), "the game update matters most");
        assert!(quit_warning(false, true, 0).unwrap().contains("updating itself"));
        assert_eq!(quit_warning(false, false, 1).unwrap(), "1 addon install is still running. Quitting now stops it.");
        assert!(quit_warning(false, false, 3).unwrap().starts_with("3 addon installs are"));
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
                published_at: None,
            });
        }
        registry
    }

    #[test]
    fn lists_sort_and_filter_as_on_the_website() {
        fn key(name: &'static str, downloads: Option<i64>, updated: Option<&'static str>) -> SortKey<'static> {
            SortKey { name, downloads, updated }
        }
        fn names(rows: &[SortKey<'static>]) -> Vec<&'static str> {
            rows.iter().map(|k| k.name).collect()
        }
        let mut rows = vec![key("Zeta", Some(5), Some("2026-10-01")), key("alpha", None, None), key("Beta", Some(50), Some("2026-09-01")), key("Gamma", Some(5), None)];
        rows.sort_by(|a, b| compare(Sort::Downloads, a, b));
        assert_eq!(names(&rows), ["Beta", "Gamma", "Zeta", "alpha"], "most downloads first, ties and unknown by name");
        rows.sort_by(|a, b| compare(Sort::Updated, a, b));
        assert_eq!(names(&rows), ["Zeta", "Beta", "alpha", "Gamma"], "newest release first, unknown last");
        rows.sort_by(|a, b| compare(Sort::Name, a, b));
        assert_eq!(names(&rows), ["alpha", "Beta", "Gamma", "Zeta"], "ignoring case");
        assert_eq!(Sort::from_index(0), Sort::Downloads, "the default, as on the website");
        assert_eq!(SORT_LABELS, ["Most downloads", "Recently updated", "Name"]);

        assert_eq!(SAFETY_LABELS, ["Any safety check", "Verified", "Checked", "Flagged", "Not checked"]);
        assert_eq!(safety_filter(0), None);
        assert!(passes_safety(safety_filter(2), Some(Trust::Scanned)));
        assert!(!passes_safety(safety_filter(1), Some(Trust::Scanned)));
        assert!(!passes_safety(safety_filter(3), None), "a zip or GitHub addon has no safety check");
        assert!(passes_safety(None, None));
    }

    #[test]
    fn badge_per_trust() {
        // The words and colours themselves are forge-core's, shared with the website.
        let rows = browse_rows(&platform_registry(Trust::Flagged), &State::default());
        assert!(rows.iter().all(|r| r.trust == Some(("Flagged", Tone::Bad))));
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
            published_at: None,
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
    fn installed_lists_the_updates_first_in_the_chosen_order() {
        let row = |id: &str, state: RowState| Row {
            id: id.into(),
            name: id.into(),
            author: String::new(),
            version: String::new(),
            badge: Badge::Unmanaged,
            state,
            description: String::new(),
            repo: None,
            out_of_date: false,
            folders: Vec::new(),
            disabled: false,
        };
        let mut rows = [
            row("a", RowState::UpToDate),
            row("b", RowState::UpdateAvailable("v2".into())),
            row("c", RowState::UpToDate),
            row("d", RowState::UpdateAvailable("v3".into())),
            row("kept", RowState::UpdateAvailable("v4".into())),
        ];
        updates_first(&mut rows, &["kept".to_string()]);
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["b", "d", "a", "c", "kept"], "updates on top, the rest keeps its order, a kept one waits");
    }

    #[test]
    fn a_version_links_to_its_release_notes_on_github() {
        assert_eq!(release_page("bob/lucky", "v1.2"), "https://github.com/bob/lucky/releases/tag/v1.2");
        assert_eq!(release_page("bob/lucky", "build/1 #2"), "https://github.com/bob/lucky/releases/tag/build%2F1%20%232");
    }

    #[test]
    fn a_running_game_gets_the_change_on_its_next_start() {
        assert_eq!(note_running_game("Installed Foo v1".into(), false), "Installed Foo v1");
        assert_eq!(note_running_game("Installed Foo v1".into(), true), "Installed Foo v1. It takes effect the next time the game starts.");
    }

    #[test]
    fn removing_names_the_installed_addons_that_need_it() {
        let registry = dep_registry();
        let mut state = State::default();
        for id in ["media", "lib", "extras"] {
            state.upsert(crate::state::Entry {
                id: id.into(),
                name: id.to_uppercase(),
                source: Source::Verified { registry_id: id.into() },
                folders: vec![id.to_uppercase()],
                version: "v1".into(),
                installed_at: 0,
            });
        }
        assert_eq!(dependents("lib", &registry, &state), ["MEDIA"], "ui needs lib too but is not installed");
        assert!(dependents("extras", &registry, &state).is_empty(), "ui needs extras only optionally");
        assert!(dependents("media", &registry, &state).is_empty());
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
            published_at: None,
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

    #[test]
    fn bugs_go_to_the_platform_only_when_everything_works() {
        let here = ("Report a bug", "https://w/addons/lucky/issues/new?kind=bug".to_string());
        let github = ("Report on GitHub", "https://github.com/alice/lucky/issues/new".to_string());
        assert_eq!(bug_report("https://w", "lucky", "alice/lucky", true, Some(true)), here);
        assert_eq!(bug_report("https://w", "lucky", "alice/lucky", true, Some(false)), github, "the App may not write issues");
        assert_eq!(bug_report("https://w", "lucky", "alice/lucky", true, None), github, "not known yet, or offline");
        assert_eq!(bug_report("https://w", "lucky", "alice/lucky", false, Some(true)), github, "the Issues tab is off");
        assert_eq!(issue_frame(r#"{"issues_here":true,"ready":true}"#), Some((true, true)));
        assert_eq!(issue_frame(r#"{"issues_here":true,"ready":false}"#), Some((true, false)), "the App may not write issues");
        assert_eq!(issue_frame(r#"{"issues_here":false,"ready":false}"#), Some((false, false)));
        assert_eq!(issue_frame("not json"), None);
    }

    #[test]
    fn the_issues_tab_reads_the_open_issues() {
        let json = r#"{"issues_here":true,"counts":{"open":1,"closed":9,"all":10},"issues":[
            {"number":69,"title":"Suggested improvements","author":{"login":"bob"},"comments":2,"created_at":"2026-10-30T05:24:16Z","kind":"other","status":"open"}]}"#;
        let (issues, open) = issue_lines(json).unwrap();
        assert_eq!(open, 1);
        assert_eq!(issues, [IssueLine { number: 69, title: "Suggested improvements".into(), author: "bob".into(), comments: 2, date: "2026-10-30".into() }]);
        assert_eq!(issue_lines(r#"{"issues_here":false}"#), Some((Vec::new(), 0)), "the tab is off: nothing to list");
        assert_eq!(issue_lines("not json"), None);
    }

    #[test]
    fn the_versions_tab_reads_every_version_newest_first() {
        let json = r#"[
            {"tag":"v2","name":"Lucky 2","published_at":"2026-11-04T12:41:27+00:00","trust":"scanned","downloads":278,"prerelease":false,"source":"available","asset_url":"https://g/v2.zip","sha256":"ab"},
            {"tag":"v1","name":"","published_at":"2026-10-01T10:00:00+00:00","trust":"blocked","downloads":5,"prerelease":true,"source":"available","asset_url":"https://g/v1.zip","sha256":"cd"},
            {"tag":"v0","name":"","published_at":"2026-09-01T10:00:00+00:00","trust":"verified","downloads":1,"prerelease":false,"source":"deleted","asset_url":"https://g/v0.zip","sha256":"ef"}]"#;
        let versions = version_lines(json).unwrap();
        let remote = RemoteVersion { id: "v2".into(), label: "Lucky 2".into(), download: "https://g/v2.zip".into(), sha256: Some("ab".into()) };
        assert_eq!(versions[0], VersionLine { tag: "v2".into(), name: "Lucky 2".into(), date: "2026-11-04".into(), trust: Trust::Scanned, downloads: 278, prerelease: false, install: Some(remote), findings: Default::default() });
        assert_eq!((versions[1].name.as_str(), versions[1].trust), ("v1", Trust::Blocked), "without a name the tag stands in");
        assert!(versions[1].install.is_none(), "a blocked version cannot be installed");
        assert!(versions[2].install.is_none(), "nor one whose file is gone from GitHub");
        assert_eq!(version_title("Lucky", "Lucky 2"), "Lucky 2", "the name is not said twice");
        assert_eq!(version_title("Lucky", "v1"), "Lucky v1");
        assert_eq!(version_lines("{}"), None);
    }

    #[test]
    fn the_detail_view_links_the_addon_pages_on_the_website() {
        assert_eq!(addon_link("https://w", "lucky", "issues/69").as_deref(), Some("https://w/addons/lucky/issues/69"));
        assert_eq!(addon_link("https://w", "lucky", "issues/x"), None, "only issue numbers");
        assert_eq!(addon_link("https://w", "lucky", "website").as_deref(), Some("https://w/addons/lucky"));
        assert_eq!(addon_link("https://w", "lucky", "issues").as_deref(), Some("https://w/addons/lucky/issues"));
        assert_eq!(addon_link("https://w", "lucky", "comments").as_deref(), Some("https://w/addons/lucky/comments"));
        assert_eq!(addon_link("https://w", "lucky", "abuse").as_deref(), Some("https://w/addons/lucky/report"));
        assert_eq!(addon_link("https://w", "lucky", "elsewhere"), None);
        let info = addon_info(r#"{"description_md":"","issues_enabled":true,"comments_enabled":false}"#).unwrap();
        assert_eq!((info.issues_on, info.comments_on), (true, false));
        let old: AddonInfo = serde_json::from_str(r#"{"about":"","icon":null,"shots":[]}"#).unwrap();
        assert_eq!((old.issues_on, old.comments_on), (false, false), "a cache from before reads as off");
    }

    fn comment(id: i64, parent: Option<i64>) -> Comment {
        Comment { id, parent_id: parent, author: format!("u{id}"), body: format!("text {id}"), ..Comment::default() }
    }

    #[test]
    fn issue_body_needs_kind_and_title() {
        assert_eq!(issue_body("bug", " Crash on start ", " It crashes.\n"), Ok(serde_json::json!({ "kind": "bug", "title": "Crash on start", "body": "It crashes." })));
        assert_eq!(issue_body("", "t", "b"), Err("Choose a bug report, a feature request or a question"));
        assert_eq!(issue_body("rant", "t", "b"), Err("Choose a bug report, a feature request or a question"));
        assert_eq!(issue_body("question", "  ", "b"), Err("Give the issue a title"));
        assert_eq!(issue_body("feature", "t", " "), Err("Describe the issue"));
    }

    #[test]
    fn issue_page_reads_issue_and_replies() {
        let json = r#"{"issues_here":true,"issue":{"number":12,"title":"Crash","state":"open","locked":false,"author":{"login":"kex","avatar_url":""},"created_at":"2026-11-01T10:00:00Z","body":"It crashes."},
            "replies":[{"id":1,"author":{"login":"octo","avatar_url":""},"body":"Fixed in v2","created_at":"2026-11-02T08:00:00Z"}]}"#;
        let page = issue_page(json).unwrap();
        assert_eq!((page.number, page.title.as_str(), page.state.as_str(), page.author.as_str(), page.date.as_str()), (12, "Crash", "open", "kex", "2026-11-01"));
        assert_eq!(page.body, "It crashes.");
        assert_eq!(page.replies, vec![IssueReply { author: "octo".into(), date: "2026-11-02".into(), body: "Fixed in v2".into() }]);
        assert_eq!(issue_page(r#"{"error":"not_found"}"#), None);
    }

    #[test]
    fn comment_body_with_and_without_parent() {
        assert_eq!(comment_body("  Nice addon 
", None), Ok(serde_json::json!({ "body": "Nice addon" })));
        assert_eq!(comment_body("Thanks", Some(7)), Ok(serde_json::json!({ "body": "Thanks", "parent_id": 7 })));
    }

    #[test]
    fn empty_comment_is_refused() {
        assert_eq!(comment_body("", None), Err("Write something first"));
        assert_eq!(comment_body(" 
	 ", Some(3)), Err("Write something first"));
    }

    #[test]
    fn me_answer_gives_login_and_avatar() {
        let me = parse_me(r#"{"id":1,"github_id":9,"login":"octo","avatar_url":"https://avatars.test/9","role":"user"}"#);
        assert_eq!(me, Some(Me { login: "octo".into(), avatar_url: "https://avatars.test/9".into() }));
        assert_eq!(parse_me(r#"{"error":"unauthorized"}"#), None);
        assert_eq!(parse_me("not json"), None);
    }

    #[test]
    fn comments_read_newest_first_with_replies_below() {
        let page = vec![comment(6, Some(1)), comment(5, None), comment(4, Some(2)), comment(3, Some(1)), comment(2, Some(1)), comment(1, None)];
        let shape: Vec<(i64, usize)> = comment_threads(page).iter().map(|c| (c.id, c.depth)).collect();
        assert_eq!(shape, [(5, 0), (1, 0), (2, 1), (4, 2), (3, 1), (6, 1)]);
        let orphan: Vec<(i64, usize)> = comment_threads(vec![comment(9, Some(1)), comment(8, None)]).iter().map(|c| (c.id, c.depth)).collect();
        assert_eq!(orphan, [(9, 0), (8, 0)], "a reply to an older page stands on its own");
        let chain: Vec<Comment> = (1..=6).rev().map(|id| comment(id, (id > 1).then(|| id - 1))).collect();
        assert_eq!(comment_threads(chain).iter().map(|c| c.depth).max(), Some(MAX_COMMENT_DEPTH));
    }

    #[test]
    fn a_page_of_comments_says_whether_older_ones_exist() {
        let one = r#"[{"id":3,"deleted":false,"body":"Hi","author":"bob","avatar_url":"https://a/1","created_at":"2026-11-01T10:00:00Z","edited_at":"2026-11-02T10:00:00Z","parent_id":null},
                      {"id":2,"deleted":true,"body":"","author":"eve","avatar_url":"","created_at":"2026-10-31T10:00:00Z","edited_at":null,"parent_id":3}]"#;
        let (comments, older) = comments_page(one).unwrap();
        assert_eq!(older, None);
        assert_eq!((comments[0].author.as_str(), comments[0].date.as_str(), comments[0].edited), ("bob", "2026-11-01", true));
        assert_eq!((comments[1].deleted, comments[1].parent_id), (true, Some(3)));
        let full: Vec<serde_json::Value> = (1..=31).rev().map(|id| serde_json::json!({ "id": id, "body": "x" })).collect();
        let (comments, older) = comments_page(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!((comments.len(), older), (COMMENTS_PER_PAGE, Some(2)), "the extra one only shows that there are more");
        assert!(comments_page("{}").is_none());
    }

    #[test]
    fn waiting_shows_placeholders_after_150_ms_and_a_word_after_5_s() {
        use std::time::Duration;
        assert_eq!(waiting(Duration::from_millis(100)), Waiting::Quiet);
        assert_eq!(waiting(Duration::from_millis(150)), Waiting::Placeholders);
        assert_eq!(waiting(Duration::from_millis(4_999)), Waiting::Placeholders);
        assert_eq!(waiting(Duration::from_secs(5)), Waiting::Slow);
    }
}
