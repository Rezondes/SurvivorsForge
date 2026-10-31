//! Askama templates. Every variable is HTML-escaped; the only exception is an addon's
//! `description_html`, which the API sanitized.

use askama::Template;
use axum::response::Html;
pub use forge_core::roles::Permission;
use serde_json::Value;

/// The logged-in visitor as the API describes them (`/v1/me`).
#[derive(Debug, Clone, Default)]
pub struct Visitor {
    pub login: String,
    pub avatar_url: String,
    pub role: String,
    /// What the API lets this user do (`forge_core::roles`), e.g. `moderate`.
    pub permissions: Vec<String>,
    /// Moderators and admins: they see the admin pages.
    pub staff: bool,
}

impl Visitor {
    pub fn from_me(me: &Value) -> Visitor {
        let permissions: Vec<String> = me["permissions"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect();
        Visitor {
            login: me["login"].as_str().unwrap_or_default().to_string(),
            avatar_url: me["avatar_url"].as_str().unwrap_or_default().to_string(),
            role: me["role"].as_str().unwrap_or("user").to_string(),
            staff: permissions.iter().any(|p| p == Permission::Moderate.as_str()),
            permissions,
        }
    }

    pub fn can(&self, permission: Permission) -> bool {
        self.permissions.iter().any(|p| p == permission.as_str())
    }
}

pub fn render(template: &impl Template) -> Html<String> {
    Html(template.render().unwrap_or_else(|e| {
        tracing::error!(error = %e, "template failed");
        "Internal error".into()
    }))
}

#[derive(Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    pub title: String,
    pub message: String,
    pub user: Option<Visitor>,
    pub csrf: String,
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// The most downloaded addons; empty when the API cannot tell.
    pub popular: Vec<Card>,
}

pub use crate::trust::Notice;

/// A category as lists and selects show it: subcategories read "Classes: Warrior".
#[derive(Debug, Clone, PartialEq)]
pub struct CategoryOption {
    pub name: String,
    pub label: String,
    pub parent: String,
    pub sort_key: String,
    /// Addons can go into it (it has no subcategories).
    pub selectable: bool,
}

impl CategoryOption {
    /// The API's `/v1/categories`, in its order.
    pub fn list(api: &Value) -> Vec<CategoryOption> {
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        api.as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                let (name, parent) = (text(&c["name"]), text(&c["parent"]));
                CategoryOption {
                    label: if parent.is_empty() { name.clone() } else { format!("{parent}: {name}") },
                    sort_key: c["sort_key"].as_i64().map(|k| k.to_string()).unwrap_or_default(),
                    selectable: c["selectable"] != false,
                    name,
                    parent,
                }
            })
            .collect()
    }

    /// The top-level categories, each with its subcategories (empty for a plain one), in order.
    pub fn groups(list: Vec<CategoryOption>) -> Vec<(CategoryOption, Vec<CategoryOption>)> {
        let mut groups: Vec<(CategoryOption, Vec<CategoryOption>)> = Vec::new();
        for c in list {
            match groups.iter_mut().find(|(top, _)| !c.parent.is_empty() && top.name.eq_ignore_ascii_case(&c.parent)) {
                Some((_, children)) => children.push(c),
                None => groups.push((c, Vec::new())),
            }
        }
        groups
    }
}

pub struct Card {
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub icon_url: Option<String>,
    pub category: String,
    pub notice: Notice,
    /// Grouped like `24,365`; `None` for addons from tags: GitHub counts only release downloads.
    pub downloads: Option<String>,
    /// `YYYY-MM-DD`, shown where there is no download count.
    pub updated: String,
}

#[derive(Template)]
#[template(path = "stats.html")]
pub struct StatsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub website: Vec<crate::stats::Stat>,
    pub launcher: Vec<crate::stats::Stat>,
    pub addons: Vec<crate::stats::Stat>,
    pub platform: Vec<crate::stats::Stat>,
    pub top: Vec<crate::stats::TopAddon>,
    pub checks: crate::stats::Checks,
}

