//! `/stats`, and the line about the release check that the addon pages show.

use crate::app::{AppState, visitor};
use crate::csrf::CsrfToken;
use crate::error::{WebError, WebResult};
use crate::public::grouped;
use crate::templates::{AdminStatsPage, Permission, StatGroup, StatTable, StatsPage, render};
use axum::Extension;
use axum::extract::State;
use axum::response::Html;
use axum_extra::extract::PrivateCookieJar;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// One periodic check in words: "4 min ago", "in 11 min".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Check {
    /// The last successful run.
    pub last: String,
    /// The last run that ended, and whether it failed (then it is later than `last`).
    pub last_run: String,
    pub failed: bool,
    pub last_run_at: String,
    pub next: String,
    /// The exact times, for `<time datetime>`.
    pub last_at: String,
    pub next_at: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Checks {
    pub releases: Check,
    pub sources: Check,
}

/// A labelled number of the statistics page.
pub struct Stat {
    pub value: String,
    pub label: String,
}

pub struct TopAddon {
    pub slug: String,
    pub name: String,
    pub installs: String,
}

fn when(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()?.parse().ok()
}

fn span(seconds: i64) -> String {
    match seconds {
        s if s < 60 => "less than a minute".into(),
        s if s < 3600 => format!("{} min", s / 60),
        s if s < 2 * 86400 => format!("{} h", s / 3600),
        s => format!("{} days", s / 86400),
    }
}

pub fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (now - at).num_seconds() {
        s if s < 60 => "just now".into(),
        s => format!("{} ago", span(s)),
    }
}

pub fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    match (at - now).num_seconds() {
        s if s < 60 => "now".into(),
        s => format!("in {}", span(s)),
    }
}

fn check(v: &Value, now: DateTime<Utc>) -> Check {
    let (last, next, last_run) = (when(&v["last"]), when(&v["next"]), when(&v["last_run"]));
    Check {
        last: last.map_or_else(|| "never".into(), |t| ago(t, now)),
        last_run: last_run.map_or_else(|| "not yet".into(), |t| ago(t, now)),
        failed: v["failed"] == true,
        last_run_at: last_run.map(|t| t.to_rfc3339()).unwrap_or_default(),
        next: next.map_or_else(|| "soon".into(), |t| until(t, now)),
        last_at: last.map(|t| t.to_rfc3339()).unwrap_or_default(),
        next_at: next.map(|t| t.to_rfc3339()).unwrap_or_default(),
    }
}

pub fn checks_of(stats: &Value, now: DateTime<Utc>) -> Checks {
    Checks { releases: check(&stats["checks"]["releases"], now), sources: check(&stats["checks"]["sources"], now) }
}

/// The check times for the addon pages; a page still shows when the API cannot tell them.
pub async fn checks(state: &AppState) -> Option<Checks> {
    let stats = state.api.get("/v1/stats", None).await.ok()?;
    Some(checks_of(&stats, Utc::now()))
}

fn n(v: &Value) -> String {
    grouped(v.as_i64().unwrap_or(0))
}

fn stat(value: String, label: &str) -> Stat {
    Stat { value, label: label.to_string() }
}

