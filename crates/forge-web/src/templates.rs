//! Askama templates. Every variable is HTML-escaped; the only exception is an addon's
//! `description_html`, which the API sanitized.

use askama::Template;
use axum::response::Html;
pub use forge_core::roles::Permission;
use serde_json::Value;

/// The logged-in visitor as the API describes them (`/v1/me`).
#[derive(Debug, Clone, Default)]
pub struct Visitor {
    pub id: i64,
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
            id: me["id"].as_i64().unwrap_or_default(),
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

impl Card {
    /// The first letter of the name, drawn in place of a missing icon.
    pub fn initial(&self) -> String {
        self.name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default()
    }
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
    /// The ticked categories; an addon in any of them (or their subcategories) is listed.
    pub chosen_categories: Vec<String>,
    pub trust: String,
    pub sort: String,
    /// Only the addons of this author (`?author=`); empty for all.
    pub author: String,
    pub total: i64,
    /// For the filter, all of them.
    /// Top-level categories with their subcategories, for the grouped filter.
    pub categories: Vec<(CategoryOption, Vec<CategoryOption>)>,
    pub cards: Vec<Card>,
    /// The page navigation; `None` when everything fits on one page.
    pub pager: Option<Pager>,
}

impl AddonsPage {
    pub fn is_chosen(&self, name: &str) -> bool {
        self.chosen_categories.iter().any(|c| c.eq_ignore_ascii_case(name))
    }

    /// The subcategories of a group stay folded unless one of them is ticked.
    pub fn opens(&self, children: &[CategoryOption]) -> bool {
        children.iter().any(|c| self.is_chosen(&c.name))
    }
}

pub struct FindingRow {
    /// `critical`, `warn`, `notice` or `info`, as the scanner names it.
    pub severity: String,
    pub rule: String,
    pub place: String,
    pub message: String,
    pub snippet: String,
    /// How the author can fix it, empty when the rule only informs.
    pub hint: String,
}

impl FindingRow {
    /// The severity in words for visitors: `Critical`, `Warning`, `Note`.
    pub fn label(&self) -> &'static str {
        match self.severity.as_str() {
            "critical" => "Critical",
            "warn" => "Warning",
            _ => "Note",
        }
    }
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

impl VersionRow {
    /// The tag for "Version …" headings: short, and `v2.8.15` as `2.8.15`.
    pub fn heading(&self) -> &str {
        match self.tag_short.strip_prefix('v') {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
            _ => &self.tag_short,
        }
    }
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
    /// GitHub counts the downloads (release files, not the source zips of tags).
    pub counted: bool,
    pub pager: Option<Pager>,
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
#[template(path = "creator/verification.html")]
pub struct VerificationPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub perms: Perms,
    /// The newest request: "open", "approved" or "rejected"; empty without one.
    pub status: String,
    pub tag: String,
    /// The admins' reason for a rejection.
    pub note: String,
    /// What the owner typed, kept when the API refused it.
    pub discord_url: String,
    pub error: Option<String>,
    pub channel: &'static str,
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
    pub verifications: usize,
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
    /// Per column: numbers, aligned right.
    pub numeric: Vec<bool>,
    /// Shown instead of an empty table.
    pub empty: String,
    /// Shown under the table, e.g. that rows were left out.
    pub note: String,
    /// What the first column holds when the browser can name it better (`country`); empty for none.
    pub cells: &'static str,
    /// Per row: `warn`, `failed` or empty; may be shorter than `rows`.
    pub row_classes: Vec<&'static str>,
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
    /// The viewer may open Grafana.
    pub grafana: bool,
    /// `overview` or the part shown (`crate::stats::SECTIONS`).
    pub section: &'static str,
    /// The overview's few numbers per part.
    pub summaries: Vec<StatSummary>,
    /// Problems shown first; empty when all is well.
    pub attention: Vec<String>,
    pub groups: Vec<StatGroup>,
}

impl StatTable {
    /// The class of row `i`: `warn`, `failed` or empty.
    pub fn row_class(&self, i: usize) -> &'static str {
        self.row_classes.get(i).copied().unwrap_or_default()
    }
}