#[derive(Template)]
#[template(path = "addons.html")]
pub struct AddonsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// When the releases were checked; `None` when the API cannot tell.
    pub checks: Option<crate::stats::Checks>,
    pub q: String,
    pub category: String,
    pub trust: String,
    pub sort: String,
    pub total: i64,
    /// For the filter, all of them.
    /// Top-level categories with their subcategories, for the grouped filter.
    pub categories: Vec<(CategoryOption, Vec<CategoryOption>)>,
    pub cards: Vec<Card>,
    pub prev: Option<String>,
    pub next: Option<String>,
}

pub struct FindingRow {
    pub severity: String,
    pub rule: String,
    pub place: String,
    pub message: String,
    pub snippet: String,
}

pub struct VersionRow {
    pub tag: String,
    /// The tag cut to 18 characters and `…` when it is longer (commit hashes), for headings.
    pub tag_short: String,
    pub name: String,
    pub published: String,
    pub prerelease: bool,
    pub deleted: bool,
    pub notice: Notice,
    pub sha256: String,
    pub asset_url: String,
    pub game: String,
    pub critical: i64,
    pub warn: i64,
    /// Grouped like `1,234`; `None` when GitHub counted nothing (versions from tags have no count).
    pub downloads: Option<String>,
}

pub struct Shot {
    pub url: String,
    pub caption: String,
    pub width: i64,
    pub height: i64,
}

pub struct Dep {
    pub slug: String,
    pub name: String,
    pub required: bool,
    pub available: bool,
    pub notice: Notice,
}

pub struct LatestBlock {
    pub row: VersionRow,
    /// GitHub directly, or the confirmation page for risky versions.
    pub href: String,
    pub findings: Vec<FindingRow>,
}

#[derive(Template)]
#[template(path = "addon.html")]
pub struct AddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub checks: Option<crate::stats::Checks>,
    pub canonical: String,
    pub slug: String,
    pub name: String,
    pub summary: String,
    pub author: String,
    pub claimed: bool,
    pub category: String,
    pub repo: String,
    /// Sanitized by the API (forge_core::markdown); the one value rendered unescaped.
    pub description_html: String,
    pub icon_url: Option<String>,
    pub shots: Vec<Shot>,
    pub contributors: Vec<String>,
    pub deps: Vec<Dep>,
    pub latest: Option<LatestBlock>,
    /// Unclaimed, and the repo belongs to the visitor's GitHub account.
    pub can_claim: bool,
    pub install_url: String,
    /// Why a claim failed.
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "blocked.html")]
pub struct BlockedPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub name: String,
    pub reason: String,
}

#[derive(Template)]
#[template(path = "versions.html")]
pub struct VersionsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    /// (download link, version)
    pub rows: Vec<(String, VersionRow)>,
    /// What the visitor may edit; `None` shows no edit toolbar.
    pub perms: Option<Perms>,
}

#[derive(Template)]
#[template(path = "version.html")]
pub struct VersionPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub row: VersionRow,
    pub href: String,
    pub findings: Vec<FindingRow>,
}

#[derive(Template)]
#[template(path = "download.html")]
pub struct DownloadPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub row: VersionRow,
    pub findings: Vec<FindingRow>,
}

/// Form errors by field name; `form` holds the ones that belong to no field.
#[derive(Debug, Default)]
pub struct Errors(pub std::collections::HashMap<String, String>);

impl Errors {
    pub fn get(&self, field: &str) -> &str {
        self.0.get(field).map(String::as_str).unwrap_or_default()
    }
}

/// What the visitor may change on one addon.
#[derive(Debug, Clone, Copy, Default)]
pub struct Perms {
    pub owner: bool,
    pub edit_description: bool,
    pub edit_images: bool,
}

pub struct MyAddon {
    pub slug: String,
    pub name: String,
    pub role: String,
    pub notice: Notice,
    pub has_version: bool,
    pub findings: i64,
    pub blocked: bool,
    pub locked: bool,
}

#[derive(Template)]
#[template(path = "creator/scan.html")]
pub struct ScanPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub result: Option<ScanResult>,
    pub error: Option<String>,
}