pub async fn stats(State(state): State<AppState>, jar: PrivateCookieJar, Extension(csrf): Extension<CsrfToken>) -> WebResult<Html<String>> {
    let user = visitor(&state, &jar).await?.map(|(v, _)| v);
    let s = state.api.get("/v1/stats", None).await?;
    let (l, a, p) = (&s["launcher"], &s["addons"], &s["platform"]);
    let current = l["current_tag"].as_str().unwrap_or_default();
    let active = a["active_installations_30d"].as_i64().unwrap_or(0);
    let on_current = a["on_current_launcher_30d"].as_i64().unwrap_or(0);
    let w = &s["website"];
    let website = vec![
        stat(n(&w["daily_visitors_30d"]), "visits in the last 30 days (each visitor once a day)"),
        stat(n(&w["views_30d"]), "pages viewed in the last 30 days"),
        stat(n(&w["daily_visitors"]), "visits in all"),
        stat(n(&w["views"]), "pages viewed in all"),
    ];
    let launcher = vec![
        stat(n(&l["downloads"]), "downloads of every version"),
        stat(n(&l["current_downloads"]), &if current.is_empty() { "downloads of the newest version".into() } else { format!("downloads of {current}, the newest version") }),
        stat(n(&l["releases"]), "releases"),
    ];
    let addons = vec![
        stat(n(&a["installs"]), "installs through the launcher"),
        stat(n(&a["updates"]), "updates through the launcher"),
        stat(n(&a["installs_30d"]), "installs in the last 30 days"),
        stat(n(&a["updates_30d"]), "updates in the last 30 days"),
        stat(grouped(active), "launchers active in the last 30 days (those that send an installation ID)"),
        stat(if active == 0 { "-".into() } else { format!("{}%", on_current * 100 / active) }, "of them on the newest launcher"),
    ];
    let platform = vec![
        stat(n(&p["addons"]), "addons"),
        stat(n(&p["verified_addons"]), "verified by hand"),
        stat(n(&p["creators"]), "creators"),
        stat(n(&p["scanned_versions"]), "versions scanned"),
        stat(n(&p["blocked_versions"]), "versions blocked"),
        stat(n(&p["game_versions"]), "game versions known"),
        stat(n(&p["github_downloads"]), "downloads of addon releases on GitHub"),
        stat(n(&p["website_downloads"]), "addon downloads through this website"),
        stat(n(&p["packs"]), "addon packs"),
        stat(n(&p["public_packs"]), "public packs"),
        stat(n(&p["pack_downloads"]), "pack downloads"),
    ];
    let top = s["top_addons_30d"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| TopAddon { slug: t["slug"].as_str().unwrap_or_default().into(), name: t["name"].as_str().unwrap_or_default().into(), installs: n(&t["installs"]) })
        .collect();
    Ok(render(&StatsPage { user, csrf: csrf.0, website, launcher, addons, platform, top, checks: checks_of(&s, Utc::now()) }))
}

#[derive(serde::Deserialize, Default)]
pub struct RangeQuery {
    #[serde(default)]
    range: Option<String>,
}

/// `1,234`, `75%`, `-`: a cell that aligns right with the other numbers of its column.
fn is_number(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| c.is_ascii_digit() || matches!(c, ',' | '.' | '%' | '-'))
}

fn table(title: &str, head: &[&str], list: &Value, cells: impl Fn(&Value) -> Vec<String>, empty: &str) -> StatTable {
    let rows: Vec<Vec<String>> = list.as_array().into_iter().flatten().map(cells).collect();
    // The first column names the row; any other is a number column when all its cells are numbers.
    let numeric = (0..head.len()).map(|i| i > 0 && !rows.is_empty() && rows.iter().all(|r| r.get(i).is_some_and(|c| is_number(c)))).collect();
    StatTable { title: title.into(), head: head.iter().map(|h| h.to_string()).collect(), rows, numeric, empty: empty.into(), note: String::new(), cells: "" }
}

/// Most hours a table lists; 30 days would be 720 rows.
const MAX_HOURS: usize = 48;

/// `t` with at most its first `MAX_HOURS` rows, and a note when some were left out.
fn capped(mut t: StatTable) -> StatTable {
    if t.rows.len() > MAX_HOURS {
        t.rows.truncate(MAX_HOURS);
        t.note = format!("The last {MAX_HOURS} hours; Grafana has the rest.");
    }
    t
}

/// What needs a look first: failing jobs, scans and installs, and a GitHub quota near its end.
pub fn attention(s: &Value, now: chrono::DateTime<Utc>) -> Vec<String> {
    let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let mut found = Vec::new();
    for job in list(&s["jobs"]).iter().filter(|j| j["failed"].as_i64().unwrap_or(0) > 0) {
        found.push(format!("{}: {} failed runs, last error: {}", text(&job["kind"]), n(&job["failed"]), text(&job["last_error"])));
    }
    for q in list(&s["github"]["quotas"]) {
        let (left, limit) = (q["remaining"].as_i64().unwrap_or(0), q["limit"].as_i64().unwrap_or(0));
        if limit > 0 && left * 10 < limit {
            let reset = when(&q["reset_at"]).map_or_else(String::new, |t| format!(", resets {}", until(t, now)));
            found.push(format!("GitHub quota {} ({}): {} of {} left{reset}", quota_name(&text(&q["quota"]), &s["github"]["installations"]), text(&q["resource"]), n(&q["remaining"]), n(&q["limit"])));
        }
    }
    if s["scans"]["errors"].as_i64().unwrap_or(0) > 0 {
        found.push(format!("{} scans failed", n(&s["scans"]["errors"])));
    }
    for a in list(&s["telemetry"]["failing_addons"]) {
        found.push(format!("{}: {} of {} installs or updates failed", text(&a["slug"]), n(&a["failed"]), n(&a["total"])));
    }
    found
}