impl AdminStatsPage {
    /// The address of `section` in the current range.
    pub fn href(&self, section: &str) -> String {
        let path = if section == "overview" { "/admin/stats".to_string() } else { format!("/admin/stats/{section}") };
        format!("{path}?range={}", self.range)
    }

    pub fn tabs(&self) -> &'static [(&'static str, &'static str)] {
        &crate::stats::SECTIONS
    }
}

/// A part of `/admin/stats` on the overview: a few numbers and the link to its page.
pub struct StatSummary {
    pub id: &'static str,
    pub title: &'static str,
    pub stats: Vec<crate::stats::Stat>,
}

/// A part of `/admin/stats` with its tables.
pub struct StatGroup {
    pub id: &'static str,
    pub title: &'static str,
    /// A line under the heading; empty for none.
    pub note: &'static str,
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

pub struct VerificationRequestRow {
    pub id: i64,
    pub slug: String,
    pub name: String,
    pub tag: String,
    pub flagged: bool,
    pub discord_url: String,
    pub requested_by: String,
    pub created: String,
}

#[derive(Template)]
#[template(path = "admin/verification_requests.html")]
pub struct AdminVerificationRequestsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub rows: Vec<VerificationRequestRow>,
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
    pub rows: Vec<AdminAddonRow>,
    pub total: i64,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub msg: Option<String>,
    pub error: Option<String>,
}

/// One addon in the admin addon list.
pub struct AdminAddonRow {
    pub slug: String,
    pub name: String,
    /// `owner/name` on GitHub.
    pub repo: String,
    /// Login of the owner; empty while nobody claimed the addon.
    pub owner: String,
    /// The owner, or the author's name while nobody claimed the addon.
    pub author: String,
    /// Tag of the version players get; empty without one.
    pub latest_tag: String,
    pub notice: Notice,
    pub findings: i64,
    pub blocked: bool,
    pub locked: bool,
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
    pub id: i64,
    pub when: String,
    pub admin: String,
    pub action: String,
    /// What the entry is about, for people: "Lucky Odds v1.2.0", a login, a category.
    pub target: String,
    /// The admin page of the target's addon or user.
    pub target_href: Option<String>,
    pub note: String,
}

#[derive(Template)]
#[template(path = "admin/audit_entry.html")]
pub struct AdminAuditEntryPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub entry: AuditRow,
    /// To the second, in UTC.
    pub when: String,
    pub target_type: String,
    pub target_id: String,
    /// The addon of the target (also of a version or a report); empty for other targets.
    pub addon_slug: String,
    pub addon_name: String,
    /// Tag of a version target.
    pub tag: String,
    /// Login of a user target.
    pub login: String,
    /// Every key the action stored, values as text.
    pub details: Vec<(String, String)>,
    /// The other entries on the same addon or target, newest first.
    pub history: Vec<AuditRow>,
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
    /// On the public pack list; only staff see the other ones.
    pub listed: bool,
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
    /// A moderator or admin, who may switch to every pack.
    pub staff: bool,
    /// Every pack, also the unlisted ones (staff only).
    pub every: bool,
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
#[template(path = "docs_index.html")]
pub struct DocsIndexPage {
    pub user: Option<Visitor>,
    pub csrf: String,
}

#[derive(Template)]
#[template(path = "docs.html")]
pub struct DocsPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub title: &'static str,
    /// The "Try it in the playground" button of the API docs.
    pub playground: bool,
    /// A file of `docs/` without its title, through the same sanitizer as addon descriptions.
    pub description_html: String,
    pub toc: Vec<TocEntry>,
}

/// One heading of a docs page in the side navigation.
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

#[derive(Template)]
#[template(path = "report.html")]
pub struct ReportPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    /// Tags of the addon's versions, newest first.
    pub versions: Vec<String>,
    /// (value for the API, what the form says)
    pub reasons: &'static [(&'static str, &'static str)],
    /// The picked version's tag; empty for the whole addon.
    pub version: String,
    pub reason: String,
    pub text: String,
    pub error: Option<String>,
}