/// A test scan of an uploaded zip, findings most severe first.
pub struct ScanResult {
    pub file_name: String,
    pub flagged: bool,
    pub folders: Vec<String>,
    pub scanner_version: String,
    pub findings: Vec<FindingRow>,
}

#[derive(Template)]
#[template(path = "creator/my_addons.html")]
pub struct MyAddonsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// Owned, shared and `claimable` (added by an admin from a repo of the visitor) addons.
    pub rows: Vec<MyAddon>,
    /// Some row can be claimed: the page explains what a claim checks.
    pub claimable: bool,
    pub install_url: String,
    pub msg: Option<String>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "creator/new.html")]
pub struct NewAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub install_url: String,
    pub repo: RepoField,
    /// What the form was filled in from, or why it could not be.
    pub filled: Option<Result<String, String>>,
    pub categories: Vec<CategoryOption>,
    pub form: crate::creator::NewAddonForm,
    pub errors: Errors,
}

/// The repo input of the publish forms: the account (fixed for creators, editable for admins)
/// and the repo name, which suggests matching public repos while typing. `page` is `admin` on
/// the admin form, so a pick leads back there.
pub struct RepoField {
    pub owner: String,
    pub name: String,
    pub editable: bool,
    /// The creator's account and public organizations to choose from (admins type any).
    pub accounts: Vec<String>,
    /// Where to install the App, which repos of an organization need.
    pub install_url: String,
    pub page: String,
    /// Picked from the suggestions: offer to fill in the form from it.
    pub picked: bool,
    /// (full name, description).
    pub suggestions: Vec<(String, String)>,
    pub error: String,
}

/// The repo field alone, for htmx when a suggestion is picked.
#[derive(Template)]
#[template(path = "creator/repo_field_fragment.html")]
pub struct RepoFieldFragment {
    pub repo: RepoField,
}

/// The suggestions alone, for htmx while the name is typed.
#[derive(Template)]
#[template(path = "creator/repo_suggestions_fragment.html")]
pub struct RepoSuggestionsFragment {
    pub repo: RepoField,
}

#[derive(Template)]
#[template(path = "creator/edit.html")]
pub struct EditAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub category: String,
    pub categories: Vec<CategoryOption>,
    pub source_type: String,
    pub asset_pattern: String,
    pub folders: String,
    pub summary: String,
    pub description: String,
    /// The preview, sanitized by forge_core::markdown like the API does it.
    pub description_html: String,
    pub perms: Perms,
    pub errors: Errors,
    pub saved: bool,
}

pub struct ImageRow {
    pub id: i64,
    pub url: String,
    pub caption: String,
}

#[derive(Template)]
#[template(path = "creator/images.html")]
pub struct ImagesPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    /// `icon` or `screenshots`.
    pub tab: String,
    pub icon_url: Option<String>,
    pub images: Vec<ImageRow>,
    pub message: Option<String>,
    pub error: Option<String>,
    pub perms: Perms,
}

pub struct MemberRow {
    pub github_id: i64,
    pub login: String,
    pub description: bool,
    pub images: bool,
}

#[derive(Template)]
#[template(path = "creator/members.html")]
pub struct MembersPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub members: Vec<MemberRow>,
    pub error: Option<String>,
    pub perms: Perms,
}

#[derive(Template)]
#[template(path = "creator/dependencies.html")]
pub struct DepsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    /// (slug, name, required)
    pub deps: Vec<(String, String, bool)>,
    pub error: Option<String>,
    pub perms: Perms,
}

#[derive(Template)]
#[template(path = "creator/version.html")]
pub struct VersionEditPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub tag: String,
    pub game_min: String,
    pub game_max: String,
    pub games: Vec<String>,
    pub error: Option<String>,
    pub perms: Perms,
}

#[derive(Template)]
#[template(path = "creator/delete.html")]
pub struct DeleteAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub error: Option<String>,
    pub perms: Perms,
}