/// A GitHub quota in words: `installation:7` is the App installation of an account.
fn quota_name(quota: &str, installations: &Value) -> String {
    match quota.split_once(':') {
        Some(("installation", id)) => installations[id].as_str().map_or_else(|| format!("App installation {id}"), |owner| format!("App installation of {owner}")),
        _ => match quota {
            "read_token" => "Read token".into(),
            "anonymous" => "Anonymous".into(),
            "app" => "App".into(),
            "oauth" => "User logins".into(),
            "web" => "github.com pages".into(),
            "download" => "Release downloads".into(),
            other => other.into(),
        },
    }
}

/// `install_failed` as `Install failed`.
fn event_name(kind: &str) -> String {
    let words = kind.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

/// The newest first.
fn reversed(list: &Value) -> Value {
    Value::Array(list.as_array().into_iter().flatten().rev().cloned().collect())
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

/// `/admin/stats`: requests, jobs, scans and launcher events of the last 24 hours, 7 or 30 days.
pub async fn admin_stats(
    State(state): State<AppState>,
    jar: PrivateCookieJar,
    Extension(csrf): Extension<CsrfToken>,
    axum::extract::Query(q): axum::extract::Query<RangeQuery>,
) -> WebResult<Html<String>> {
    let (user, token) = visitor(&state, &jar).await?.ok_or(WebError::LoginRequired)?;
    if !user.can(Permission::ViewAdminStats) {
        return Err(WebError::Forbidden);
    }
    let range = match q.range.as_deref() {
        Some(r @ ("24h" | "7d" | "30d")) => r.to_string(),
        _ => "24h".into(),
    };
    let s = state.api.get(&format!("/v1/admin/stats?range={range}"), Some(&token)).await?;
    let (r, sc) = (&s["requests"], &s["scans"]);
    let (g, now) = (&s["github"], Utc::now());
    let left = g["quotas"].as_array().into_iter().flatten().find(|q| q["quota"] == "read_token" || q["quota"] == "anonymous");
    let github = left.map_or_else(|| "unknown".into(), |q| format!("{} of {}", n(&q["remaining"]), n(&q["limit"])));
    let totals = vec![
        stat(n(&r["total"]), "API requests"),
        stat(n(&r["rate_limited"]), "turned away by the rate limit"),
        stat(n(&sc["scans"]), "scans"),
        stat(n(&sc["flagged"]), "scans with findings"),
        stat(n(&sc["errors"]), "scans that failed"),
        stat(n(&sc["blocked_versions"]), "versions blocked"),
        stat(github, "public GitHub requests left now"),
        stat(n(&g["total"]), "GitHub requests"),
    ];
    let t = &s["telemetry"];
    let w = &s["website"];
    let visits = |title: &str, head: &str, list: &Value| table(title, &[head, "Views", "Visitors"], list, |x| vec![text(&x["value"]), n(&x["views"]), n(&x["visitors"])], "No visits yet.");
    let group = |id: &'static str, title: &'static str, tables: Vec<StatTable>| StatGroup { id, title, note: "", tables };
    let groups = vec![
        StatGroup { note: "Counted by whole days (UTC): the 24 hours range shows today and yesterday.", ..group("website", "Website", vec![
            table("Website visits per day (UTC)", &["Day", "Views", "Visitors"], &reversed(&w["per_day"]), |x| vec![text(&x["day"]), n(&x["views"]), n(&x["visitors"])], "No visits yet."),
            table("Most viewed pages", &["Page", "Views", "Visitors"], &w["pages"], |x| vec![text(&x["page"]), n(&x["views"]), n(&x["visitors"])], "No visits yet."),
            table("Most viewed addons and packs", &["Page", "Views", "Visitors"], &w["targets"], |x| {
                vec![text(&x["page"]).replace("{slug}", &text(&x["target"])).replace("{id}", &text(&x["target"])), n(&x["views"]), n(&x["visitors"])]
            }, "No visits yet."),
            visits("Where visitors come from", "Site", &w["referrers"]),
            StatTable { cells: "country", ..visits("Countries", "Country", &w["countries"]) },
            visits("Devices", "Device", &w["devices"]),
        ]) },
        group("api", "API", vec![
            table("Busiest routes", &["Route", "Requests", "Average ms", "Slowest ms"], &r["routes"], |x| {
                vec![format!("{} {}", text(&x["method"]), text(&x["route"])), n(&x["count"]), n(&x["avg_ms"]), n(&x["max_ms"])]
            }, "No requests yet."),
            table("Clients", &["Client", "Requests"], &r["clients"], |x| vec![text(&x["client"]), n(&x["count"])], "No requests yet."),
            table("Status codes", &["Status", "Requests"], &r["statuses"], |x| vec![text(&x["status"]), n(&x["count"])], "No requests yet."),
        ]),
        group("jobs", "Jobs and scans", vec![
            table("Jobs", &["Kind", "Waiting now", "Running now", "Done", "Failed", "Last error"], &s["jobs"], |x| {
                vec![text(&x["kind"]), n(&x["open"]), n(&x["running"]), n(&x["done"]), n(&x["failed"]), text(&x["last_error"])]
            }, "No jobs."),
        ]),
        group("github", "GitHub", vec![
            table("GitHub quotas now", &["Quota", "Limit", "Used", "Left", "Resets", "As of"], &g["quotas"], |x| {
                let at = |v: &Value| when(v);
                vec![
                    format!("{} ({})", quota_name(&text(&x["quota"]), &g["installations"]), text(&x["resource"])),
                    n(&x["limit"]),
                    n(&x["used"]),
                    n(&x["remaining"]),
                    at(&x["reset_at"]).map_or_else(|| "-".into(), |t| until(t, now)),
                    at(&x["seen_at"]).map_or_else(|| "-".into(), |t| ago(t, now)),
                ]
            }, "GitHub has not answered yet."),
            table("GitHub requests by origin", &["Caused by", "Requests"], &g["origins"], |x| vec![text(&x["origin"]), n(&x["count"])], "No GitHub requests."),
            table("GitHub endpoints", &["Endpoint", "Quota", "Requests", "Errors"], &g["endpoints"], |x| {
                vec![text(&x["endpoint"]), quota_name(&text(&x["quota"]), &g["installations"]), n(&x["count"]), n(&x["errors"])]
            }, "No GitHub requests."),
            capped(table("GitHub requests per hour", &["Hour (UTC)", "Requests"], &reversed(&g["per_hour"]), |x| {
                vec![when(&x["hour"]).map_or_else(|| text(&x["hour"]), |t| t.format("%Y-%m-%d %H:00").to_string()), n(&x["count"])]
            }, "No GitHub requests.")),
        ]),
        group("launcher", "Launcher", vec![
            table("Launcher events", &["Kind", "Events"], &t["kinds"], |x| vec![event_name(&text(&x["kind"])), n(&x["count"])], "No events from launchers yet."),
            table("Addons that fail to install", &["Addon", "Failed", "All events"], &t["failing_addons"], |x| {
                vec![text(&x["slug"]), n(&x["failed"]), n(&x["total"])]
            }, "No failed installs."),
            table("Error codes", &["Code", "Events"], &t["error_codes"], |x| vec![text(&x["code"]), n(&x["count"])], "No errors."),
            table("Launcher versions", &["Version", "Events"], &t["launcher_versions"], |x| vec![text(&x["version"]), n(&x["count"])], "No events from launchers yet."),
        ]),
    ];
    let attention = attention(&s, now);
    let since = when(&s["since"]).map_or_else(String::new, |t| t.format("%Y-%m-%d %H:%M UTC").to_string());
    let grafana = user.can(Permission::ViewGrafana);
    Ok(render(&AdminStatsPage { user: Some(user), csrf: csrf.0, grafana, range, since, totals, attention, groups }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_naturally() {
        let now: DateTime<Utc> = "2026-10-31T10:07:00Z".parse().unwrap();
        assert_eq!(ago("2026-10-31T10:03:00Z".parse().unwrap(), now), "4 min ago");
        assert_eq!(ago("2026-10-31T10:06:30Z".parse().unwrap(), now), "just now");
        assert_eq!(until("2026-10-31T10:15:00Z".parse().unwrap(), now), "in 8 min");
        assert_eq!(until("2026-10-31T16:00:00Z".parse().unwrap(), now), "in 5 h");
        assert_eq!(until("2026-10-31T10:06:00Z".parse().unwrap(), now), "now", "overdue: running now");
        assert_eq!(ago("2026-10-27T10:07:00Z".parse().unwrap(), now), "4 days ago");
    }

    #[test]
    fn missing_times_say_so() {
        let checks = checks_of(&serde_json::json!({}), Utc::now());
        assert_eq!((checks.releases.last_run.as_str(), checks.releases.next.as_str(), checks.releases.failed), ("not yet", "soon", false));
    }
}