#[derive(Template)]
#[template(path = "report_done.html")]
pub struct ReportDonePage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
}

/// The page navigation of a long list.
pub struct Pager {
    /// "21–40 of 87 addons".
    pub summary: String,
    pub prev: Option<String>,
    pub next: Option<String>,
    pub items: Vec<PageItem>,
}

/// A page number of the pager, or a gap between them (`number` 0).
pub struct PageItem {
    pub number: i64,
    pub href: String,
    pub current: bool,
}

impl Pager {
    /// First and last page and the pages next to `page`; skipped stretches become one gap. A gap
    /// of a single page shows that page instead, so "…" always stands for two or more. Shown even
    /// for a single page; only an empty list has none.
    pub fn new(page: i64, per_page: i64, total: i64, noun: &str, link: impl Fn(i64) -> String) -> Option<Pager> {
        let pages = (total + per_page - 1) / per_page;
        if pages < 1 {
            return None;
        }
        let page = page.clamp(1, pages);
        let shown = |n: i64| n == 1 || n == pages || (n - page).abs() <= 1 || (n == 2 && page == 4) || (n == pages - 1 && page == pages - 3);
        let mut items: Vec<PageItem> = Vec::new();
        for n in 1..=pages {
            if shown(n) {
                items.push(PageItem { number: n, href: link(n), current: n == page });
            } else if items.last().is_some_and(|i| i.number != 0) {
                items.push(PageItem { number: 0, href: String::new(), current: false });
            }
        }
        let first = (page - 1) * per_page + 1;
        let last = (page * per_page).min(total);
        Some(Pager {
            summary: format!("{first}–{last} of {} {}", crate::public::grouped(total), if total == 1 { noun.trim_end_matches('s') } else { noun }),
            prev: (page > 1).then(|| link(page - 1)),
            next: (page < pages).then(|| link(page + 1)),
            items,
        })
    }
}

#[cfg(test)]
mod pager_tests {
    use super::Pager;

    fn numbers(page: i64, total: i64) -> Vec<i64> {
        Pager::new(page, 20, total, "addons", |n| n.to_string()).map(|p| p.items.iter().map(|i| i.number).collect()).unwrap_or_default()
    }

    #[test]
    fn gaps_stand_for_two_or_more_pages() {
        assert_eq!(numbers(1, 0), Vec::<i64>::new(), "nothing found: no pager");
        assert_eq!(numbers(1, 20), vec![1], "one page: still a pager");
        assert_eq!(numbers(1, 200), vec![1, 2, 0, 10]);
        assert_eq!(numbers(5, 200), vec![1, 0, 4, 5, 6, 0, 10]);
        assert_eq!(numbers(4, 200), vec![1, 2, 3, 4, 5, 0, 10], "page 2 instead of a gap of one");
        assert_eq!(numbers(10, 200), vec![1, 0, 9, 10]);
        assert_eq!(numbers(2, 60), vec![1, 2, 3]);
    }

    #[test]
    fn the_summary_counts_what_the_page_shows() {
        let pager = Pager::new(10, 20, 195, "addons", |n| n.to_string()).unwrap();
        assert_eq!(pager.summary, "181–195 of 195 addons");
        assert!(pager.next.is_none() && pager.prev.is_some());
        assert_eq!(Pager::new(1, 20, 1, "addons", |n| n.to_string()).unwrap().summary, "1–1 of 1 addon");
    }
}

pub struct IssueItem {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub author: String,
}

pub struct IssueComment {
    pub id: i64,
    pub can_edit: bool,
    pub author: String,
    pub body: String,
}

#[derive(Template)]
#[template(path = "issues.html")]
pub struct IssuesPage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub issues: Vec<IssueItem>,
    pub next: Option<i64>,
}

#[derive(Template)]
#[template(path = "issue.html")]
pub struct IssuePage {
    pub user: Option<Visitor>,
    pub csrf: String,
    pub slug: String,
    pub name: String,
    pub id: i64,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub status: String,
    pub author: String,
    pub comments: Vec<IssueComment>,
    pub can_manage: bool,
    pub can_edit_issue: bool,
    pub can_delete_issue: bool,
}