#[derive(Template)]
#[template(path = "admin/index.html")]
pub struct AdminIndexPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub queue: usize,
    pub reports: usize,
    /// The viewer may open `/admin/stats` (`ADMIN_STATS_ROLES`).
    pub stats: bool,
    /// The viewer may open Grafana (admins).
    pub grafana: bool,
}

/// A titled table of `/admin/stats`; the first column is text, the others are numbers.
pub struct StatTable {
    pub title: String,
    pub head: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// Shown instead of an empty table.
    pub empty: String,
}

#[derive(Template)]
#[template(path = "admin/stats.html")]
pub struct AdminStatsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// `24h`, `7d` or `30d`.
    pub range: String,
    /// Where the range starts, e.g. `2026-10-30 10:00 UTC`.
    pub since: String,
    pub totals: Vec<crate::stats::Stat>,
    /// Problems shown first; empty when all is well.
    pub attention: Vec<String>,
    pub groups: Vec<StatGroup>,
}

/// A section of `/admin/stats` with its jump anchor.
pub struct StatGroup {
    pub id: &'static str,
    pub title: &'static str,
    pub tables: Vec<StatTable>,
}

pub struct ReviewItem {
    pub version_id: i64,
    pub slug: String,
    pub name: String,
    pub tag: String,
    pub trust: String,
    pub compare_url: Option<String>,
    pub findings: Vec<FindingRow>,
}

#[derive(Template)]
#[template(path = "admin/review.html")]
pub struct AdminReviewPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub addons: Vec<ReviewAddon>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

/// An addon on the review overview: how many versions wait and their trust levels.
pub struct ReviewAddon {
    pub slug: String,
    pub name: String,
    pub versions: usize,
    pub trusts: Vec<String>,
}

#[derive(Template)]
#[template(path = "admin/review_addon.html")]
pub struct AdminReviewAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub items: Vec<ReviewItem>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

pub struct ReportRow {
    pub id: i64,
    pub target: String,
    pub addon: String,
    pub reason: String,
    pub text: String,
    pub reporter: String,
    pub created: String,
}

#[derive(Template)]
#[template(path = "admin/reports.html")]
pub struct AdminReportsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub rows: Vec<ReportRow>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

pub struct UserRow {
    pub github_id: i64,
    pub login: String,
    pub role: String,
    pub banned: bool,
    pub ban_reason: String,
    pub addons: i64,
}

#[derive(Template)]
#[template(path = "admin/users.html")]
pub struct AdminUsersPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub admins: Vec<AdminRow>,
    /// The viewer may give roles and ban moderators.
    pub manage_roles: bool,
    pub login: String,
    pub found: Option<UserRow>,
    pub not_found: bool,
    pub msg: Option<String>,
    pub error: Option<String>,
}

pub struct AdminRow {
    pub github_id: i64,
    pub login: String,
    pub role: String,
    /// From ADMIN_GITHUB_IDS: only the configuration can take it away.
    pub by_config: bool,
}

#[derive(Template)]
#[template(path = "admin/addon.html")]
pub struct AdminAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// Newest first.
    pub versions: Vec<AdminVersionRow>,
    pub slug: String,
    pub name: String,
    pub owner: String,
    pub blocked: bool,
    pub block_reason: String,
    pub msg: Option<String>,
    pub error: Option<String>,
}

pub struct AdminVersionRow {
    pub id: i64,
    pub tag: String,
    pub published: String,
    pub trust: String,
    pub critical: i64,
    pub warn: i64,
}

#[derive(Template)]
#[template(path = "admin/addons.html")]
pub struct AdminAddonsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub q: String,
    /// (slug, name, trust of the current version)
    pub found: Vec<(String, String, String)>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/add_addon.html")]
pub struct AdminAddAddonPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub repo: RepoField,
    /// What the form was filled in from, or why it could not be.
    pub filled: Option<Result<String, String>>,
    pub categories: Vec<CategoryOption>,
    pub form: crate::admin::AdminAddonForm,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "admin/list.html")]
pub struct AdminListPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// `game-versions` or `categories`.
    pub kind: String,
    pub title: String,
    /// (key, name, sort key, parent); the parent only for categories.
    pub rows: Vec<(String, String, String, String)>,
    /// Categories a new or moved category can sit under.
    pub parents: Vec<String>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

pub struct AuditRow {
    pub when: String,
    pub admin: String,
    pub action: String,
    pub target: String,
    pub note: String,
}

#[derive(Template)]
#[template(path = "admin/audit.html")]
pub struct AdminAuditPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub filter_admin: String,
    pub filter_action: String,
    pub filter_target_type: String,
    pub filter_target_id: String,
    pub entries: Vec<AuditRow>,
    pub prev: Option<String>,
    pub next: Option<String>,
}

/// An addon in the pack picker: its current version.
pub struct PackChoice {
    pub slug: String,
    pub name: String,
    pub tag: String,
    /// Empty when the release size is unknown (source zips).
    pub size: String,
    pub notice: Notice,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "packs/new.html")]
pub struct NewPackPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub name: String,
    pub choices: Vec<PackChoice>,
    pub error: Option<String>,
}

pub struct PackEntry {
    pub slug: String,
    pub name: String,
    pub tag: String,
    pub available: bool,
    pub notice: Notice,
    pub folders: String,
    /// Follows the addon's newest checked version.
    pub latest: bool,
    /// Ticked to stay on the edit page; false after a refused save that took it out.
    pub kept: bool,
    /// Tags that can go into a pack, newest first.
    pub versions: Vec<String>,
}

#[derive(Template)]
#[template(path = "packs/edit.html")]
pub struct PackEditPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub id: String,
    pub name: String,
    pub listed: bool,
    pub entries: Vec<PackEntry>,
    /// Packable addons not in the pack yet.
    pub others: Vec<PackChoice>,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "packs/show.html")]
pub struct PackPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub id: String,
    /// The full address to share.
    pub link: String,
    pub name: String,
    pub created_by: String,
    pub created: String,
    pub available: bool,
    pub size: Option<String>,
    pub entries: Vec<PackEntry>,
    /// The picker with the addons of this pack that are still available.
    pub rebuild: String,
    /// The visitor created the pack or is an admin.
    pub can_edit: bool,
    /// Set by the action that redirected here.
    pub msg: Option<String>,
}

pub struct PackListRow {
    pub id: String,
    pub name: String,
    pub created_by: String,
    pub created: String,
    /// Addon names, comma-separated.
    pub addons: String,
}

#[derive(Template)]
#[template(path = "packs/list.html")]
pub struct PacksPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub rows: Vec<PackListRow>,
    pub total: i64,
    pub prev: Option<String>,
    pub next: Option<String>,
    /// The search text.
    pub q: String,
}

#[derive(Template)]
#[template(path = "login.html")]
pub struct LoginPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// The API's GitHub login.
    pub start: String,
}

#[derive(Template)]
#[template(path = "packs/building.html")]
pub struct PackBuildingPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub id: String,
    /// Why the build failed; the page then stops asking.
    pub failed: Option<String>,
    /// The try the page asks for next; `None` once it gave up waiting.
    pub next_try: Option<u32>,
}

#[derive(Template)]
#[template(path = "api_docs.html")]
pub struct ApiDocsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// `docs/public-api.md` without its title, through the same sanitizer as addon descriptions.
    pub description_html: String,
    pub toc: Vec<TocEntry>,
}

/// One heading of the API docs in the side navigation.
pub struct TocEntry {
    pub id: String,
    pub text: String,
    /// A subsection (`###` in the Markdown).
    pub sub: bool,
}

#[derive(Template)]
#[template(path = "api_playground.html")]
pub struct ApiPlaygroundPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    /// The form as sent: `registry`, `addon`, `versions` or `findings`.
    pub endpoint: String,
    pub slug: String,
    pub tag: String,
    /// The API path that was asked, empty before the first request.
    pub path: String,
    pub api_url: String,
    pub error: String,
    /// The API's answer, pretty-printed.
    pub result: String,
}
