#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use survivorsforge::config::{self, Config};
use survivorsforge::controller::{self, Badge, Row, RowState, UNMANAGED_PREFIX};
use survivorsforge::account::{Account, Keyring, TokenSource};
use survivorsforge::deep_link;
use survivorsforge::login;
use survivorsforge::game_update::{self, ExeSeen, WebServer};
use survivorsforge::github::{normalize_repo, ApiGitHub, Client, GitHub, RoutedGitHub, TokenGitHub, WebGitHub};
use survivorsforge::install::{self, Request};
use survivorsforge::local_scan;
use survivorsforge::registry::{self, Origin, Registry, SourceSpec};
use survivorsforge::repo_detect;
use survivorsforge::scan::{scan_addons, InstalledAddon};
use survivorsforge::self_update::{self, Available};
use survivorsforge::state::{Reconciled, Source, State};
use survivorsforge::updates::{self, RemoteVersion, UpdateStatus};
use survivorsforge::telemetry::{self, ApiSender};
use survivorsforge::{archive, unique_name, Error, APP_REPO, APP_VERSION};
use slint_toast::{Kind, Toasts};

#[cfg(target_os = "macos")]
mod macos;

slint::include_modules!();

const USE_WINDOWS_GAME: bool = cfg!(any(windows, target_os = "macos"));

struct Core {
    data_dir: PathBuf,
    config: Config,
    registry: Registry,
    state: State,
    scan: Vec<InstalledAddon>,
    reconciled: Reconciled,
    statuses: HashMap<String, UpdateStatus>,
    /// Entry ids with an install, update or removal running.
    busy: HashSet<String>,
    checking: bool,
    search: String,
    /// Running work and how things ended, bottom right.
    toasts: Toasts,
    /// The busy toast of the running work (checking, installing), which turns into how it ended.
    work: Option<i32>,
    /// The toast of the last invalid setting, closed once the settings are valid again.
    settings_error: Option<i32>,
    /// The toast of the last settings save, replaced by the next one.
    settings_saved: Option<i32>,
    self_update: Option<Available>,
    /// The platform's game versions, oldest first; empty without the platform.
    game_versions: Vec<controller::GameVersion>,
    /// The last check got no answer for them; an empty list is an answer.
    game_versions_failed: bool,
    /// The last check could not reach the platform: installs and updates wait.
    offline: bool,
    /// The platform's categories; empty offline.
    categories: Vec<controller::Category>,
    /// The Browse category filter, `None` for all.
    category: Option<String>,
    /// The safety check filter and the order of Browse and Installed, as on the website.
    safety: Option<forge_core::trust::Trust>,
    sort: controller::Sort,
    /// Browse addons picked for an addon pack.
    picked: std::collections::BTreeSet<String>,
    pack_busy: bool,
    /// The game is starting or running: the launch button waits for it to close.
    game_running: bool,
    /// What the game's server has that the game folder lacks; `None` when up to date or unknown.
    game_plan: Option<game_update::Plan>,
    /// The game's exe as last hashed.
    game_exe: Option<ExeSeen>,
    game_updating: bool,
    /// What the running game update says on the launch button, and how full its bar is.
    game_step: Option<(String, f32)>,
    /// Said under the launch button after an update, until the next check.
    game_note: String,
    /// Where each running install or update stands.
    progress: HashMap<String, controller::Progress>,
    /// How each install or update since the last check ended: (text, failed).
    outcomes: HashMap<String, (String, bool)>,
    /// Redraws the window from any thread; set once the window exists.
    repaint: Option<Arc<dyn Fn() + Send + Sync>>,
    /// When progress last redrew the window, so a fast download does not redraw it for every read.
    painted: Option<std::time::Instant>,
    /// The addon in the detail view, and whether it was opened from Browse (else Installed).
    detail: Option<(String, bool)>,
    /// Icon URLs and detail pages the platform last showed, kept across starts in cache/platform.json.
    platform: controller::PlatformCache,
    /// Addon ids whose detail page is being asked for, and those asked for since this start.
    fetching: HashSet<String>,
    refreshed: HashSet<String>,
    /// Downloaded icons and screenshots by URL; `None` while loading or when it failed.
    pictures: HashMap<String, Option<Pixels>>,
    /// The comments page of the detail view, while it is open.
    comments: Option<Comments>,
    /// The Issues and Versions tabs of the detail view, once opened.
    issues: Option<Listing<(Vec<controller::IssueLine>, u64)>>,
    versions: Option<Listing<Vec<controller::VersionLine>>>,
    /// What the Issues tab shows besides the list: the form or one issue.
    issue_view: IssueView,
    /// The GitHub login on the platform, `None` while logged out.
    me: Option<controller::Me>,
    /// Set while the browser is open for a login; storing `true` stops the wait.
    login_cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Why the last login failed; empty after a good one.
    login_error: String,
    /// Logged in to GitHub alone (device flow) without the platform: the GitHub login name.
    github_only: Option<String>,
    /// The code to enter at github.com/login/device while a device login waits, and that page.
    device_code: String,
    device_url: String,
}

/// The Issues tab of the detail view beyond its list.
#[derive(Default)]
struct IssueView {
    /// 0 the list, 1 the form for a new issue, 2 one issue.
    mode: i32,
    slug: String,
    number: u64,
    page: Option<controller::IssuePage>,
    loading: bool,
    failed: bool,
    sending: bool,
    /// Why the last issue or reply was not sent; the text stays.
    send_error: String,
    /// The login or the GitHub token ran out: the error offers to log in again.
    needs_login: bool,
}

/// The comments of one addon loaded so far, and how the next page is coming.
struct Comments {
    slug: String,
    /// Newest first, as the platform sent them; `comment_threads` orders them for reading.
    loaded: Vec<controller::Comment>,
    /// The id to ask older comments before, while there are more.
    older: Option<i64>,
    /// When the page now asked for was asked for; `None` while nothing loads.
    asked: Option<std::time::Instant>,
    failed: bool,
    off: bool,
    /// Counts the requests: an answer for an older one (another addon, a closed page) is dropped.
    request: u64,
    /// The comment being answered (id, author); `None` for a new comment.
    reply_to: Option<(i64, String)>,
    /// The dialog to write is open.
    composing: bool,
    sending: bool,
    /// Why the last comment was not sent; the text stays in the box.
    send_error: String,
    /// The login ran out: the error offers to log in again.
    needs_login: bool,
}

type Pixels = slint::SharedPixelBuffer<slint::Rgba8Pixel>;

/// Longer side in pixels the launcher keeps of an icon (the platform's full 256, for the tiles), and
/// of a screenshot or description image.
const ICON_EDGE: u32 = 256;
const SHOT_EDGE: u32 = 1600;

type Shared = Arc<Mutex<Core>>;

fn lock(shared: &Shared) -> MutexGuard<'_, Core> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

impl Core {
    fn new(data_dir: PathBuf, config: Option<Config>) -> Core {
        let registry = registry::load(None, &data_dir.join("cache")).0;
        let platform = controller::PlatformCache::load(&data_dir.join("cache"));
        let mut core = Core {
            data_dir,
            config: Config::default(),
            registry,
            state: State::default(),
            scan: Vec::new(),
            reconciled: Reconciled::default(),
            statuses: HashMap::new(),
            busy: HashSet::new(),
            checking: false,
            search: String::new(),
            toasts: Toasts::default(),
            work: None,
            settings_error: None,
            settings_saved: None,
            self_update: None,
            game_versions: Vec::new(),
            game_versions_failed: false,
            offline: false,
            categories: Vec::new(),
            category: None,
            safety: None,
            sort: controller::Sort::default(),
            picked: Default::default(),
            pack_busy: false,
            game_running: false,
            game_plan: None,
            game_exe: None,
            game_updating: false,
            game_step: None,
            game_note: String::new(),
            progress: HashMap::new(),
            outcomes: HashMap::new(),
            repaint: None,
            painted: None,
            detail: None,
            platform,
            fetching: HashSet::new(),
            refreshed: HashSet::new(),
            pictures: HashMap::new(),
            comments: None,
            issues: None,
            versions: None,
            issue_view: IssueView::default(),
            me: None,
            login_cancel: None,
            login_error: String::new(),
            github_only: None,
            device_code: String::new(),
            device_url: String::new(),
        };
        if let Some(config) = config {
            core.switch_config(config);
        }
        core
    }

    /// Applies a (new) config: loads the state of its AddOns folder and scans it.
    fn switch_config(&mut self, config: Config) {
        self.config = config;
        let (state, note) = State::load(&self.data_dir, &self.config.addons_dir);
        self.state = state;
        self.statuses.clear();
        if note.is_some() {
            self.notify("state.json was damaged and saved as state.json.bak; starting fresh.".into(), Kind::Error);
        }
        // Leftovers of downloads that were never installed (cancelled dialogs, crashes).
        let _ = std::fs::remove_dir_all(self.downloads());
        self.rescan();
    }

    fn addons_dir(&self) -> &Path {
        &self.config.addons_dir
    }

    fn downloads(&self) -> PathBuf {
        install::work_dir(self.addons_dir()).join("downloads")
    }

    /// A platform client; it carries the installation ID when the user allows it.
    fn client(&self) -> Client {
        Client::new().with_install_id(telemetry::install_header(&self.config)).with_github_token(self.github_token())
    }

    /// The user's GitHub token while it still runs: release zips of `TokenGitHub` come through
    /// GitHub's REST API as them.
    fn github_token(&self) -> Option<String> {
        survivorsforge::account::valid_github_token(&Keyring, &self.api_url(), chrono::Utc::now())
    }

    fn save_platform(&self) {
        let _ = self.platform.save(&self.data_dir.join("cache"));
    }

    fn api_url(&self) -> String {
        registry::api_url(self.config.api_url.as_deref())
    }

    fn web_url(&self) -> String {
        registry::web_url(self.config.web_url.as_deref())
    }

    /// Release and tag metadata, through the platform.
    /// GitHub through the SurvivorsForge server, or straight from github.com while it is unreachable.
    fn github(&self) -> Box<dyn GitHub + Send> {
        // Repos the platform knows go to it while it answers; any other repo, and all while it is
        // away, straight to GitHub: with the user's token when there is one (`github::github_for`).
        let token = survivorsforge::account::valid_github_token(&Keyring, &self.api_url(), chrono::Utc::now());
        let client = self.client();
        let direct: Box<dyn GitHub + Send> = match token {
            Some(token) => Box::new(TokenGitHub::new(client.clone(), token)),
            None => Box::new(WebGitHub::new(client.clone())),
        };
        if self.offline {
            return direct;
        }
        let mut known: HashSet<String> = self.registry.addons.iter().map(|a| a.repo.to_ascii_lowercase()).collect();
        known.insert(APP_REPO.to_ascii_lowercase());
        Box::new(RoutedGitHub { platform: Some(ApiGitHub::new(client, &self.api_url())), direct, known })
    }

    fn rescan(&mut self) {
        if self.addons_dir().as_os_str().is_empty() {
            return;
        }
        match scan_addons(&self.config.addons_dir) {
            Ok(scan) => {
                if self.state.adopt(&scan, &self.registry) {
                    self.save_state();
                }
                self.reconciled = self.state.reconcile(&scan);
                self.scan = scan;
            }
            Err(e) => self.notify(format!("Cannot read the AddOns folder: {e}"), Kind::Error),
        }
    }

    fn save_state(&mut self) {
        if let Err(e) = self.state.save(&self.data_dir, &self.config.addons_dir) {
            self.notify(format!("Could not save state.json: {e}"), Kind::Error);
        }
    }

    /// Keeps the platform's issue frame of `rid`: the Issues tab (`here`) and whether bugs go to
    /// the platform (`ready`). True when that changed what the details show.
    fn take_issue_frame(&mut self, rid: &str, here: bool, ready: bool) -> bool {
        let mut changed = self.platform.issues_ready.insert(rid.to_string(), ready) != Some(ready);
        if let Some(info) = self.platform.info.get_mut(rid).filter(|i| i.issues_on != here) {
            info.issues_on = here;
            changed = true;
        }
        changed
    }

    /// Shows running work in the busy toast; the next step of it changes its text.
    // ponytail: one busy toast for all work, as the old status line; one per job if they overlap often.
    fn set_status(&mut self, text: String) -> i32 {
        match self.work.filter(|id| self.toasts.get(*id).is_some()) {
            Some(id) => {
                self.toasts.set_text(id, text);
                id
            }
            None => *self.work.insert(self.toasts.busy(text)),
        }
    }

    /// How something ended: the busy toast turns into it, else it comes as a new toast.
    /// A settings save says so in one toast: ticking box after box does not stack them.
    fn settings_saved(&mut self, text: String, kind: Kind) {
        if let Some(id) = self.settings_saved.take() {
            self.toasts.dismiss(id);
        }
        self.settings_saved = Some(self.toasts.push(text, kind));
    }

    fn notify(&mut self, text: String, kind: Kind) {
        match self.work.take() {
            Some(id) => self.toasts.finish(id, text, kind),
            None => self.toasts.push(text, kind),
        };
    }

    fn repo_of(&self, id: &str) -> Option<String> {
        match self.state.get(id).map(|e| &e.source) {
            Some(Source::GitHub { repo, .. }) => Some(repo.clone()),
            Some(Source::Verified { registry_id }) => self.registry.get(registry_id).map(|a| a.repo.clone()),
            Some(Source::Zip { .. }) => None,
            None => self.registry.get(id).map(|a| a.repo.clone()),
        }
    }
}

fn model(rows: Vec<AddonRow>) -> ModelRc<AddonRow> {
    ModelRc::new(VecModel::from(rows))
}

fn initial(name: &str) -> SharedString {
    name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default().into()
}

/// Whether the picture at `url` was tried and could not be loaded.
fn picture_failed(core: &Core, url: Option<&str>) -> bool {
    matches!(url.and_then(|u| core.pictures.get(u)), Some(None))
}

/// The downloaded picture at `url`; empty (width 0) while it loads, when it failed or without one.
fn picture(core: &Core, url: Option<&str>) -> slint::Image {
    match url.and_then(|u| core.pictures.get(u)) {
        Some(Some(pixels)) => slint::Image::from_rgba8(pixels.clone()),
        _ => slint::Image::default(),
    }
}

/// The icon of the platform addon `id` (an installed entry or a Browse row).
fn icon(core: &Core, id: &str) -> slint::Image {
    let slug = registry_entry(core, id).map_or(id, |a| a.id.as_str());
    picture(core, core.platform.icons.get(slug).map(String::as_str))
}

fn installed_row(row: &Row, core: &Core) -> AddonRow {
    let (status, kind, action) = match &row.state {
        RowState::Unchecked => (if core.checking { "Checking..." } else { "Not checked" }.to_string(), 0, ""),
        RowState::UpToDate => ("Up to date".into(), 1, ""),
        // Kept at an older version on purpose: the update is offered, not pushed.
        RowState::UpdateAvailable(label) if core.config.held.contains(&row.id) => (format!("Kept at this version, {label} available"), 0, "Update"),
        RowState::UpdateAvailable(label) => (format!("Update: {label}"), 2, "Update"),
        RowState::Manual => ("Installed from zip".into(), 0, ""),
        RowState::CheckFailed(_) => ("Check failed".into(), 3, ""),
        RowState::Missing => ("Folder missing".into(), 3, if row.badge == Badge::Zip { "" } else { "Reinstall" }),
        RowState::Unmanaged => ("Found in the AddOns folder".into(), 0, ""),
    };
    let warning = match &row.state {
        RowState::CheckFailed(message) => message.clone(),
        _ if row.out_of_date => "Interface is not 11200: enable \"Load out of date AddOns\" in the game".into(),
        _ => String::new(),
    };
    let (status, kind, progress) = row_progress(core, &row.id).unwrap_or((status, kind, -1.0));
    AddonRow {
        id: row.id.clone().into(),
        name: row.name.clone().into(),
        initial: initial(&row.name),
        icon: icon(core, &row.id),
        author: row.author.clone().into(),
        version: row.version.clone().into(),
        badge: row.badge.label().into(),
        // "Verified" reads green wherever it shows, as on the website.
        badge_tone: if row.badge == Badge::Verified { tone_kind(forge_core::trust::Tone::Ok) } else { 0 },
        description: row.description.clone().into(),
        status: status.into(),
        status_kind: kind,
        progress,
        action: action.into(),
        busy: core.busy.contains(&row.id),
        can_remove: true,
        can_open: row.repo.is_some(),
        warning: warning.into(),
        can_pick: false,
        picked: false,
    }
}

/// A running install's step, else how the last one ended: (status, status kind, bar or `-1`).
fn row_progress(core: &Core, id: &str) -> Option<(String, i32, f32)> {
    if let Some(progress) = core.progress.get(id) {
        let (text, bar) = controller::progress_label(*progress);
        return Some((text, 0, bar));
    }
    if core.busy.contains(id) {
        return Some(("Working...".into(), 0, -1.0));
    }
    core.outcomes.get(id).map(|(text, failed)| (text.clone(), if *failed { 3 } else { 1 }, -1.0))
}

/// Why a Browse card has no Install button.
fn not_installable(trust: Option<forge_core::trust::Trust>) -> &'static str {
    match trust {
        Some(forge_core::trust::Trust::SourceChanged) => "Repo changed: waits for a new check",
        _ => "Blocked by the admins",
    }
}

/// A Browse card. An installed addon shows its card from Installed (status, Update, Remove), only
/// with the pack box Browse adds.
fn browse_row(b: controller::BrowseRow, core: &Core, rows: &[Row]) -> AddonRow {
    let can_pick = core.registry.get(&b.id).is_some_and(controller::packable);
    let picked = core.picked.contains(&b.id);
    if let Some(row) = rows.iter().find(|r| r.id == b.id) {
        return AddonRow { can_pick, picked, ..installed_row(row, core) };
    }
    let (status, status_kind) = match (b.installed, b.installable) {
        (true, _) => ("Installed".to_string(), 1),
        (false, false) => (not_installable(core.registry.get(&b.id).and_then(|a| a.latest.as_ref()).map(|l| l.trust)).to_string(), 3),
        (false, true) => (String::new(), 0),
    };
    let (status, status_kind, progress) = row_progress(core, &b.id).unwrap_or((status, status_kind, -1.0));
    AddonRow {
        initial: initial(&b.name),
        icon: icon(core, &b.id),
        id: b.id.clone().into(),
        name: b.name.into(),
        author: b.author.into(),
        version: SharedString::new(),
        badge: b.trust.map_or(b.category, |(label, _)| label.to_string()).into(),
        badge_tone: b.trust.map_or(0, |(_, tone)| tone_kind(tone)),
        description: b.description.into(),
        status: status.into(),
        status_kind,
        progress,
        action: if b.installed || !b.installable { "" } else { "Install" }.into(),
        busy: core.busy.contains(&b.id),
        can_remove: false,
        can_open: true,
        can_pick,
        picked,
        warning: SharedString::new(),
    }
}

/// The platform entry behind an installed entry or a Browse row; `None` for GitHub and zip addons.
fn registry_entry<'a>(core: &'a Core, id: &str) -> Option<&'a registry::VerifiedAddon> {
    match core.state.get(id).map(|e| &e.source) {
        Some(Source::Verified { registry_id }) => core.registry.get(registry_id),
        Some(_) => None,
        None => core.registry.get(id),
    }
}

/// The detail view of `core.detail`, built like its row in the list it was opened from.
fn detail(core: &Core, rows: &[Row]) -> Option<AddonDetail> {
    let (id, browse) = core.detail.as_ref()?;
    let (row, folders) = if *browse {
        let b = controller::browse_rows(&core.registry, &core.state).into_iter().find(|b| &b.id == id)?;
        (browse_row(b, core, rows), Vec::new())
    } else {
        let r = rows.iter().find(|r| &r.id == id)?;
        (installed_row(r, core), r.folders.clone())
    };
    let entry = registry_entry(core, id);
    let mut row = row;
    if let (true, Some(latest)) = (row.version.is_empty(), entry.and_then(|a| a.latest.as_ref())) {
        row.version = controller::display_version(&latest.tag).into();
    }
    let facts = controller::details(entry, &core.registry, &folders);
    // What was stored shows at once; only without it does the page wait for the platform.
    let loaded = entry.and_then(|a| core.platform.info.get(&a.id)).cloned();
    let about_loading = loaded.is_none() && entry.is_some_and(|a| core.fetching.contains(&a.id));
    let about_failed = loaded.is_none() && entry.is_some_and(|a| !core.fetching.contains(&a.id));
    let loaded = loaded.unwrap_or_default();
    let base = loaded.image_origin().unwrap_or_default();
    let mut about: Vec<AboutBlock> = forge_core::markdown::blocks(&loaded.about, &base).into_iter().map(|b| about_block(core, b)).collect();
    if about.is_empty() {
        about.push(about_block(core, forge_core::markdown::Block::Paragraph(row.description.to_string())));
    }
    let shots: Vec<Shot> = loaded.shots.iter().map(|(url, caption)| Shot { image: picture(core, Some(url)), caption: caption.into(), failed: picture_failed(core, Some(url)) }).collect();
    Some(AddonDetail {
        row,
        category: facts.category.into(),
        safety: facts.safety.into(),
        game: facts.game.into(),
        folders: facts.folders.into(),
        needs: facts.needs.into(),
        about: ModelRc::new(VecModel::from(about)),
        shots: ModelRc::new(VecModel::from(shots)),
        about_loading,
        about_failed,
        on_website: entry.is_some(),
        bug_label: entry.map(|a| controller::bug_report("", &a.id, &a.repo, loaded.issues_on, core.platform.issues_ready.get(&a.id).copied()).0).unwrap_or_default().into(),
        issues_ready: loaded.issues_on && entry.is_some_and(|a| core.platform.issues_ready.get(&a.id) == Some(&true)),
        comments_on: loaded.comments_on,
        can_auto_update: !*browse && matches!(core.state.get(id).map(|e| &e.source), Some(Source::Verified { .. })),
        auto_update: core.config.auto_updates(id),
        auto_update_all: core.config.auto_update_all,
        held: core.config.held.iter().any(|h| h == id),
    })
}

/// One block of a description for the detail view; `kind` as `AboutBlock` in components.slint.
fn about_block(core: &Core, block: forge_core::markdown::Block) -> AboutBlock {
    use forge_core::markdown::Block;
    let (kind, marker, text, url) = match block {
        Block::Paragraph(t) => (0, String::new(), t, None),
        Block::Heading(t) => (1, String::new(), t, None),
        Block::Item { marker, text } => (2, marker, text, None),
        Block::Code(t) => (3, String::new(), t, None),
        Block::Quote(t) => (4, String::new(), t, None),
        Block::Image { url, alt } => (5, String::new(), alt, Some(url)),
    };
    AboutBlock { kind, marker: marker.into(), text: text.into(), image: picture(core, url.as_deref()), failed: picture_failed(core, url.as_deref()) }
}

/// Loads the pictures at `urls` that are not loaded yet, one after the other on this thread, and
/// redraws after each. A failed one stays empty until the next start.
fn load_pictures(weak: &slint::Weak<AppWindow>, shared: &Shared, client: &Client, urls: Vec<String>, edge: u32) {
    use survivorsforge::pictures;
    let mut todo = Vec::new();
    let cache = {
        let mut core = lock(shared);
        for url in urls {
            if !core.pictures.contains_key(&url) {
                core.pictures.insert(url.clone(), None);
                todo.push(url);
            }
        }
        core.data_dir.join("cache").join("images")
    };
    for url in todo {
        // A named picture comes from the cache while its hash matches, else once from the platform.
        let bytes = match pictures::content_hash(&url) {
            Some(hash) => pictures::cached(&cache, hash).or_else(|| {
                let bytes = client.get_image(&url, pictures::MAX_BYTES).ok()?;
                pictures::store(&cache, hash, &bytes).then_some(bytes)
            }),
            None => client.get_image(&url, pictures::MAX_BYTES).ok(),
        };
        let picture = bytes.and_then(|b| pictures::decode(&b, edge));
        let Some(p) = picture else { continue };
        lock(shared).pictures.insert(url, Some(Pixels::clone_from_slice(&p.rgba, p.width, p.height)));
        let shared = shared.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    }
}

/// Learns the icons of all platform addons and loads them, on a thread of its own.
fn fetch_icons(ui: &AppWindow, shared: &Shared) {
    let (client, api) = {
        let core = lock(shared);
        (core.client(), core.api_url())
    };
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        // The icons of the last start come from the disk first, also offline.
        let stored: Vec<String> = lock(&shared).platform.icons.values().cloned().collect();
        load_pictures(&weak, &shared, &client, stored, ICON_EDGE);
        let mut icons = Vec::new();
        // ponytail: at most 20 pages of 50 addons; more pages when the platform grows past 1000.
        for page in 1..=20 {
            let Some((found, more)) = client.get_text(&format!("{api}/v1/addons?per_page=50&page={page}")).ok().and_then(|t| controller::icon_page(&t)) else { break };
            icons.extend(found);
            if !more {
                break;
            }
        }
        if icons.is_empty() {
            return;
        }
        let urls = icons.iter().map(|(_, url)| url.clone()).collect();
        {
            let mut core = lock(&shared);
            core.platform.icons.extend(icons);
            core.save_platform();
        }
        load_pictures(&weak, &shared, &client, urls, ICON_EDGE);
    });
}

/// The pictures of a detail page: icon, screenshots and the images of the description.
fn detail_pictures(info: &controller::AddonInfo) -> (Vec<String>, Vec<String>) {
    let base = info.image_origin().unwrap_or_default();
    let mut shots: Vec<String> = info.shots.iter().map(|(url, _)| url.clone()).collect();
    shots.extend(forge_core::markdown::blocks(&info.about, &base).into_iter().filter_map(|b| match b {
        forge_core::markdown::Block::Image { url, .. } => Some(url),
        _ => None,
    }));
    (info.icon.clone().into_iter().collect(), shots)
}

/// Where bugs go, asked on every open of the details: a creator may turn the Issues tab on or let
/// the App write issues while the launcher runs. Until the platform says it takes them, the button
/// sends them to GitHub; offline the stored answer stays.
fn ask_issue_frame(weak: &slint::Weak<AppWindow>, shared: &Shared, client: &Client, api: &str, rid: &str) {
    let Some((here, ready)) = client.get_text_quick(&format!("{api}/v1/addons/{rid}/issues/frame")).ok().and_then(|t| controller::issue_frame(&t)) else {
        return;
    };
    let mut core = lock(shared);
    if core.take_issue_frame(rid, here, ready) {
        core.save_platform();
        drop(core);
        let redraw = shared.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &redraw));
    }
}

/// Opens the detail view of a row. What was stored shows at once, its pictures from the disk; the
/// platform is asked once per start in the background and replaces it when it answers. Where bugs
/// go is asked on every open.
fn on_row_details(ui: &AppWindow, shared: &Shared, id: String) {
    ui.set_detail_tab(0);
    let job = {
        let mut core = lock(shared);
        core.comments = None;
        core.issues = None;
        core.versions = None;
        core.detail = Some((id.clone(), ui.get_page() == 1));
        registry_entry(&core, &id).map(|a| a.id.clone()).map(|rid| {
            let ask = !core.refreshed.contains(&rid) && core.fetching.insert(rid.clone());
            (core.platform.info.get(&rid).cloned(), ask, core.client(), core.api_url(), rid)
        })
    };
    refresh(ui, shared);
    ui.set_detail_page(ui.get_page());
    let Some((stored, ask, client, api, rid)) = job else { return };
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        if let Some(stored) = &stored {
            let (icon, shots) = detail_pictures(stored);
            load_pictures(&weak, &shared, &client, icon, ICON_EDGE);
            load_pictures(&weak, &shared, &client, shots, SHOT_EDGE);
        }
        if !ask {
            return ask_issue_frame(&weak, &shared, &client, &api, &rid);
        }
        // Offline the stored page stays (or the summary without one); the next start asks again.
        let fresh = client.get_text(&format!("{api}/v1/addons/{rid}")).ok().and_then(|t| controller::addon_info(&t));
        {
            let mut core = lock(&shared);
            core.fetching.remove(&rid);
            if let Some(info) = &fresh {
                core.refreshed.insert(rid.clone());
                if core.platform.info.get(&rid) != Some(info) {
                    if let Some(icon) = &info.icon {
                        core.platform.icons.insert(rid.clone(), icon.clone());
                    }
                    core.platform.info.insert(rid.clone(), info.clone());
                    core.save_platform();
                }
            }
        }
        let redraw = shared.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &redraw));
        ask_issue_frame(&weak, &shared, &client, &api, &rid);
        if let Some(info) = fresh {
            let (icon, shots) = detail_pictures(&info);
            load_pictures(&weak, &shared, &client, icon, ICON_EDGE);
            load_pictures(&weak, &shared, &client, shots, SHOT_EDGE);
        }
    });
}

/// The order of two addons (id, name) in Browse and Installed, by the platform's downloads and newest
/// release; GitHub and zip addons have neither.
fn list_order<'a>(core: &'a Core, a: (&str, &'a str), b: (&str, &'a str)) -> std::cmp::Ordering {
    let key = |(id, name): (&str, &'a str)| {
        let entry = registry_entry(core, id);
        let updated = entry.and_then(|e| e.latest.as_ref()).and_then(|l| l.published_at.as_deref());
        controller::SortKey { name, downloads: entry.and_then(|e| e.downloads), updated }
    };
    controller::compare(core.sort, &key(a), &key(b))
}

fn refresh(ui: &AppWindow, shared: &Shared) {
    let core = lock(shared);
    // The game version list arrives with the update check; fill it once it is there.
    if slint::Model::row_count(&ui.get_settings_game_versions()) != core.game_versions.len() + 1 {
        fill_game_versions(ui, &core);
    }
    let rows = controller::installed_rows(&core.state, &core.reconciled, &core.scan, &core.registry, &core.statuses);
    ui.set_installed_total(rows.len() as i32);
    let choices = controller::category_choices(&core.categories, &core.registry);
    // Search, category and safety check narrow both lists; both sort the same way.
    let shown = |id: &str, name: &str, author: &str, category: Option<&str>| {
        let entry = registry_entry(&core, id);
        controller::matches(&core.search, name, author)
            && core.category.as_deref().is_none_or(|w| category.is_some_and(|c| controller::in_category(c, w, &choices)))
            && controller::passes_safety(core.safety, entry.and_then(|a| a.latest.as_ref()).map(|l| l.trust))
    };
    let mut installed: Vec<&Row> = rows.iter().filter(|r| shown(&r.id, &r.name, &r.author, registry_entry(&core, &r.id).map(|a| a.category.as_str()))).collect();
    installed.sort_by(|a, b| list_order(&core, (&a.id, &a.name), (&b.id, &b.name)));
    controller::updates_first(&mut installed, &core.config.held);
    // Game Mode opens no browser, so the rows offer no GitHub button there (as the detail view).
    let links = !ui.get_game_mode();
    let linked = |row: AddonRow| AddonRow { can_open: row.can_open && links, ..row };
    ui.set_installed_rows(model(installed.into_iter().map(|r| linked(installed_row(r, &core))).collect()));
    if slint::Model::row_count(&ui.get_category_labels()) != choices.len() + 1 {
        let mut labels: Vec<SharedString> = vec!["All categories".into()];
        labels.extend(choices.iter().map(|c| SharedString::from(c.label())));
        let index = core.category.as_ref().and_then(|w| choices.iter().position(|c| c.name.eq_ignore_ascii_case(w))).map_or(0, |i| i + 1);
        ui.set_category_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
        let (list, levels): (Vec<SharedString>, Vec<i32>) =
            std::iter::once(("All categories".to_string(), 0)).chain(controller::list_entries(&choices)).map(|(l, n)| (SharedString::from(l), n)).unzip();
        ui.set_category_list_labels(slint::ModelRc::new(slint::VecModel::from(list)));
        ui.set_category_levels(slint::ModelRc::new(slint::VecModel::from(levels)));
        ui.set_category_index(index as i32);
    }
    ui.set_picked_count(core.picked.len() as i32);
    ui.set_pack_busy(core.pack_busy);
    let mut browse: Vec<controller::BrowseRow> =
        controller::browse_rows(&core.registry, &core.state).into_iter().filter(|b| shown(&b.id, &b.name, &b.author, Some(&b.category))).collect();
    browse.sort_by(|a, b| list_order(&core, (&a.id, &a.name), (&b.id, &b.name)));
    ui.set_browse_rows(model(browse.into_iter().map(|b| linked(browse_row(b, &core, &rows))).collect()));
    ui.set_offline(core.offline);
    let me = core.me.clone().unwrap_or_default();
    ui.set_account_login(me.login.as_str().into());
    ui.set_account_initial(me.login.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default().into());
    ui.set_account_avatar(picture(&core, (!me.avatar_url.is_empty()).then_some(me.avatar_url.as_str())));
    ui.set_account_busy(core.login_cancel.is_some());
    ui.set_account_error(core.login_error.as_str().into());
    ui.set_account_github_only(core.github_only.clone().unwrap_or_default().into());
    ui.set_account_device_code(core.device_code.as_str().into());
    ui.set_device_login_offered(core.offline && survivorsforge::device_login::client_id().is_some());
    match detail(&core, &rows) {
        Some(detail) => ui.set_detail(detail),
        // Removed meanwhile: back to the list.
        None => ui.set_detail_page(-1),
    }
    let (issues, open, loading, failed) = match &core.issues {
        Some(Listing { rows: Some((rows, open)), .. }) => (rows.iter().map(issue_line).collect(), *open as i32, false, false),
        Some(l) => (Vec::new(), 0, !l.failed, l.failed),
        None => (Vec::new(), 0, false, false),
    };
    ui.set_issues_rows(ModelRc::new(VecModel::from(issues)));
    ui.set_issues_open(open);
    ui.set_issues_loading(loading);
    ui.set_issues_failed(failed);
    let view = &core.issue_view;
    ui.set_issues_mode(view.mode);
    ui.set_issues_sending(view.sending);
    ui.set_issues_send_error(view.send_error.as_str().into());
    ui.set_issues_needs_login(view.needs_login);
    ui.set_issue_number(view.number as i32);
    ui.set_issue_loading(view.loading);
    ui.set_issue_failed(view.failed);
    let page = view.page.clone().unwrap_or_default();
    ui.set_issue_title(page.title.as_str().into());
    ui.set_issue_meta(format!("Opened {} by {}", page.date, page.author).into());
    ui.set_issue_body(page.body.as_str().into());
    ui.set_issue_closed(page.state == "closed");
    ui.set_issue_locked(page.locked);
    let replies: Vec<IssueReplyRow> =
        page.replies.into_iter().map(|r| IssueReplyRow { author: r.author.into(), date: r.date.into(), body: r.body.into() }).collect();
    ui.set_issue_replies(ModelRc::new(VecModel::from(replies)));
    let verified_author = core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)).is_some_and(|a| a.verified_author == Some(true));
    let installed = core.detail.as_ref().and_then(|(id, _)| core.state.get(id)).map(|e| e.version.clone()).unwrap_or_default();
    let held = core.detail.as_ref().is_some_and(|(id, _)| core.config.held.contains(id));
    let (versions, loading, failed) = match &core.versions {
        Some(Listing { rows: Some(rows), .. }) => {
            // The newest is the first that is no pre-release, as the platform installs it.
            let newest = rows.iter().position(|v| !v.prerelease);
            (rows.iter().enumerate().map(|(i, v)| version_line(v, verified_author, &installed, Some(i) == newest, held)).collect(), false, false)
        }
        Some(l) => (Vec::new(), !l.failed, l.failed),
        None => (Vec::new(), false, false),
    };
    ui.set_versions_rows(ModelRc::new(VecModel::from(versions)));
    ui.set_versions_loading(loading);
    ui.set_versions_failed(failed);
    if let Some(c) = &core.comments {
        let rows: Vec<CommentRow> = controller::comment_threads(c.loaded.clone()).into_iter().map(|m| comment_row(&core, m)).collect();
        ui.set_comments_rows(ModelRc::new(VecModel::from(rows)));
        // Placeholders stand in for the first page only; older ones load under their button.
        let first = c.loaded.is_empty();
        ui.set_comments_waiting(match c.asked.filter(|_| first).map(|t| controller::waiting(t.elapsed())) {
            Some(controller::Waiting::Placeholders) => 1,
            Some(controller::Waiting::Slow) => 2,
            // Asked, too soon for placeholders: nothing yet, and no "No comments yet." either.
            Some(_) => 3,
            None => 0,
        });
        ui.set_comments_loading_more(c.asked.is_some() && !first);
        ui.set_comments_failed(c.failed);
        ui.set_comments_off(c.off);
        ui.set_comments_more(c.older.is_some());
        ui.set_comments_reply_to(c.reply_to.as_ref().map(|(_, who)| who.as_str()).unwrap_or_default().into());
        ui.set_comments_compose_open(c.composing);
        ui.set_comments_sending(c.sending);
        ui.set_comments_send_error(c.send_error.as_str().into());
        ui.set_comments_needs_login(c.needs_login);
    }
    ui.set_update_count(controller::update_all_ids(&core.statuses, &core.config.held).len() as i32);
    ui.set_checking(core.checking);
    let game_missing = match config::game_executable(core.addons_dir(), USE_WINDOWS_GAME) {
        Some(_) => "",
        None if USE_WINDOWS_GAME => "Can't launch: AzerothSurvivors.exe was not found next to benilla-config. Check the AddOns folder in Settings.",
        None => "Can't launch: no Linux build of the game next to benilla-config. Start a Windows build through Steam, or check the AddOns folder in Settings.",
    };
    ui.set_game_missing(game_missing.into());
    ui.set_game_running(core.game_running);
    ui.set_settings_game_versions_failed(core.game_versions_failed);
    ui.set_addons_busy(!core.busy.is_empty());
    ui.set_game_updating(core.game_updating);
    let (label, hint) = match &core.game_plan {
        Some(plan) if core.game_updating => (String::new(), if plan.exe.is_some() { format!("Updating to v{}", plan.version) } else { "Updating the game data".into() }),
        Some(plan) if plan.exe.is_some() => (
            format!("Update to v{}", plan.version),
            core.game_exe.as_ref().and_then(|e| e.version.clone()).map(|v| format!("You have v{v}.")).unwrap_or_default(),
        ),
        Some(_) => ("Update game data".to_string(), "New game data is out.".to_string()),
        None => (String::new(), core.game_note.clone()),
    };
    ui.set_game_update(label.into());
    ui.set_game_hint(hint.into());
    let (step, bar) = core.game_step.clone().unwrap_or_default();
    ui.set_game_step(step.into());
    ui.set_game_progress(bar);
    // Row by row into the same model: a toast that stays keeps its timer.
    let toasts: Vec<Toast> = core.toasts.items().iter().map(|t| Toast { id: t.id, text: t.text.as_str().into(), kind: t.kind as i32 }).collect();
    let model = ui.get_toasts();
    match slint::Model::as_any(&model).downcast_ref::<VecModel<Toast>>() {
        Some(rows) => slint_toast::sync(rows, toasts, |t| t.id),
        None => ui.set_toasts(ModelRc::new(VecModel::from(toasts))),
    }
    ui.set_addons_dir(core.config.addons_dir.to_string_lossy().into_owned().into());
    ui.set_self_update_version(core.self_update.as_ref().map(|a| a.version.clone()).unwrap_or_default().into());
}

/// Runs `job` off the UI thread, then shows its result in the status bar and refreshes the UI.
fn run_bg(
    weak: slint::Weak<AppWindow>,
    shared: Shared,
    busy_id: Option<String>,
    job: impl FnOnce(&Shared) -> survivorsforge::Result<String> + Send + 'static,
) {
    std::thread::spawn(move || {
        let result = job(&shared);
        {
            let mut core = lock(&shared);
            if let Some(id) = &busy_id {
                core.busy.remove(id);
            }
            match result {
                // A job for one addon changed its folders.
                Ok(message) if busy_id.is_some() => {
                    let message = controller::note_running_game(message, core.game_running);
                    core.notify(message, Kind::Success)
                }
                Ok(message) => core.notify(message, Kind::Success),
                Err(e) => core.notify(e.to_string(), Kind::Error),
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

thread_local! {
    /// The action behind the open confirmation dialog. Lives on the UI thread only.
    static PENDING: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    /// The action behind its third button, when it has one.
    static PENDING_EXTRA: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
}

fn ask(ui: &AppWindow, title: &str, text: &str, ok: &str, danger: bool, action: impl FnOnce() + 'static) {
    ui.set_confirm_title(title.into());
    ui.set_confirm_text(text.into());
    ui.set_confirm_ok(ok.into());
    ui.set_confirm_danger(danger);
    ui.set_confirm_extra(SharedString::new());
    ui.set_confirm_info(false);
    ui.set_confirm_open(true);
    PENDING.with(|p| *p.borrow_mut() = Some(Box::new(action)));
    PENDING_EXTRA.with(|p| p.borrow_mut().take());
}

/// Like `ask`, with "Scan locally" next to OK: `scan` checks the addon on this computer first.
fn ask_or_scan(ui: &AppWindow, title: &str, text: &str, ok: &str, danger: bool, scan: impl FnOnce() + 'static, action: impl FnOnce() + 'static) {
    ask(ui, title, text, ok, danger, action);
    ui.set_confirm_extra("Scan locally".into());
    PENDING_EXTRA.with(|p| *p.borrow_mut() = Some(Box::new(scan)));
}

/// Shows a result with only an OK button.
fn tell(ui: &AppWindow, title: &str, text: &str) {
    ask(ui, title, text, "OK", false, || {});
    ui.set_confirm_info(true);
}

/// Runs the local scan `job` off the UI thread, then hands its report to `then` on the UI thread.
fn scan_then<T: Send + 'static>(
    ui: &AppWindow,
    shared: &Shared,
    name: &str,
    job: impl FnOnce(&Shared) -> survivorsforge::Result<T> + Send + 'static,
    then: impl FnOnce(&AppWindow, &Shared, T) + Send + 'static,
) {
    lock(shared).set_status(format!("Scanning {name} locally..."));
    refresh(ui, shared);
    let (weak, shared, name) = (ui.as_weak(), shared.clone(), name.to_string());
    std::thread::spawn(move || {
        let result = job(&shared);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            match result {
                Ok(report) => {
                    lock(&shared).notify(format!("{name} scanned locally"), Kind::Success);
                    then(&ui, &shared, report);
                }
                Err(e) => lock(&shared).notify(format!("Could not scan {name}: {e}"), Kind::Error),
            }
            refresh(&ui, &shared);
        });
    });
}

/// The confirmation text after a local scan: what was found, then what the dialog said before.
fn scanned_text(report: &forge_scan::Report, before: &str) -> String {
    let found = local_scan::summary(report);
    if before.is_empty() { found } else { format!("{found}\n\n{before}") }
}

/// Downloads the file `id` would install, scans it here, then offers to install exactly that file.
/// `picked` is a version from the Versions tab; `None` scans the one Install would take.
fn scan_remote(ui: &AppWindow, shared: &Shared, id: String, deps: Vec<String>, title: String, picked: Option<RemoteVersion>) {
    let job_id = id.clone();
    let name = lock(shared).state.get(&id).map(|e| e.name.clone()).unwrap_or_else(|| id.clone());
    scan_then(
        ui,
        shared,
        &name,
        move |shared| {
            let (job, client, github, downloads) = {
                let core = lock(shared);
                (remote_job(&core, &job_id), core.client(), core.github(), core.downloads())
            };
            let (_, repo, spec, _, remote) = job.ok_or(Error::NotFound)??;
            let mut remote = match picked.or(remote) {
                Some(remote) => remote,
                None => updates::resolve_remote(&*github, &repo, &spec)?,
            };
            let zip = downloads.join(format!("{}.zip", unique_name()));
            client.download(&remote.download, &zip)?;
            let scanned = (|| {
                let folders: Vec<String> = archive::analyze(&zip).map_err(Error::from)?.into_iter().map(|a| a.name).collect();
                remote.sha256 = Some(install::sha256_of(&zip)?);
                local_scan::scan_zip(&zip, &folders)
            })();
            let _ = std::fs::remove_file(&zip);
            Ok((scanned?, remote))
        },
        move |ui, shared, (report, pinned)| {
            let text = scanned_text(&report, "Install installs exactly the file that was scanned.");
            let (weak, shared) = (ui.as_weak(), shared.clone());
            ask(ui, &title, &text, "Install", local_scan::flagged(&report), move || {
                if let Some(ui) = weak.upgrade() {
                    start_install(&ui, &shared, id, deps, Some(pinned));
                }
            });
        },
    );
}

/// Downloads `remote` (or the latest version of `repo`) and installs it as entry `id`, and counts
/// the install, update or rollback (`older`) of a verified addon for the platform's statistics.
#[allow(clippy::too_many_arguments)]
fn install_remote(
    shared: &Shared,
    id: &str,
    name: &str,
    repo: &str,
    spec: &SourceSpec,
    source: Source,
    remote: Option<RemoteVersion>,
    older: bool,
) -> survivorsforge::Result<String> {
    let addon = telemetry::reported_addon(&source).map(str::to_string);
    let update = lock(shared).state.get(id).is_some();
    let mut version = remote.as_ref().map(|r| r.id.clone());
    let result = download_and_install(shared, id, name, repo, spec, source, remote, &mut version);
    let outcome = match &result {
        Ok(_) => {
            let shown = controller::display_version(version.as_deref().unwrap_or_default());
            (format!("{} {shown}", if update { "Updated to" } else { "Installed" }), false)
        }
        Err(e) => (format!("Failed: {e}"), true),
    };
    show_progress(shared, true, |core| {
        core.progress.remove(id);
        core.outcomes.insert(id.to_string(), outcome);
    });
    let event = addon.and_then(|addon| telemetry::event(update, older, &addon, version.as_deref().unwrap_or("unknown"), result.as_ref().map(|_| ())));
    if let Some(event) = event {
        let core = lock(shared);
        telemetry::report(&core.config, &ApiSender { api: core.api_url() }, &[event]);
    }
    result
}

/// The work of `install_remote`; `version` learns the version once it is known.
#[allow(clippy::too_many_arguments)]
fn download_and_install(
    shared: &Shared,
    id: &str,
    name: &str,
    repo: &str,
    spec: &SourceSpec,
    source: Source,
    remote: Option<RemoteVersion>,
    version: &mut Option<String>,
) -> survivorsforge::Result<String> {
    let (client, github, downloads, addons) = {
        let core = lock(shared);
        (core.client(), core.github(), core.downloads(), core.addons_dir().to_path_buf())
    };
    let remote = match remote {
        Some(remote) => remote,
        None => updates::resolve_remote(&*github, repo, spec)?,
    };
    *version = Some(remote.id.clone());
    let zip = downloads.join(format!("{}.zip", unique_name()));
    show_progress(shared, true, |core| {
        core.progress.insert(id.to_string(), controller::Progress::Downloading { done: 0, total: None });
    });
    client.download_with(&remote.download, &zip, &mut |done, total| {
        show_progress(shared, false, |core| {
            core.progress.insert(id.to_string(), controller::Progress::Downloading { done, total });
        });
    })?;
    show_progress(shared, true, |core| {
        core.progress.insert(id.to_string(), controller::Progress::Installing);
    });
    let mut core = lock(shared);
    let request = Request { zip: &zip, id, name, source, version: remote.id.clone(), expected_sha256: remote.sha256.clone() };
    let result = install::install_zip(&addons, &mut core.state, request);
    let _ = std::fs::remove_file(&zip);
    result?;
    core.save_state();
    core.statuses.insert(id.to_string(), UpdateStatus::UpToDate);
    core.rescan();
    Ok(format!("{name} {} installed", remote.label))
}

/// The install job for an entry or verified addon, or `None` when it has no remote source.
/// (name, repo, spec, source, already known remote version)
type RemoteJob = (String, String, SourceSpec, Source, Option<RemoteVersion>);

fn remote_job(core: &Core, id: &str) -> Option<survivorsforge::Result<RemoteJob>> {
    let known = match core.statuses.get(id) {
        Some(UpdateStatus::UpdateAvailable(remote)) => Some(remote.clone()),
        _ => None,
    };
    if let Some(entry) = core.state.get(id) {
        return updates::remote_of(&entry.source, &core.registry)
            .map(|r| r.map(|(repo, spec)| (entry.name.clone(), repo, spec, entry.source.clone(), known)));
    }
    core.registry.get(id).map(|a| {
        let platform = updates::platform_remote(a).transpose()?;
        Ok((a.name.clone(), a.repo.clone(), a.source.clone(), Source::Verified { registry_id: a.id.clone() }, platform))
    })
}

/// Installs right away, or asks first when the platform has a reason to warn.
fn on_row_action(ui: &AppWindow, shared: &Shared, id: String) {
    let decision = {
        let mut core = lock(shared);
        let registry_id = match core.state.get(&id).map(|e| &e.source) {
            Some(Source::Verified { registry_id }) => registry_id.clone(),
            Some(_) => String::new(),
            None => id.clone(),
        };
        let game = core.config.game_version.clone();
        match core.registry.get(&registry_id).cloned() {
            None => Ok(None),
            Some(addon) => {
                let plan = controller::dependency_plan(&registry_id, &core.registry, &core.state);
                if !plan.unavailable.is_empty() {
                    let msg = format!("{} needs {}, which cannot be installed right now", addon.name, plan.unavailable.join(", "));
                    core.notify(msg, Kind::Error);
                    Err(())
                } else {
                    let mut lines = Vec::new();
                    let mut danger = false;
                    let mut warned = false;
                    if !plan.install.is_empty() {
                        let names: Vec<String> = plan.install.iter().map(|d| core.registry.get(d).map(|a| a.name.clone()).unwrap_or(d.clone())).collect();
                        lines.push(format!("It needs these addons, which are installed first: {}.", names.join(", ")));
                    }
                    for target in plan.install.iter().map(String::as_str).chain(std::iter::once(registry_id.as_str())) {
                        let Some(a) = core.registry.get(target) else { continue };
                        let warnings = controller::install_warnings(a, game.as_deref(), &core.game_versions);
                        if !warnings.is_empty() {
                            let (_, text, red) = controller::warning_dialog(&a.name, &warnings);
                            danger |= red;
                            warned = true;
                            lines.push(format!("{}: {text}", a.name));
                        }
                    }
                    if !plan.optional.is_empty() {
                        lines.push(format!("Works with, not installed: {}.", plan.optional.join(", ")));
                    }
                    if let Some(label) = controller::unchecked_update(&core.state, &core.statuses, &id) {
                        danger = true;
                        warned = true;
                        lines.insert(0, format!(
                            "{label} is not checked for harmful code: the SurvivorsForge server cannot be reached and it is newer than the last checked version. It comes straight from GitHub ({})."
                            , addon.repo
                        ));
                    }
                    let unchecked = controller::unchecked_update(&core.state, &core.statuses, &id).is_some();
                    let asks = !plan.install.is_empty() || warned;
                    Ok(asks.then(|| (format!("Install {}?", addon.name), lines.join("\n\n"), danger, plan.install, unchecked)))
                }
            }
        }
    };
    match decision {
        Err(()) => refresh(ui, shared),
        Ok(None) => start_install(ui, shared, id, Vec::new(), None),
        Ok(Some((title, text, danger, deps, unchecked))) => {
            let weak = ui.as_weak();
            let shared = shared.clone();
            let ok = if danger { "Install anyway" } else { "Install" };
            let install = {
                let (weak, shared, id, deps) = (weak.clone(), shared.clone(), id.clone(), deps.clone());
                move || {
                    if let Some(ui) = weak.upgrade() {
                        start_install(&ui, &shared, id, deps, None);
                    }
                }
            };
            if unchecked {
                // The server cannot check it: the user may check the very file here before installing it.
                let asked = title.clone();
                let scan = move || {
                    if let Some(ui) = weak.upgrade() {
                        scan_remote(&ui, &shared, id, deps, asked, None);
                    }
                };
                ask_or_scan(ui, &title, &text, ok, danger, scan, install);
            } else {
                ask(ui, &title, &text, ok, danger, install);
            }
        }
    }
}

/// Installs the dependencies `deps` in order, then `id`. A failing dependency stops before `id`.
/// `pinned` replaces the version to install, for example the file a local scan checked.
fn start_install(ui: &AppWindow, shared: &Shared, id: String, deps: Vec<String>, pinned: Option<RemoteVersion>) {
    let job = {
        let mut core = lock(shared);
        if id.starts_with(UNMANAGED_PREFIX) || core.busy.contains(&id) {
            return;
        }
        match remote_job(&core, &id) {
            None => return,
            Some(Err(e)) => {
                core.notify(e.to_string(), Kind::Error);
                None
            }
            Some(Ok(job)) => {
                core.busy.insert(id.clone());
                core.outcomes.remove(&id);
                core.set_status(format!("Installing {}...", job.0));
                Some(job)
            }
        }
    };
    refresh(ui, shared);
    let Some((name, repo, spec, source, remote)) = job else { return };
    // An older version than the platform's newest is held there; any other install lets it update again.
    let newest = registry_entry(&lock(shared), &id).and_then(|a| a.latest.as_ref()).map(|l| l.tag.clone());
    let held = pinned.as_ref().is_some_and(|p| newest.is_some_and(|n| n != p.id));
    let remote = pinned.or(remote);
    run_bg(ui.as_weak(), shared.clone(), Some(id.clone()), move |shared| {
        for dep in &deps {
            let job = remote_job(&lock(shared), dep);
            let Some(Ok((dep_name, dep_repo, dep_spec, dep_source, dep_remote))) = job else {
                return Err(Error::NotInstallable(format!("{dep} cannot be installed, so {name} was not installed")));
            };
            install_remote(shared, dep, &dep_name, &dep_repo, &dep_spec, dep_source, dep_remote, false)
                .map_err(|e| Error::NotInstallable(format!("{dep_name} failed ({e}), so {name} was not installed")))?;
        }
        let done = install_remote(shared, &id, &name, &repo, &spec, source, remote, held)?;
        let mut core = lock(shared);
        if core.config.held.contains(&id) != held {
            core.config.hold(&id, held);
            let _ = core.config.save(&core.data_dir);
        }
        Ok(done)
    });
}

/// Keep launch and update mutually exclusive while the worker starts or waits for the game.
fn on_launch_game(ui: &AppWindow, shared: &Shared) {
    let (addons, data_dir) = {
        let mut core = lock(shared);
        if core.game_running || core.game_updating || !core.busy.is_empty() {
            return;
        }
        core.game_running = true;
        (core.addons_dir().to_path_buf(), core.data_dir.clone())
    };
    refresh(ui, shared);

    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let started = config::game_executable(&addons, USE_WINDOWS_GAME)
            .ok_or_else(|| "The game was not found next to the AddOns folder.".to_string())
            .and_then(|exe| start_game(&exe, &data_dir).map_err(|e| format!("Could not start the game: {e}")));
        let (message, kind) = match started {
            Ok(mut child) => {
                lock(&shared).notify("Azeroth Survivors is running.".into(), Kind::Info);
                let repaint = shared.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &repaint));
                match child.wait() {
                    Ok(status) if cfg!(target_os = "macos") && !status.success() => {
                        (format!("Azeroth Survivors exited with {status}."), Kind::Info)
                    }
                    Err(e) if cfg!(target_os = "macos") => (format!("Could not wait for the game: {e}"), Kind::Error),
                    _ => ("Azeroth Survivors was closed.".into(), Kind::Info),
                }
            }
            Err(message) => (message, Kind::Error),
        };
        {
            let mut core = lock(&shared);
            core.game_running = false;
            core.notify(message, kind);
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

fn start_game(exe: &Path, _data_dir: &Path) -> std::io::Result<std::process::Child> {
    #[cfg(target_os = "macos")]
    {
        macos::start_game(exe, _data_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::process::Command::new(exe).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn()
    }
}

/// Applies `change` and redraws the window: at once when `now`, else at most ten times a second.
fn show_progress(shared: &Shared, now: bool, change: impl FnOnce(&mut Core)) {
    let repaint = {
        let mut core = lock(shared);
        change(&mut core);
        let due = now || core.painted.is_none_or(|t| t.elapsed() >= std::time::Duration::from_millis(100));
        if due {
            core.painted = Some(std::time::Instant::now());
        }
        core.repaint.clone().filter(|_| due)
    };
    if let Some(repaint) = repaint {
        repaint();
    }
}

/// The game's folder, two levels above `<game>/benilla-config/AddOns`, when it holds the game's data.
fn game_dir(addons_dir: &Path) -> Option<PathBuf> {
    addons_dir.parent()?.parent().filter(|dir| dir.join("Data").is_dir()).map(Path::to_path_buf)
}

/// The game's exe in `dir` as last hashed (again only once it changed), and the build whose index
/// fits it. Without an exe, the platform's build: its data patches are the same.
fn look_at_game(dir: &Path, seen: Option<&ExeSeen>) -> (Option<ExeSeen>, game_update::Build) {
    let native = if USE_WINDOWS_GAME { game_update::Build::Windows } else { game_update::Build::Linux };
    match game_update::find_exe(dir, !USE_WINDOWS_GAME) {
        Some((path, build)) => (game_update::look_at_exe(&path, seen), build),
        None => (None, native),
    }
}

/// Asks first, with the size and the game's own notes, then updates the game from its server.
fn on_update_game(ui: &AppWindow, shared: &Shared) {
    let (plan, running) = {
        let core = lock(shared);
        (core.game_plan.clone(), core.game_running || core.game_updating)
    };
    let Some(plan) = plan else { return };
    if running {
        return;
    }
    let what = if plan.exe.is_some() { format!("Azeroth Survivors {} is out.", plan.version) } else { "New game data is out.".to_string() };
    let size = (plan.download_size() as f64 / 1_048_576.0).ceil();
    let notes = if plan.notes.is_empty() { String::new() } else { format!("\n\n{}", plan.notes) };
    let text = format!("{what} SurvivorsForge downloads {size} MB from wow.aethus.pl, checks them and puts them in the game's folder. Close the game first.{notes}");
    let (weak, shared) = (ui.as_weak(), shared.clone());
    ask(ui, "Update the game", &text, "Update", false, move || {
        let Some(ui) = weak.upgrade() else { return };
        {
            let mut core = lock(&shared);
            if core.game_running || core.game_updating {
                return;
            }
            core.game_updating = true;
            core.set_status("Updating the game...".into());
        }
        refresh(&ui, &shared);
        run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
            let (client, addons, seen) = {
                let core = lock(shared);
                (core.client(), core.addons_dir().to_path_buf(), core.game_exe.clone())
            };
            let result = (|| {
                let dir = game_dir(&addons).ok_or_else(|| Error::Io("The game's folder with its Data folder was not found".into()))?;
                #[cfg(target_os = "macos")]
                macos::require_game_closed()?;
                let server = WebServer(client);
                // Asked again right before: the server keeps only the newest parts.
                let (exe, build) = look_at_game(&dir, seen.as_ref());
                let plan = game_update::check(&server, &dir, build, exe.as_ref())?;
                let mut progress = |step| {
                    let now = !matches!(step, game_update::Step::Downloading { .. });
                    show_progress(shared, now, |core| core.game_step = Some(game_update::step_label(&step)));
                };
                #[cfg(target_os = "macos")]
                game_update::update_guarded(&server, &dir, &plan, &mut progress, &mut || macos::require_game_closed().map_err(Error::from))?;
                #[cfg(not(target_os = "macos"))]
                game_update::update(&server, &dir, &plan, &mut progress)?;
                let (exe, _) = look_at_game(&dir, None);
                Ok::<_, Error>((plan, exe))
            })();
            let mut core = lock(shared);
            core.game_updating = false;
            core.game_step = None;
            let (plan, exe) = result?;
            core.game_plan = None;
            core.game_exe = exe;
            core.game_note = if plan.exe.is_some() { format!("Updated to v{}.", plan.version) } else { "Game data updated.".into() };
            Ok(if plan.exe.is_some() { format!("Azeroth Survivors is updated to {}", plan.version) } else { "The game's data is up to date".into() })
        });
    });
}

fn on_update_all(ui: &AppWindow, shared: &Shared) {
    let (ids, skipped) = {
        let core = lock(shared);
        let ids: Vec<String> =
            controller::update_all_ids(&core.statuses, &core.config.held).into_iter().filter(|id| !core.busy.contains(id)).collect();
        // Updates that need a confirmation are left for the user to install one by one.
        let (ids, mut skipped) =
            controller::split_confirmations(ids, &core.registry, &core.state, core.config.game_version.as_deref(), &core.game_versions);
        let (ids, unchecked): (Vec<String>, Vec<String>) = ids.into_iter().partition(|id| controller::unchecked_update(&core.state, &core.statuses, id).is_none());
        skipped.extend(unchecked);
        let skipped_names: Vec<String> = skipped.iter().map(|id| core.state.get(id).map(|e| e.name.clone()).unwrap_or(id.clone())).collect();
        (ids, skipped_names)
    };
    install_updates(ui, shared, ids, skipped);
}

/// Installs the updates `ids` one after the other; `skipped` names the ones left for the user.
fn install_updates(ui: &AppWindow, shared: &Shared, ids: Vec<String>, skipped: Vec<String>) {
    let jobs = {
        let mut core = lock(shared);
        let jobs: Vec<_> = ids
            .into_iter()
            .filter_map(|id| match remote_job(&core, &id) {
                Some(Ok(job)) => Some((id, job)),
                _ => None,
            })
            .collect();
        for (id, _) in &jobs {
            core.busy.insert(id.clone());
            core.outcomes.remove(id);
            core.progress.insert(id.clone(), controller::Progress::Waiting);
        }
        core.set_status(format!("Updating {} addons...", jobs.len()));
        jobs
    };
    refresh(ui, shared);
    let weak = ui.as_weak();
    run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
        let total = jobs.len();
        let mut failed = Vec::new();
        for (i, (id, (name, repo, spec, source, remote))) in jobs.into_iter().enumerate() {
            show_progress(shared, true, |core| {
                core.set_status(format!("Updating {} of {total}: {name}", i + 1));
            });
            if let Err(e) = install_remote(shared, &id, &name, &repo, &spec, source, remote, false) {
                failed.push(format!("{name}: {e}"));
            }
            lock(shared).busy.remove(&id);
            let s = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &s));
        }
        let skipped_note = if skipped.is_empty() {
            String::new()
        } else {
            format!(". Needs your confirmation, update one by one: {}", skipped.join(", "))
        };
        if failed.is_empty() && total == 0 && !skipped.is_empty() {
            Ok(format!("Nothing updated. Needs your confirmation, update one by one: {}", skipped.join(", ")))
        } else if failed.is_empty() {
            Ok(controller::note_running_game(format!("Updated {total} addons{skipped_note}"), lock(shared).game_running))
        } else {
            Err(Error::UpdatesFailed { failed: failed.len(), total, details: failed.join(" | ") })
        }
    });
}

fn check_updates(ui: &AppWindow, shared: &Shared) {
    {
        let mut core = lock(shared);
        if core.checking || core.addons_dir().as_os_str().is_empty() {
            return;
        }
        core.checking = true;
        // Each addon shows "Checking..." until its own answer arrives.
        core.statuses.clear();
        core.outcomes.clear();
        core.game_note.clear();
        core.rescan();
        core.set_status("Checking for updates...".into());
    }
    refresh(ui, shared);
    let weak = ui.as_weak();
    run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
        let (client, github, mut state, cache, api) = {
            let core = lock(shared);
            (core.client(), core.github(), core.state.clone(), core.data_dir.join("cache"), core.api_url())
        };
        // Everything is shown as soon as it is known, the addons one by one, not all at the end.
        let (registry, origin) = registry::load(Some(client.get_text(&registry::registry_url(&api))), &cache);
        let offline = origin != Origin::Api;
        show_progress(shared, true, |core| {
            core.registry = registry.clone();
            core.offline = offline;
        });
        let (game_dir, seen) = {
            let core = lock(shared);
            (game_dir(core.addons_dir()), core.game_exe.clone())
        };
        if let Some(dir) = game_dir {
            let (exe, build) = look_at_game(&dir, seen.as_ref());
            let plan = game_update::check(&WebServer(client.clone()), &dir, build, exe.as_ref()).ok().filter(game_update::Plan::behind);
            show_progress(shared, true, |core| {
                core.game_plan = plan;
                core.game_exe = exe;
            });
        }
        // Without the server every addon (and the launcher itself) is checked on github.com directly,
        // against the stored list; what the server did not scan is marked as unchecked.
        let github: Box<dyn GitHub + Send> = if offline { Box::new(WebGitHub::new(client.clone())) } else { github };
        let results = updates::check_each(&*github, &mut state, &registry, offline, &mut |id, status| {
            show_progress(shared, false, |core| {
                core.statuses.insert(id.to_string(), status.clone());
            });
        });
        let results = Some(results);
        let self_update = if self_update::from_store() {
            self_update::store_available()
        } else {
            github.latest_release(APP_REPO).ok().flatten().and_then(|release| self_update::available(APP_VERSION, &release))
        };
        let game_versions = if offline { None } else { fetch_game_versions(&client, &api) };
        let categories = if offline { Vec::new() } else { client.get_text(&format!("{api}/v1/categories")).map(|t| controller::parse_categories(&t)).unwrap_or_default() };
        show_progress(shared, true, |core| {
            core.self_update = self_update;
            core.game_versions_failed = game_versions.is_none();
            core.game_versions = game_versions.unwrap_or_default();
            core.categories = categories;
        });

        let mut core = lock(shared);
        // Keep versions learned from adopted addons, but never undo an install that ran meanwhile.
        let mut changed = false;
        for checked in &state.entries {
            if let Some(real) = core.state.entries.iter_mut().find(|e| e.id == checked.id)
                && real.version != checked.version
                && real.version.starts_with(survivorsforge::state::TOC_PREFIX)
            {
                real.version = checked.version.clone();
                changed = true;
            }
        }
        if changed {
            core.save_state();
        }
        if let Some(results) = results {
            core.statuses = results.into_iter().collect();
        }
        core.checking = false;
        core.rescan();
        if offline {
            let age = registry::cache_age(&cache, std::time::SystemTime::now())
                .filter(|_| origin == Origin::Cache)
                .map(|age| format!("the list from {} ago", registry::describe_age(age)))
                .unwrap_or_else(|| "the built-in list".into());
            let unchecked = core.state.entries.iter().filter(|e| controller::unchecked_update(&core.state, &core.statuses, &e.id).is_some()).count();
            let note = match unchecked {
                0 => String::new(),
                1 => " 1 update is not checked yet and asks before it installs.".into(),
                n => format!(" {n} updates are not checked yet and ask before they install."),
            };
            return Err(Error::NotInstallable(format!(
                "The SurvivorsForge server cannot be reached: showing {age}, updates come straight from GitHub.{note}"
            )));
        }

        // Switched on in the settings or the detail view: checked, clean updates install right away.
        let picked: Vec<String> = core.state.entries.iter().map(|e| e.id.clone()).filter(|id| core.config.auto_updates(id)).collect();
        let auto: Vec<String> = controller::auto_update_ids(&core.statuses, &core.registry, &core.state, false, &picked, core.config.game_version.as_deref(), &core.game_versions)
            .into_iter()
            .filter(|id| !core.busy.contains(id))
            .collect();
        if !auto.is_empty() {
            let s = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| install_updates(&ui, &s, auto, Vec::new()));
        }

        let updates = controller::update_all_ids(&core.statuses, &core.config.held).len();
        let failed = core.statuses.values().filter(|s| matches!(s, UpdateStatus::CheckFailed(_))).count();
        let mut message = match updates {
            0 => "All addons are up to date".to_string(),
            1 => "1 update available".to_string(),
            n => format!("{n} updates available"),
        };
        if failed > 0 {
            message += &format!(", {failed} checks failed");
        }
        Ok(message)
    });
}

/// Scans the installed folders of the open detail page on this computer and shows what was found.
fn on_detail_scan(ui: &AppWindow, shared: &Shared) {
    let (name, folders, addons) = {
        let core = lock(shared);
        let Some((id, _)) = core.detail.clone() else { return };
        let rows = controller::installed_rows(&core.state, &core.reconciled, &core.scan, &core.registry, &core.statuses);
        let Some(row) = rows.into_iter().find(|r| r.id == id) else { return };
        (row.name, row.folders, core.addons_dir().to_path_buf())
    };
    if folders.is_empty() {
        return;
    }
    let title = format!("Local scan of {name}");
    scan_then(ui, shared, &name, move |_| local_scan::scan_folders(&addons, &folders), move |ui, _shared, report| {
        tell(ui, &title, &local_scan::summary(&report));
    });
}

fn on_row_remove(ui: &AppWindow, shared: &Shared, id: String) {
    let (name, folders, needed_by) = {
        let core = lock(shared);
        if let Some(folder) = id.strip_prefix(UNMANAGED_PREFIX) {
            let name = core.scan.iter().find(|s| s.folder == folder).map(|s| s.toc.title.clone());
            (name.unwrap_or_else(|| folder.to_string()), vec![folder.to_string()], Vec::new())
        } else if let Some(entry) = core.state.get(&id) {
            let registry_id = match &entry.source {
                survivorsforge::state::Source::Verified { registry_id } => registry_id.as_str(),
                _ => entry.id.as_str(),
            };
            let needed_by = controller::dependents(registry_id, &core.registry, &core.state);
            (entry.name.clone(), entry.folders.clone(), needed_by)
        } else {
            return;
        }
    };
    let mut text = format!(
        "These folders are moved to the recycle bin: {}.\n\nYour addon settings (SavedVariables in the WTF folder) stay untouched.",
        folders.join(", ")
    );
    if !needed_by.is_empty() {
        text.push_str(&format!("\n\n{} need it and stop working without it.", needed_by.join(", ")));
    }
    let (weak, shared) = (ui.as_weak(), shared.clone());
    ask(ui, &format!("Remove {name}?"), &text, "Remove", true, move || {
        lock(&shared).busy.insert(id.clone());
        run_bg(weak, shared, Some(id.clone()), move |shared| {
            let mut core = lock(shared);
            let addons = core.addons_dir().to_path_buf();
            if id.starts_with(UNMANAGED_PREFIX) {
                install::remove_folders(&addons, &folders, &mut install::to_recycle_bin)?;
            } else {
                install::uninstall(&addons, &mut core.state, &id, &mut install::to_recycle_bin)?;
                core.statuses.remove(&id);
                core.save_state();
            }
            core.rescan();
            Ok(format!("{name} removed"))
        });
    });
}

fn on_install_zip(ui: &AppWindow, shared: &Shared) {
    // The button is hidden then; this only guards against a stray call.
    if !ui.get_file_dialog() {
        return;
    }
    let Some(file) = rfd::FileDialog::new().add_filter("Zip archive", &["zip"]).pick_file() else { return };
    let detected = match archive::analyze(&file).map_err(Error::from) {
        Ok(detected) => detected,
        Err(e) => {
            let message = if e == Error::NoAddonFound { "No addon found in the zip".into() } else { e.to_string() };
            lock(shared).notify(message, Kind::Error);
            refresh(ui, shared);
            return;
        }
    };
    let plan = {
        let core = lock(shared);
        controller::plan_zip_install(&detected, &core.state, &core.scan)
    };
    let file_name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let title = if plan.is_update { "Update from zip" } else { "Install from zip" };
    let ok = if plan.is_update { "Update" } else { "Install" };
    let text = plan.describe();
    let install = {
        let (weak, shared, plan, file, file_name) = (ui.as_weak(), shared.clone(), plan.clone(), file.clone(), file_name.clone());
        move || install_zip_file(weak, shared, plan, file, file_name)
    };
    let weak = ui.as_weak();
    let shared = shared.clone();
    let scan = move || {
        let Some(ui) = weak.upgrade() else { return };
        let (zip, folders, name) = (file.clone(), plan.addons.clone(), plan.name.clone());
        scan_then(&ui, &shared, &name, move |_| local_scan::scan_zip(&zip, &folders), move |ui, shared, report| {
            let text = scanned_text(&report, &plan.describe());
            let (weak, shared) = (ui.as_weak(), shared.clone());
            ask(ui, title, &text, ok, local_scan::flagged(&report), move || install_zip_file(weak, shared, plan, file, file_name));
        });
    };
    ask_or_scan(ui, title, &text, ok, false, scan, install);
}

/// Installs the picked zip as `plan` says.
fn install_zip_file(weak: slint::Weak<AppWindow>, shared: Shared, plan: controller::ZipPlan, file: PathBuf, file_name: String) {
    {
        lock(&shared).busy.insert(plan.id.clone());
        run_bg(weak, shared, Some(plan.id.clone()), move |shared| {
            let mut core = lock(shared);
            let addons = core.addons_dir().to_path_buf();
            // Work on a copy: if the install fails, the replaced entries stay as they were.
            let mut state = core.state.clone();
            for (id, _) in &plan.takes_over {
                state.remove(id);
            }
            let request = Request {
                zip: &file,
                id: &plan.id,
                name: &plan.name,
                source: Source::Zip { file_name: file_name.clone() },
                version: format!("zip:{file_name}"),
                expected_sha256: None,
            };
            install::install_zip(&addons, &mut state, request)?;
            core.state = state;
            for (id, _) in &plan.takes_over {
                core.statuses.remove(id);
            }
            core.statuses.insert(plan.id.clone(), UpdateStatus::Manual);
            core.save_state();
            core.rescan();
            Ok(format!("{} installed from {file_name}", plan.name))
        });
    }
}

fn on_repo_submit(ui: &AppWindow, shared: &Shared) {
    let repo = match normalize_repo(&ui.get_repo_input()) {
        Ok(repo) => repo,
        Err(e) => return ui.set_repo_error(e.to_string().into()),
    };
    ui.set_repo_error(SharedString::new());
    ui.set_repo_busy(true);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let (client, github, registry, downloads) = {
            let core = lock(&shared);
            (core.client(), core.github(), core.registry.clone(), core.downloads())
        };
        let result = repo_detect::detect(&*github, &registry, &repo, &mut |url| {
            let zip = downloads.join(format!("{}.zip", unique_name()));
            client.download(url, &zip)?;
            archive::analyze(&zip).map_err(Error::from).map(|addons| (zip.clone(), addons)).inspect_err(|_| {
                let _ = std::fs::remove_file(&zip);
            })
        });
        let _ = weak.clone().upgrade_in_event_loop(move |ui| {
            ui.set_repo_busy(false);
            match result {
                Err(Error::NoAddonFound) => ui.set_repo_error("No addon found in this repo".into()),
                Err(e) => ui.set_repo_error(e.to_string().into()),
                Ok(detection) => {
                    ui.set_repo_open(false);
                    show_detection(&ui, &shared, detection);
                }
            }
        });
    });
}

fn show_detection(ui: &AppWindow, shared: &Shared, detection: repo_detect::Detection) {
    let source = match &detection.spec {
        SourceSpec::Release { asset } => format!("the latest release (asset {asset})"),
        SourceSpec::Tag => "the newest tag".into(),
        SourceSpec::Branch => "the default branch (every new commit counts as an update)".into(),
    };
    let names: Vec<&str> = detection.addons.iter().map(|a| a.name.as_str()).collect();
    let mut text = format!(
        "Addons found: {}.\nVersion: {}.\nUpdates come from {source}.",
        names.join(", "),
        detection.remote.label
    );
    let existing: Vec<&str> = {
        let core = lock(shared);
        names.iter().copied().filter(|n| core.scan.iter().any(|s| s.folder.eq_ignore_ascii_case(n))).collect()
    };
    if !existing.is_empty() {
        text += &format!("\n\nExisting folders get replaced: {}.", existing.join(", "));
    }
    let title = format!("Add {}?", detection.repo);
    let install = {
        let (weak, shared, detection) = (ui.as_weak(), shared.clone(), detection.clone());
        move || install_detection(weak, shared, detection)
    };
    let (weak, shared, asked, before) = (ui.as_weak(), shared.clone(), title.clone(), text.clone());
    let scan = move || {
        let Some(ui) = weak.upgrade() else { return };
        let zip = detection.zip.clone();
        let folders: Vec<String> = detection.addons.iter().map(|a| a.name.clone()).collect();
        scan_then(&ui, &shared, &detection.name(), move |_| local_scan::scan_zip(&zip, &folders), move |ui, shared, report| {
            let text = scanned_text(&report, &before);
            let (weak, shared) = (ui.as_weak(), shared.clone());
            ask(ui, &asked, &text, "Install", local_scan::flagged(&report), move || install_detection(weak, shared, detection));
        });
    };
    ask_or_scan(ui, &title, &text, "Install", false, scan, install);
}

/// Installs what `show_detection` found in a repo.
fn install_detection(weak: slint::Weak<AppWindow>, shared: Shared, detection: repo_detect::Detection) {
    {
        let id = detection.entry_id();
        lock(&shared).busy.insert(id.clone());
        run_bg(weak, shared, Some(id.clone()), move |shared| {
            let mut core = lock(shared);
            let addons = core.addons_dir().to_path_buf();
            let name = detection.name();
            let request = Request {
                zip: &detection.zip,
                id: &id,
                name: &name,
                source: Source::GitHub { repo: detection.repo.clone(), spec: detection.spec.clone() },
                version: detection.remote.id.clone(),
                expected_sha256: None,
            };
            let result = install::install_zip(&addons, &mut core.state, request);
            let _ = std::fs::remove_file(&detection.zip);
            result?;
            core.statuses.insert(id.clone(), UpdateStatus::UpToDate);
            core.save_state();
            core.rescan();
            Ok(format!("{name} added from {}", detection.repo))
        });
    }
}

/// The path the user typed, with `~` resolved. Shown back in the field so they see what is used.
fn typed_path(text: &str) -> PathBuf {
    let home = directories::UserDirs::new();
    config::expand_home(text, home.as_ref().map(|u| u.home_dir()))
}

fn settings_error(ui: &AppWindow, shared: &Shared, text: String) {
    let mut core = lock(shared);
    core.settings_error = Some(core.toasts.error(text));
    drop(core);
    refresh(ui, shared);
}

fn on_settings_save(ui: &AppWindow, shared: &Shared) {
    let path = typed_path(&ui.get_settings_path());
    ui.set_settings_path(path.to_string_lossy().into_owned().into());
    let (saved, recheck, warning, errors) = {
        let mut core = lock(shared);
        let game_index = ui.get_settings_game_index();
        let game_version =
            if game_index > 0 { core.game_versions.get(game_index as usize - 1).map(|g| g.name.clone()) } else { None };
        let input = config::SettingsInput {
            addons_dir: path,
            game_version,
            api_url: ui.get_settings_api_url().into(),
            web_url: ui.get_settings_web_url().into(),
            telemetry: ui.get_settings_telemetry(),
            send_install_id: ui.get_settings_send_install_id(),
            auto_update_all: ui.get_settings_auto_update(),
        };
        // A bad field keeps its saved value and its text in the field; the others still save.
        let (config, field_errors, warning) = config::apply_settings(&core.config, input);
        ui.set_settings_path_error(field_errors.path.clone().unwrap_or_default().into());
        ui.set_settings_api_error(field_errors.api.clone().unwrap_or_default().into());
        ui.set_settings_web_error(field_errors.web.clone().unwrap_or_default().into());
        let errors = field_errors.messages();
        let platform_changed = config.api_url != core.config.api_url;
        // The values are valid again: an error about them is over.
        if errors.is_empty()
            && let Some(id) = core.settings_error.take()
        {
            core.toasts.dismiss(id);
        }
        // Saving runs on every focus change: nothing changed, nothing to write.
        if config == core.config {
            drop(core);
            if !errors.is_empty() {
                return settings_error(ui, shared, errors.join(" "));
            }
            return refresh(ui, shared);
        }
        // Another platform: its list, versions and categories replace the old ones.
        let recheck = config.addons_dir != core.config.addons_dir || platform_changed;
        let saved = config.save(&core.data_dir);
        if config.addons_dir != core.config.addons_dir {
            core.switch_config(config);
        } else {
            core.config = config;
        }
        (saved, recheck, warning, errors)
    };
    if !errors.is_empty() {
        settings_error(ui, shared, errors.join(" "));
    }
    match saved {
        // Saved, but the folder looks odd (no game next to it): a warning.
        // The error toast names what was not saved; the rest saved quietly.
        Ok(()) if !errors.is_empty() => {}
        Ok(()) => match warning {
            Some(w) => lock(shared).settings_saved(format!("Settings saved. {w}"), Kind::Warning),
            // A wrong but reachable address still saves; the list then says the platform is away.
            None if recheck => lock(shared).settings_saved("Settings saved. Loading the addon list again...".into(), Kind::Success),
            None => lock(shared).settings_saved("Settings saved.".into(), Kind::Success),
        },
        Err(e) => lock(shared).notify(format!("Could not save the settings: {e}"), Kind::Error),
    }
    refresh(ui, shared);
    if recheck {
        check_updates(ui, shared);
        load_account(ui, shared);
    }
}

/// The login kept for the current platform: shown at once by its saved name, then checked with
/// `/v1/me` on a thread. A login the platform no longer knows (`401`) is forgotten.
fn load_account(ui: &AppWindow, shared: &Shared) {
    let (api, client) = {
        let mut core = lock(shared);
        let api = core.api_url();
        let account = Account::load(&Keyring, &api).unwrap_or_default();
        core.github_only = account.github.filter(|g| account.platform.is_none() && g.source == TokenSource::Device).map(|g| g.login);
        let session = account.platform;
        core.me = session.as_ref().map(|s| controller::Me { login: s.login.clone(), avatar_url: String::new() });
        // A GitHub token runs 8 hours: renew it now while someone (platform or GitHub) can.
        let (renew_api, renew_client) = (api.clone(), core.client());
        std::thread::spawn(move || {
            let _ = survivorsforge::account::github_access(&renew_client, &Keyring, &renew_api, chrono::Utc::now());
        });
        let Some(session) = session else {
            drop(core);
            return refresh(ui, shared);
        };
        (api, (core.client(), session.token))
    };
    refresh(ui, shared);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let (client, token) = client;
        match client.get_text_as(&format!("{api}/v1/me"), &token) {
            Ok(text) => {
                let Some(me) = controller::parse_me(&text) else { return };
                remember_login_name(&api, &me.login);
                let avatar = me.avatar_url.clone();
                lock(&shared).me = Some(me);
                if !avatar.is_empty() {
                    load_pictures(&weak, &shared, &client, vec![avatar], ICON_EDGE);
                }
            }
            Err(Error::Api { status: 401, .. }) => {
                let _ = Account::delete(&Keyring, &api);
                lock(&shared).me = None;
            }
            // The platform is away: the saved name stays.
            Err(_) => return,
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

/// Keeps the login name with the account, to show it while the platform cannot be reached.
fn remember_login_name(api: &str, login: &str) {
    if let Some(mut account) = Account::load(&Keyring, api)
        && let Some(session) = account.platform.as_mut()
        && session.login != login
    {
        session.login = login.to_string();
        let _ = account.save(&Keyring, api);
    }
}

/// Logs in through the browser on a thread; the settings show the wait and how it ended.
fn on_login(ui: &AppWindow, shared: &Shared) {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (api, client) = {
        let mut core = lock(shared);
        if core.login_cancel.is_some() {
            return;
        }
        core.login_cancel = Some(cancel.clone());
        core.login_error.clear();
        (core.api_url(), core.client())
    };
    refresh(ui, shared);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let result = login::login(&client, &Keyring, &api, |url| {
            let _ = open::that(url);
        }, &cancel);
        {
            let mut core = lock(&shared);
            core.login_cancel = None;
            core.login_error = match &result {
                Ok(_) | Err(login::LoginError::Cancelled) => String::new(),
                Err(e) => e.to_string(),
            };
            // Logged in from this moment on: no "Log in" button in between.
            if let Some(session) = result.as_ref().ok().and_then(|a| a.platform.as_ref()) {
                core.me = Some(controller::Me { login: session.login.clone(), avatar_url: String::new() });
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if result.is_ok() {
                load_account(&ui, &shared);
            } else {
                refresh(&ui, &shared);
            }
        });
    });
}

/// Logs in to GitHub alone (device flow) on a thread, for while the platform cannot be reached:
/// the settings show the code to enter at github.com/login/device.
fn on_login_device(ui: &AppWindow, shared: &Shared) {
    use survivorsforge::device_login;
    let Some(client_id) = device_login::client_id() else { return };
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (api, client) = {
        let mut core = lock(shared);
        if core.login_cancel.is_some() {
            return;
        }
        core.login_cancel = Some(cancel.clone());
        core.login_error.clear();
        core.device_code.clear();
        (core.api_url(), core.client())
    };
    // Game Mode opens no browser: the code goes in on another device.
    let open_browser = !ui.get_game_mode();
    refresh(ui, shared);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let result = device_login::start(&client, device_login::GITHUB_OAUTH, &client_id).and_then(|code| {
            {
                let mut core = lock(&shared);
                core.device_code = code.user_code.clone();
                core.device_url = code.verification_uri.clone();
            }
            let redraw = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &redraw));
            if open_browser {
                let _ = open::that(&code.verification_uri);
            }
            let mut token = device_login::wait(&client, device_login::GITHUB_OAUTH, &client_id, &code, &cancel, std::thread::sleep)?;
            token.login = device_login::login_name(&client, survivorsforge::github::GITHUB_API, &token.access_token).unwrap_or_default();
            device_login::save(&Keyring, &api, token.clone())?;
            Ok(token.login)
        });
        {
            let mut core = lock(&shared);
            core.login_cancel = None;
            core.device_code.clear();
            match result {
                Ok(login) => core.github_only = Some(if login.is_empty() { "GitHub".into() } else { login }),
                Err(login::LoginError::Cancelled) => {}
                Err(e) => core.login_error = e.to_string(),
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

fn on_login_cancel(ui: &AppWindow, shared: &Shared) {
    if let Some(cancel) = lock(shared).login_cancel.as_ref() {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    refresh(ui, shared);
}

/// Logs out on a thread: the platform ends the session if it answers, the keyring forgets it.
fn on_logout(ui: &AppWindow, shared: &Shared) {
    let (api, client) = {
        let mut core = lock(shared);
        core.me = None;
        core.github_only = None;
        core.login_error.clear();
        (core.api_url(), core.client())
    };
    refresh(ui, shared);
    std::thread::spawn(move || {
        let _ = login::logout(&client, &Keyring, &api);
    });
}

fn on_setup_confirm(ui: &AppWindow, shared: &Shared) {
    let path = typed_path(&ui.get_setup_path());
    ui.set_setup_path(path.to_string_lossy().into_owned().into());
    match config::validate_addons_dir(&path) {
        Err(e) => {
            ui.set_setup_error(e.to_string().into());
            ui.set_setup_warning(SharedString::new());
            return;
        }
        // The first click shows the warning, the second ("use anyway") accepts it.
        Ok(Some(warning)) if ui.get_setup_warning().is_empty() => {
            ui.set_setup_error(SharedString::new());
            ui.set_setup_warning(warning.to_string().into());
            return;
        }
        Ok(_) => {}
    }
    let config = Config { addons_dir: path, ..Default::default() };
    {
        let mut core = lock(shared);
        if let Err(e) = config.save(&core.data_dir) {
            ui.set_setup_error(format!("Could not save the settings: {e}").into());
            return;
        }
        core.switch_config(config);
    }
    fill_settings(ui, shared);
    ui.set_needs_setup(false);
    refresh(ui, shared);
    check_updates(ui, shared);
}

fn fill_settings(ui: &AppWindow, shared: &Shared) {
    let core = lock(shared);
    ui.set_settings_path(core.config.addons_dir.to_string_lossy().into_owned().into());
    ui.set_settings_api_url(core.config.api_url.clone().unwrap_or_default().into());
    ui.set_settings_web_url(core.config.web_url.clone().unwrap_or_default().into());
    ui.set_settings_api_default(registry::api_url(None).into());
    ui.set_settings_web_default(registry::web_url(None).into());
    ui.set_settings_telemetry(core.config.telemetry);
    ui.set_settings_send_install_id(core.config.send_install_id);
    ui.set_settings_auto_update(core.config.auto_update_all);
    ui.set_installed_tiles(core.config.tiles("installed"));
    ui.set_browse_tiles(core.config.tiles("browse"));
    fill_game_versions(ui, &core);
}

fn fill_game_versions(ui: &AppWindow, core: &Core) {
    let mut names: Vec<SharedString> = vec!["Not set".into()];
    names.extend(core.game_versions.iter().map(|g| SharedString::from(g.name.as_str())));
    let index = core.config.game_version.as_ref().and_then(|v| core.game_versions.iter().position(|g| &g.name == v)).map(|i| i + 1).unwrap_or(0);
    ui.set_settings_game_versions(slint::ModelRc::new(slint::VecModel::from(names)));
    ui.set_settings_game_index(index as i32);
}

/// Downloads the current version of every picked addon, checks its hash and packs them into one
/// zip with every addon folder at its root, for the browser version of the game. Without a file
/// dialog (Game Mode) the zip goes to the Downloads folder.
fn on_pack_save(ui: &AppWindow, shared: &Shared) {
    let name = "SurvivorsForge-pack.zip";
    // Before the save dialog: offline nothing can be packed.
    if lock(shared).offline {
        return lock(shared).notify(Error::Offline.to_string(), Kind::Error);
    }
    let dest = if ui.get_file_dialog() && !ui.get_game_mode() {
        let Some(dest) = rfd::FileDialog::new().add_filter("Zip archive", &["zip"]).set_file_name(name).save_file() else { return };
        dest
    } else {
        match directories::UserDirs::new().and_then(|d| d.download_dir().map(|p| p.join(name))) {
            Some(dest) => dest,
            None => return lock(shared).notify("No Downloads folder found".into(), Kind::Error),
        }
    };
    let parts: Vec<(String, String, String)> = {
        let mut core = lock(shared);
        if core.offline {
            return core.notify(Error::Offline.to_string(), Kind::Error);
        }
        core.pack_busy = true;
        core.picked
            .iter()
            .filter_map(|id| core.registry.get(id))
            .filter_map(|a| a.latest.as_ref().map(|l| (a.name.clone(), l.asset_url.clone(), l.sha256.clone().unwrap_or_default())))
            .collect()
    };
    refresh(ui, shared);
    run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
        let (client, downloads) = {
            let core = lock(shared);
            (core.client(), core.downloads())
        };
        let mut files = Vec::new();
        let result = (|| -> survivorsforge::Result<String> {
            for (name, url, sha256) in &parts {
                let zip = downloads.join(format!("{}.zip", unique_name()));
                files.push(zip.clone());
                client.download(url, &zip)?;
                if install::sha256_of(&zip)? != *sha256 {
                    return Err(Error::NotInstallable(format!("{name} is not the file that was checked, nothing was saved")));
                }
            }
            let readers = files.iter().map(std::fs::File::open).collect::<std::io::Result<Vec<_>>>()?;
            let part = dest.with_extension("zip.part");
            let bundled = std::fs::File::create(&part).map_err(Error::from).and_then(|out| Ok(forge_core::pack::bundle(readers, out, 1_000_000_000)?));
            match bundled {
                Ok(done) => {
                    std::fs::rename(&part, &dest)?;
                    Ok(format!("Saved {} addons ({:.1} MB) to {}", parts.len(), done.packed_bytes as f64 / 1e6, dest.display()))
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    Err(e)
                }
            }
        })();
        for file in &files {
            let _ = std::fs::remove_file(file);
        }
        lock(shared).pack_busy = false;
        result
    });
}

/// The platform's game versions, oldest first. Empty when there is no platform or it fails.
/// The platform's game versions; `None` when it did not answer with a list.
fn fetch_game_versions(client: &Client, api: &str) -> Option<Vec<controller::GameVersion>> {
    let text = client.get_text(&format!("{api}/v1/game-versions")).ok()?;
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
    Some(
        list.iter()
            .filter_map(|g| Some(controller::GameVersion { name: g["name"].as_str()?.to_string(), sort_key: g["sort_key"].as_i64()? as i32 }))
            .collect(),
    )
}

fn on_self_update(ui: &AppWindow, shared: &Shared) {
    if !self_update::can_replace_self() {
        // In Game Mode a browser can't open over the app, so name the address instead.
        let opened = !ui.get_game_mode() && open::that(self_update::RELEASES_URL).is_ok();
        let message = if opened {
            "Opened the download page in your browser.".into()
        } else {
            format!("Download the new version from {}", self_update::RELEASES_URL)
        };
        lock(shared).notify(message, Kind::Info);
        return refresh(ui, shared);
    }
    let (client, update, work) = {
        let mut core = lock(shared);
        if !core.busy.is_empty() {
            core.notify("Wait until the running installs finish, then update SurvivorsForge.".into(), Kind::Warning);
            drop(core);
            return refresh(ui, shared);
        }
        let Some(update) = core.self_update.clone() else { return };
        (core.client(), update, core.data_dir.join("update"))
    };
    ui.set_self_update_busy(true);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let result = if self_update::from_store() { self_update::apply_store() } else { self_update::apply(&client, &update, &work).map(|()| true) };
        match result {
            Ok(true) => {
                let _ = weak.upgrade_in_event_loop(|_| self_update::restart());
                return;
            }
            Ok(false) => lock(&shared).notify("Opened SurvivorsForge in the Microsoft Store, update it there.".into(), Kind::Info),
            Err(e) => lock(&shared).notify(format!("Self-update failed: {e}"), Kind::Error),
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_self_update_busy(false);
            refresh(&ui, &shared);
        });
    });
}

/// On Linux, rfd's dialogs go through xdg-desktop-portal over D-Bus. Without a session bus
/// (some minimal sessions) the dialog silently returns nothing, so the UI hides Browse instead.
fn file_dialog_available() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        || std::env::var_os("XDG_RUNTIME_DIR").is_some_and(|dir| Path::new(&dir).join("bus").exists())
}

fn pick_folder(start: &str) -> Option<String> {
    let mut dialog = rfd::FileDialog::new();
    if Path::new(start).is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder().map(|p| p.to_string_lossy().into_owned())
}

/// Wayland and X11 take the taskbar icon from the `.desktop` file whose name matches this id.
#[cfg(target_os = "linux")]
fn select_backend() -> Result<(), slint::PlatformError> {
    use slint::winit_030::winit::platform::{wayland::WindowAttributesExtWayland, x11::WindowAttributesExtX11};
    const APP_ID: &str = "survivorsforge";
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| {
            let attrs = WindowAttributesExtWayland::with_name(attrs, APP_ID, APP_ID);
            WindowAttributesExtX11::with_name(attrs, APP_ID, APP_ID)
        })
        .select()
}

fn main() -> Result<(), slint::PlatformError> {
    // A link from the website: a running launcher opens the addon, this one ends.
    let link = std::env::args().nth(1).and_then(|a| deep_link::addon_id(&a));
    if let Some(id) = &link
        && deep_link::send_to_running(id)
    {
        return Ok(());
    }
    // Debug builds leave the links to the installed launcher; the Store version has them from its manifest.
    if !cfg!(debug_assertions) && !self_update::from_store() {
        deep_link::register();
    }
    // The exe an update set aside, which may still run for a moment after the restart.
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        std::thread::spawn(move || self_update::clean_up_after_update(&exe));
    }
    #[cfg(target_os = "linux")]
    select_backend()?;
    let game_mode = config::is_game_mode(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some(),
    );
    // The icon at once, before anything is read or built (ui/splash.slint).
    let splash = Splash::new()?;
    if game_mode {
        splash.window().set_fullscreen(true);
    }
    splash.show()?;
    // The config, the stored lists and the cache load beside the window being built.
    let loading = std::thread::spawn(|| {
        let data_dir = config::data_dir();
        let config = Config::load(&data_dir);
        let needs_setup = config.is_none();
        (Core::new(data_dir, config), needs_setup)
    });
    // ponytail: a short delay so the splash is painted before the window is built on this thread;
    // a zero timer can run before the first frame.
    let started: std::rc::Rc<RefCell<Option<Result<AppWindow, slint::PlatformError>>>> = Default::default();
    let holder = started.clone();
    slint::Timer::single_shot(std::time::Duration::from_millis(30), move || {
        let result = start(loading, link, game_mode);
        if let Ok(ui) = &result {
            let _ = ui.show();
        }
        let _ = splash.hide();
        if result.is_err() {
            let _ = slint::quit_event_loop();
        }
        *holder.borrow_mut() = Some(result);
    });
    slint::run_event_loop()?;
    match started.take() {
        Some(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

/// Builds the window and wires it up while the splash shows; `loading` brings the Core.
fn start(loading: std::thread::JoinHandle<(Core, bool)>, link: Option<String>, game_mode: bool) -> Result<AppWindow, slint::PlatformError> {
    let ui = AppWindow::new()?;
    let (core, needs_setup) = loading.join().map_err(|_| slint::PlatformError::Other("loading the launcher failed".into()))?;
    let shared: Shared = Arc::new(Mutex::new(core));
    {
        let (weak, s) = (ui.as_weak(), shared.clone());
        // ponytail: the closure holds the Arc of the Core it sits in; both live as long as the app.
        lock(&shared).repaint = Some(Arc::new(move || {
            let s = s.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &s));
        }));
    }
    // A controller works the window like the keyboard, only while the window has the focus and
    // the game does not run: its presses are meant for the game then.
    {
        let (weak, s) = (ui.as_weak(), shared.clone());
        survivorsforge::gamepad::listen(move |pad| {
            let s = s.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                let focused = ui.window().with_winit_window(|w| w.has_focus()).unwrap_or(false);
                if !focused || lock(&s).game_running {
                    return;
                }
                let key: SharedString = match survivorsforge::gamepad::press(pad) {
                    survivorsforge::gamepad::Press::Focus { back: true } => slint::platform::Key::Backtab.into(),
                    survivorsforge::gamepad::Press::Focus { back: false } => slint::platform::Key::Tab.into(),
                    survivorsforge::gamepad::Press::Key(key) => key.into(),
                };
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyPressed { text: key.clone() });
                ui.window().dispatch_event(slint::platform::WindowEvent::KeyReleased { text: key });
            });
        });
    }
    ui.set_app_version(APP_VERSION.into());
    let labels = |list: &[&str]| ModelRc::new(VecModel::from(list.iter().map(|l| SharedString::from(*l)).collect::<Vec<_>>()));
    ui.set_safety_labels(labels(&controller::SAFETY_LABELS));
    ui.set_sort_labels(labels(&controller::SORT_LABELS));
    ui.set_self_update_in_place(self_update::can_replace_self());

    ui.set_game_mode(game_mode);
    ui.global::<Theme>().set_touch(game_mode);
    if game_mode {
        ui.window().set_fullscreen(true);
    }
    // The portal's file chooser may not show under gamescope, so Game Mode types paths.
    ui.set_file_dialog(file_dialog_available() && !game_mode);

    if needs_setup {
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let home = directories::UserDirs::new();
            let home = home.as_ref().map(|u| u.home_dir());
            // ponytail: first five only; a scroll list if people really keep more game copies
            let found: Vec<Suggestion> = config::find_addons_dirs(&config::search_roots(), 3)
                .into_iter()
                .take(5)
                .map(|p| Suggestion {
                    label: config::suggestion_label(&p, home).into(),
                    path: p.to_string_lossy().into_owned().into(),
                })
                .collect();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                // A single find goes straight into the field, so Continue is the one thing to press.
                if let [only] = found.as_slice()
                    && ui.get_setup_path().is_empty()
                {
                    ui.set_setup_path(only.path.clone());
                }
                ui.set_setup_suggestions(ModelRc::new(VecModel::from(found)));
                ui.set_setup_searching(false);
            });
        });
    } else {
        ui.set_setup_searching(false);
    }
    ui.set_addons_example(if cfg!(windows) { r"...\benilla-config\AddOns" } else { ".../benilla-config/AddOns" }.into());
    ui.set_needs_setup(needs_setup);
    fill_settings(&ui, &shared);

    macro_rules! on {
        ($setter:ident, |$ui:ident, $shared:ident $(, $arg:ident)*| $body:expr) => {{
            let (weak, $shared) = (ui.as_weak(), shared.clone());
            ui.$setter(move |$($arg),*| {
                let $ui = weak.unwrap();
                let $shared = &$shared;
                $body
            });
        }};
    }

    on!(on_setup_browse, |ui, _shared| {
        if let Some(p) = pick_folder(&ui.get_setup_path()) {
            ui.set_setup_path(p.into());
            ui.set_setup_warning(SharedString::new());
        }
    });
    on!(on_setup_confirm, |ui, shared| on_setup_confirm(&ui, shared));
    on!(on_search_changed, |ui, shared, text| {
        lock(shared).search = text.to_string();
        refresh(&ui, shared);
    });
    on!(on_check_updates, |ui, shared| check_updates(&ui, shared));
    ui.on_view_changed({
        let shared = shared.clone();
        move |page, tiles| {
            let mut core = lock(&shared);
            core.config.set_tiles(if page == 0 { "installed" } else { "browse" }, tiles);
            let _ = core.config.save(&core.data_dir);
        }
    });
    on!(on_launch_game, |ui, shared| on_launch_game(&ui, shared));
    on!(on_update_game, |ui, shared| on_update_game(&ui, shared));
    on!(on_update_all, |ui, shared| on_update_all(&ui, shared));
    on!(on_install_zip, |ui, shared| on_install_zip(&ui, shared));
    on!(on_add_repo, |ui, _shared| {
        ui.set_repo_input(SharedString::new());
        ui.set_repo_error(SharedString::new());
        ui.set_repo_open(true);
    });
    on!(on_repo_cancel, |ui, _shared| ui.set_repo_open(false));
    on!(on_repo_submit, |ui, shared| on_repo_submit(&ui, shared));
    on!(on_row_action, |ui, shared, id| on_row_action(&ui, shared, id.to_string()));
    on!(on_row_remove, |ui, shared, id| on_row_remove(&ui, shared, id.to_string()));
    on!(on_row_open, |_ui, shared, id| if let Some(repo) = lock(shared).repo_of(&id) {
        let _ = open::that(format!("https://github.com/{repo}"));
    });
    on!(on_row_details, |ui, shared, id| on_row_details(&ui, shared, id.to_string()));
    on!(on_detail_website, |ui, shared| on_detail_link(&ui, shared, "website"));
    on!(on_open_addons_dir, |ui, shared| {
        let dir = lock(shared).addons_dir().to_path_buf();
        let (weak, shared) = (ui.as_weak(), shared.clone());
        std::thread::spawn(move || {
            if open::that(&dir).is_err() {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    lock(&shared).notify(format!("Could not open the folder {}", dir.display()), Kind::Error);
                    refresh(&ui, &shared);
                });
            }
        });
    });
    on!(on_detail_link, |ui, shared, page| on_detail_link(&ui, shared, &page));
    on!(on_comments_show, |ui, shared| on_comments_show(&ui, shared));
    on!(on_detail_tab_changed, |ui, shared, tab| match tab {
        3 => on_comments_show(&ui, shared),
        2 | 4 => load_detail_list(&ui, shared, tab),
        _ => {}
    });
    on!(on_detail_retry, |ui, shared, tab| load_detail_list(&ui, shared, tab));
    on!(on_version_install, |ui, shared, tag| on_version_install(&ui, shared, tag.to_string()));
    on!(on_comments_older, |ui, shared| load_comments(&ui, shared, true));
    on!(on_comments_retry, |ui, shared| {
        let older = lock(shared).comments.as_ref().is_some_and(|c| !c.loaded.is_empty());
        load_comments(&ui, shared, older);
    });
    on!(on_confirm_accept, |ui, _shared| {
        ui.set_confirm_open(false);
        if let Some(action) = PENDING.with(|p| p.borrow_mut().take()) {
            action();
        }
    });
    on!(on_confirm_cancel, |ui, _shared| {
        ui.set_confirm_open(false);
        PENDING.with(|p| p.borrow_mut().take());
        PENDING_EXTRA.with(|p| p.borrow_mut().take());
    });
    on!(on_confirm_extra_accept, |ui, _shared| {
        ui.set_confirm_open(false);
        PENDING.with(|p| p.borrow_mut().take());
        if let Some(action) = PENDING_EXTRA.with(|p| p.borrow_mut().take()) {
            action();
        }
    });
    on!(on_detail_scan, |ui, shared| on_detail_scan(&ui, shared));
    on!(on_detail_auto_update, |ui, shared| {
        {
            let mut core = lock(shared);
            let Some((id, _)) = core.detail.clone() else { return };
            let on = !core.config.auto_update.contains(&id);
            core.config.auto_update.retain(|a| a != &id);
            if on {
                core.config.auto_update.push(id);
            }
            let _ = core.config.save(&core.data_dir);
        }
        refresh(&ui, shared);
    });
    on!(on_settings_browse, |ui, shared| {
        if let Some(p) = pick_folder(&ui.get_settings_path()) {
            ui.set_settings_path(p.into());
            on_settings_save(&ui, shared);
        }
    });
    on!(on_settings_save, |ui, shared| on_settings_save(&ui, shared));
    on!(on_login, |ui, shared| on_login(&ui, shared));
    on!(on_comments_send, |ui, shared| on_comment_send(&ui, shared));
    on!(on_issue_show, |ui, shared, number| on_issue_show(&ui, shared, number));
    on!(on_issues_back, |ui, shared| on_issues_back(&ui, shared));
    on!(on_issues_new, |ui, shared| on_issues_new(&ui, shared));
    on!(on_issues_submit, |ui, shared| on_issues_submit(&ui, shared));
    on!(on_issue_send_reply, |ui, shared| on_issue_send_reply(&ui, shared));
    on!(on_comments_reply, |ui, shared, id| on_comment_reply(&ui, shared, id));
    on!(on_comments_compose, |ui, shared| on_comment_compose(&ui, shared));
    on!(on_comments_cancel_reply, |ui, shared| on_comment_cancel_reply(&ui, shared));
    on!(on_login_cancel, |ui, shared| on_login_cancel(&ui, shared));
    on!(on_login_device, |ui, shared| on_login_device(&ui, shared));
    on!(on_open_device_page, |_ui, shared| {
        let url = lock(shared).device_url.clone();
        if !url.is_empty() {
            let _ = open::that(url);
        }
    });
    on!(on_logout, |ui, shared| on_logout(&ui, shared));
    on!(on_toast_dismiss, |ui, shared, id| {
        lock(shared).toasts.dismiss(id);
        refresh(&ui, shared);
    });
    on!(on_category_changed, |ui, shared, index| {
        {
            let mut core = lock(shared);
            let choices = controller::category_choices(&core.categories, &core.registry);
            core.category = usize::try_from(index - 1).ok().and_then(|i| choices.get(i)).map(|c| c.name.clone());
        }
        refresh(&ui, shared);
    });
    on!(on_safety_changed, |ui, shared, index| {
        lock(shared).safety = controller::safety_filter(index);
        refresh(&ui, shared);
    });
    on!(on_sort_changed, |ui, shared, index| {
        lock(shared).sort = controller::Sort::from_index(index);
        refresh(&ui, shared);
    });
    on!(on_row_pick, |ui, shared, id| {
        {
            let mut core = lock(shared);
            let id = id.to_string();
            if !core.picked.remove(&id) {
                core.picked.insert(id);
            }
        }
        refresh(&ui, shared);
    });
    on!(on_pack_clear, |ui, shared| {
        lock(shared).picked.clear();
        refresh(&ui, shared);
    });
    on!(on_pack_share, |ui, shared| {
        let (ids, web): (Vec<String>, String) = {
            let core = lock(shared);
            (core.picked.iter().cloned().collect(), core.web_url())
        };
        let link = controller::pack_link(&web, &ids);
        if open::that(&link).is_err() {
            lock(shared).notify(format!("Open {link} in your browser"), Kind::Info);
            refresh(&ui, shared);
        }
    });
    on!(on_pack_save, |ui, shared| on_pack_save(&ui, shared));
    on!(on_self_update, |ui, shared| on_self_update(&ui, shared));

    // Own window frame (`no-frame`): moving, resizing and the caption buttons go through winit.
    on!(on_start_drag, |ui, _shared| {
        ui.window().with_winit_window(|w| {
            let _ = w.drag_window();
        });
    });
    on!(on_start_resize, |ui, _shared, direction| {
        use slint::winit_030::winit::window::ResizeDirection::*;
        let direction = [North, South, East, West, NorthEast, NorthWest, SouthEast, SouthWest][direction as usize];
        ui.window().with_winit_window(|w| {
            let _ = w.drag_resize_window(direction);
        });
    });
    on!(on_minimize, |ui, _shared| ui.window().set_minimized(true));
    on!(on_toggle_maximize, |ui, _shared| {
        ui.window().set_maximized(!ui.window().is_maximized());
    });
    // Quitting the loop instead of hiding: a hidden fullscreen window kept the app running.
    on!(on_close_window, |ui, shared| {
        // A field typed into without Enter still counts, as when the page is left.
        if ui.get_page() == 2 {
            on_settings_save(&ui, shared);
        }
        let warning = {
            let core = lock(shared);
            controller::quit_warning(core.game_updating, ui.get_self_update_busy(), core.busy.len())
        };
        match warning {
            Some(text) => ask(&ui, "Quit SurvivorsForge?", &text, "Quit anyway", true, || {
                let _ = slint::quit_event_loop();
            }),
            None => {
                let _ = slint::quit_event_loop();
            }
        }
    });
    // Alt+F4 and the system's close go the same way, so they also save and ask.
    {
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                ui.invoke_close_window();
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    refresh(&ui, &shared);
    if !needs_setup {
        check_updates(&ui, &shared);
        fetch_icons(&ui, &shared);
    }
    load_account(&ui, &shared);
    let (weak, linked) = (ui.as_weak(), shared.clone());
    deep_link::listen(move |id| {
        let shared = linked.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| open_link(&ui, &shared, id));
    });
    if let Some(id) = link {
        open_link(&ui, &shared, id);
    }
    Ok(ui)
}

/// One comment for the comments page; its avatar shows once it is loaded.
fn issue_line(i: &controller::IssueLine) -> IssueLine {
    IssueLine { number: i.number as i32, title: i.title.as_str().into(), author: i.author.as_str().into(), comments: i.comments as i32, date: i.date.as_str().into() }
}

/// A trust tag's colour as the Slint `Pill` takes it.
fn tone_kind(tone: forge_core::trust::Tone) -> i32 {
    use forge_core::trust::Tone;
    match tone {
        Tone::Plain => 0,
        Tone::Ok => 1,
        Tone::Warn => 2,
        Tone::Bad => 3,
    }
}

fn version_line(v: &controller::VersionLine, verified_author: bool, installed: &str, newest: bool, held: bool) -> VersionLine {
    let trust_kind = tone_kind(v.trust.tone(verified_author));
    VersionLine {
        tag: v.tag.as_str().into(),
        name: v.name.as_str().into(),
        date: v.date.as_str().into(),
        trust: v.trust.label(verified_author).into(),
        trust_kind,
        downloads: v.downloads as i32,
        prerelease: v.prerelease,
        installed: v.tag == installed,
        can_install: v.install.is_some(),
        newest,
        held: held && v.tag == installed,
    }
}

/// Installs exactly the version `tag` of the detail view's addon, after a confirmation that says
/// what an older one means: it stays until the newest is installed again by hand.
fn on_version_install(ui: &AppWindow, shared: &Shared, tag: String) {
    let job = {
        let core = lock(shared);
        let Some((id, _)) = core.detail.clone() else { return };
        let Some(version) = core.versions.as_ref().and_then(|l| l.rows.as_ref()).and_then(|rows| rows.iter().find(|v| v.tag == tag)).cloned() else { return };
        let Some(remote) = version.install.clone() else { return };
        let addon = registry_entry(&core, &id).map(|a| (a.name.clone(), a.latest.as_ref().map(|l| l.tag.clone())));
        let (name, newest) = addon.unwrap_or_default();
        let held = core.config.held.contains(&id);
        (id, version, remote, name, newest.as_deref() != Some(tag.as_str()), held)
    };
    let (id, version, remote, name, older, held) = job;
    // The same warnings, counts and closing line as for the newest version.
    let warnings = controller::version_warnings(&version);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    let scan = {
        let (weak, shared, id, remote) = (weak.clone(), shared.clone(), id.clone(), remote.clone());
        move |title: String| {
            if let Some(ui) = weak.upgrade() {
                scan_remote(&ui, &shared, id, Vec::new(), title, Some(remote));
            }
        }
    };
    let install = move || {
        if let Some(ui) = weak.upgrade() {
            start_install(&ui, &shared, id, Vec::new(), Some(remote));
        }
    };
    if !older && !held && warnings.is_empty() {
        return install();
    }
    let title = format!("Install {}?", controller::version_title(&name, &version.name));
    let first = if older {
        format!("{} is not the newest version. SurvivorsForge does not update {name} on its own until you install the newest again, here under Versions or with Update.", version.name)
    } else {
        format!("{} is the newest version. Installing it ends the hold: SurvivorsForge updates {name} on its own again.", version.name)
    };
    let (_, warned, danger) = controller::warning_dialog(&name, &warnings);
    let text = if warnings.is_empty() { first } else { format!("{first}\n\n{warned}") };
    let ok = if danger { "Install anyway" } else { "Install" };
    if warnings.is_empty() {
        return ask(ui, &title, &text, ok, danger, install);
    }
    // As for the newest version: the very file may be checked on this computer first.
    let asked = title.clone();
    ask_or_scan(ui, &title, &text, ok, danger, move || scan(asked), install);
}

fn comment_row(core: &Core, c: controller::Comment) -> CommentRow {
    let avatar = (!c.avatar_url.is_empty()).then_some(c.avatar_url.as_str());
    let own = core.me.as_ref().is_some_and(|me| me.login.eq_ignore_ascii_case(&c.author));
    CommentRow {
        id: c.id as i32,
        own,
        avatar: picture(core, avatar),
        initial: c.author.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_default().into(),
        author: c.author.into(),
        date: c.date.into(),
        edited: c.edited,
        deleted: c.deleted,
        depth: c.depth as i32,
        body: c.body.into(),
    }
}

/// Opens a page of the detail view's addon in the browser. On a thread, so a slow browser start
/// does not hold up the window; a busy toast says it is coming, and where to go if it fails.
fn on_detail_link(ui: &AppWindow, shared: &Shared, page: &str) {
    let url = {
        let core = lock(shared);
        let Some(entry) = core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)) else { return };
        if page == "bug" {
            let issues_on = core.platform.info.get(&entry.id).is_some_and(|i| i.issues_on);
            controller::bug_report(&core.web_url(), &entry.id, &entry.repo, issues_on, core.platform.issues_ready.get(&entry.id).copied()).1
        } else if let Some(tag) = page.strip_prefix("release/") {
            controller::release_page(&entry.repo, tag)
        } else {
            let Some(url) = controller::addon_link(&core.web_url(), &entry.id, page) else { return };
            url
        }
    };
    let opening = lock(shared).set_status("Opening the browser".into());
    refresh(ui, shared);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let failed = open::that(&url).is_err();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut core = lock(&shared);
            if failed {
                core.notify(format!("Could not open the browser. The page is {url}"), Kind::Error);
            } else if core.work == Some(opening) {
                core.work = None;
                core.toasts.dismiss(opening);
            }
            drop(core);
            refresh(&ui, &shared);
        });
    });
}

/// A list page of the detail view: `rows` once the platform answered, `failed` when it did not.
struct Listing<T> {
    slug: String,
    rows: Option<T>,
    failed: bool,
}

/// Loads the Issues (`tab` 2) or Versions (4) page of the detail view's addon on a thread; an
/// answer for another addon is dropped.
fn load_detail_list(ui: &AppWindow, shared: &Shared, tab: i32) {
    let job = {
        let mut core = lock(shared);
        let Some(slug) = core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)).map(|a| a.id.clone()) else { return };
        match tab {
            2 => {
                core.issues = Some(Listing { slug: slug.clone(), rows: None, failed: false });
                core.issue_view = IssueView { slug: slug.clone(), ..Default::default() };
            }
            4 => core.versions = Some(Listing { slug: slug.clone(), rows: None, failed: false }),
            _ => return,
        }
        (slug, core.api_url(), core.client())
    };
    refresh(ui, shared);
    let (slug, api, client) = job;
    let path = if tab == 2 { "issues" } else { "versions" };
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let answer = client.get_text_quick(&format!("{api}/v1/addons/{slug}/{path}")).ok();
        {
            let mut core = lock(&shared);
            if tab == 2 {
                if let Some(l) = core.issues.as_mut().filter(|l| l.slug == slug) {
                    l.rows = answer.as_deref().and_then(controller::issue_lines);
                    l.failed = l.rows.is_none();
                }
            } else if let Some(l) = core.versions.as_mut().filter(|l| l.slug == slug) {
                l.rows = answer.as_deref().and_then(controller::version_lines);
                l.failed = l.rows.is_none();
            }
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

/// Opens the comments page of the detail view's addon and loads its newest comments.
fn on_comments_show(ui: &AppWindow, shared: &Shared) {
    let slug = {
        let core = lock(shared);
        core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)).map(|a| a.id.clone())
    };
    let Some(slug) = slug else { return };
    lock(shared).comments = Some(Comments {
        slug,
        loaded: Vec::new(),
        older: None,
        asked: None,
        failed: false,
        off: false,
        request: 0,
        reply_to: None,
        composing: false,
        sending: false,
        send_error: String::new(),
        needs_login: false,
    });
    ui.set_comments_draft(SharedString::new());
    load_comments(ui, shared, false);
}

/// Asks the platform for the newest comments, or (`older`) the page before the oldest loaded, on a
/// thread. The page answers at once: placeholders after 150 ms, a word after 5 s, and after
/// `QUICK_TIMEOUT` a failure with Try again.
fn load_comments(ui: &AppWindow, shared: &Shared, older: bool) {
    let job = {
        let mut core = lock(shared);
        let (api, client) = (core.api_url(), core.client());
        let Some(c) = core.comments.as_mut() else { return };
        if c.asked.is_some() {
            return;
        }
        let before = if older {
            let Some(id) = c.older else { return };
            format!("?before={id}")
        } else {
            c.loaded.clear();
            String::new()
        };
        c.request += 1;
        c.asked = Some(std::time::Instant::now());
        c.failed = false;
        (format!("{api}/v1/addons/{}/comments{before}", c.slug), c.request, client)
    };
    refresh(ui, shared);
    for after in [controller::QUIET_FOR, controller::SLOW_AFTER] {
        let (weak, shared) = (ui.as_weak(), shared.clone());
        slint::Timer::single_shot(after, move || {
            if let Some(ui) = weak.upgrade() {
                refresh(&ui, &shared);
            }
        });
    }
    let (url, request, client) = job;
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let answer = client.get_text_quick(&url);
        let avatars: Vec<String> = {
            let mut core = lock(&shared);
            // Closed meanwhile, or another addon's page: this answer is for nobody.
            let Some(c) = core.comments.as_mut().filter(|c| c.request == request) else { return };
            c.asked = None;
            match answer.as_ref().ok().and_then(|t| controller::comments_page(t)) {
                Some((page, more)) => {
                    c.loaded.extend(page);
                    c.older = more;
                    c.loaded.iter().map(|m| m.avatar_url.clone()).filter(|u| !u.is_empty()).collect()
                }
                None => {
                    // The owner turned comments off since the details were loaded.
                    if matches!(answer, Err(survivorsforge::Error::Http(403))) {
                        c.off = true;
                    } else {
                        c.failed = true;
                    }
                    Vec::new()
                }
            }
        };
        let redraw = shared.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &redraw));
        load_pictures(&weak, &shared, &client, avatars, ICON_EDGE);
    });
}

/// The bearer token of the login on the current platform, if any.
fn bearer(core: &Core) -> Option<String> {
    Account::load(&Keyring, &core.api_url()).and_then(|a| a.platform).map(|s| s.token)
}

/// The platform no longer knows the login (`401`): forget it, the user logs in again.
fn forget_login(core: &mut Core) {
    let _ = Account::delete(&Keyring, &core.api_url());
    core.me = None;
}

/// Sends the comment in the box (a reply when one is picked) on a thread; the list reloads with it.
fn on_comment_send(ui: &AppWindow, shared: &Shared) {
    let draft = ui.get_comments_draft().to_string();
    let job = {
        let mut core = lock(shared);
        let token = bearer(&core);
        let (api, client) = (core.api_url(), core.client());
        let Some(c) = core.comments.as_mut() else { return };
        if c.sending {
            return;
        }
        let body = match controller::comment_body(&draft, c.reply_to.as_ref().map(|(id, _)| *id)) {
            Ok(body) => body,
            Err(why) => {
                c.send_error = why.into();
                drop(core);
                return refresh(ui, shared);
            }
        };
        let Some(token) = token else {
            forget_login(&mut core);
            drop(core);
            return refresh(ui, shared);
        };
        c.sending = true;
        c.send_error.clear();
        c.needs_login = false;
        (format!("{api}/v1/addons/{}/comments", c.slug), body, token, client, c.slug.clone())
    };
    refresh(ui, shared);
    let (url, body, token, client, slug) = job;
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let result = client.post_json(&url, Some(&token), &[], &body);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let sent = {
                let mut core = lock(&shared);
                let logged_out = matches!(result, Err(Error::Api { status: 401, .. }));
                if logged_out {
                    forget_login(&mut core);
                }
                // Another addon's page meanwhile: its box is not this one.
                let Some(c) = core.comments.as_mut().filter(|c| c.slug == slug) else { return };
                c.sending = false;
                match &result {
                    Ok(_) => {
                        c.reply_to = None;
                        c.composing = false;
                        true
                    }
                    Err(_) if logged_out => {
                        c.send_error = "Your login ran out. Log in again to write.".into();
                        c.needs_login = true;
                        false
                    }
                    Err(e) => {
                        c.send_error = e.to_string();
                        false
                    }
                }
            };
            if sent {
                ui.set_comments_draft(SharedString::new());
                load_comments(&ui, &shared, false);
            } else {
                refresh(&ui, &shared);
            }
        });
    });
}

/// The addon of the detail view's Issues tab.
fn issues_slug(core: &Core) -> Option<String> {
    core.detail.as_ref().and_then(|(id, _)| registry_entry(core, id)).map(|a| a.id.clone())
}

/// Shows issue `number` in the Issues tab and loads it with its replies on a thread.
fn on_issue_show(ui: &AppWindow, shared: &Shared, number: i32) {
    let job = {
        let mut core = lock(shared);
        let Some(slug) = issues_slug(&core) else { return };
        let keep_error = core.issue_view.mode == 2 && core.issue_view.number == number as u64;
        let send_error = if keep_error { std::mem::take(&mut core.issue_view.send_error) } else { String::new() };
        core.issue_view = IssueView { mode: 2, slug: slug.clone(), number: number as u64, loading: true, send_error, ..Default::default() };
        (format!("{}/v1/addons/{slug}/issues/{number}", core.api_url()), core.client(), slug)
    };
    refresh(ui, shared);
    let (url, client, slug) = job;
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let page = client.get_text_quick(&url).ok().and_then(|t| controller::issue_page(&t));
        {
            let mut core = lock(&shared);
            let view = &mut core.issue_view;
            if view.mode != 2 || view.slug != slug || view.number != number as u64 {
                return;
            }
            view.loading = false;
            view.failed = page.is_none();
            view.page = page;
        }
        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
    });
}

fn on_issues_back(ui: &AppWindow, shared: &Shared) {
    {
        let mut core = lock(shared);
        let slug = std::mem::take(&mut core.issue_view.slug);
        core.issue_view = IssueView { slug, ..Default::default() };
    }
    refresh(ui, shared);
}

fn on_issues_new(ui: &AppWindow, shared: &Shared) {
    {
        let mut core = lock(shared);
        let slug = std::mem::take(&mut core.issue_view.slug);
        core.issue_view = IssueView { mode: 1, slug, ..Default::default() };
    }
    refresh(ui, shared);
}

/// Posts `body` to `path` of the detail view's addon as the user on GitHub, on a thread: the
/// GitHub token is renewed first when needed. `done` gets the answer on the UI thread.
fn post_as_github_user(ui: &AppWindow, shared: &Shared, path: String, body: serde_json::Value, done: fn(&AppWindow, &Shared, String)) {
    let job = {
        let mut core = lock(shared);
        let Some(token) = bearer(&core) else {
            forget_login(&mut core);
            drop(core);
            return refresh(ui, shared);
        };
        core.issue_view.sending = true;
        core.issue_view.send_error.clear();
        core.issue_view.needs_login = false;
        (core.api_url(), core.client(), token, core.issue_view.slug.clone())
    };
    refresh(ui, shared);
    let (api, client, token, slug) = job;
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let reauth = || Error::Api { status: 403, code: "github_reauth".into(), message: "Log in again to post on GitHub".into() };
        let result = survivorsforge::account::github_access(&client, &Keyring, &api, chrono::Utc::now()).map_err(|_| reauth()).and_then(|github| {
            client.post_json(&format!("{api}/v1/addons/{slug}/{path}"), Some(&token), &[(survivorsforge::account::GITHUB_TOKEN_HEADER, &github)], &body)
        });
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let answer = {
                let mut core = lock(&shared);
                if matches!(result, Err(Error::Api { status: 401, .. })) {
                    forget_login(&mut core);
                }
                if core.issue_view.slug != slug {
                    return;
                }
                core.issue_view.sending = false;
                core.issue_view.needs_login = matches!(&result, Err(Error::Api { status: 401, .. })) || matches!(&result, Err(Error::Api { code, .. }) if code == "github_reauth");
                match result {
                    Ok(answer) => Some(answer),
                    Err(Error::Api { status: 401, .. }) => {
                        core.issue_view.send_error = "Your login ran out. Log in again to write.".into();
                        None
                    }
                    Err(e) => {
                        core.issue_view.send_error = e.to_string();
                        None
                    }
                }
            };
            match answer {
                Some(answer) => done(&ui, &shared, answer),
                None => refresh(&ui, &shared),
            }
        });
    });
}

/// Creates the issue of the form; once created the list reloads and the new issue shows.
fn on_issues_submit(ui: &AppWindow, shared: &Shared) {
    let kind = usize::try_from(ui.get_issues_new_kind() - 1).ok().and_then(|i| controller::ISSUE_KINDS.get(i)).map_or("", |(k, _)| k);
    let body = match controller::issue_body(kind, &ui.get_issues_new_title(), &ui.get_issues_new_body()) {
        Ok(body) => body,
        Err(why) => {
            lock(shared).issue_view.send_error = why.into();
            return refresh(ui, shared);
        }
    };
    if lock(shared).issue_view.sending {
        return;
    }
    post_as_github_user(ui, shared, "issues".into(), body, |ui, shared, answer| {
        ui.set_issues_new_kind(0);
        ui.set_issues_new_title(SharedString::new());
        ui.set_issues_new_body(SharedString::new());
        load_detail_list(ui, shared, 2);
        let number = serde_json::from_str::<serde_json::Value>(&answer).ok().and_then(|v| v["number"].as_i64());
        if let Some(number) = number {
            on_issue_show(ui, shared, number as i32);
        }
    });
}

/// Answers the issue shown; the issue reloads with the reply.
fn on_issue_send_reply(ui: &AppWindow, shared: &Shared) {
    let text = ui.get_issue_reply_draft().trim().to_string();
    let number = {
        let core = lock(shared);
        if core.issue_view.sending || core.issue_view.mode != 2 {
            return;
        }
        core.issue_view.number
    };
    if text.is_empty() {
        return;
    }
    post_as_github_user(ui, shared, format!("issues/{number}/comments"), serde_json::json!({ "body": text }), |ui, shared, _| {
        ui.set_issue_reply_draft(SharedString::new());
        let number = lock(shared).issue_view.number;
        on_issue_show(ui, shared, number as i32);
    });
}

/// Picks the comment `id` to answer; the box says to whom.
fn on_comment_reply(ui: &AppWindow, shared: &Shared, id: i32) {
    {
        let mut core = lock(shared);
        let Some(c) = core.comments.as_mut() else { return };
        let Some(author) = c.loaded.iter().find(|m| m.id == i64::from(id)).map(|m| m.author.clone()) else { return };
        c.reply_to = Some((i64::from(id), author));
        c.composing = true;
        c.send_error.clear();
    }
    refresh(ui, shared);
}

/// Closes the dialog to write; the text stays for the next time.
fn on_comment_cancel_reply(ui: &AppWindow, shared: &Shared) {
    if let Some(c) = lock(shared).comments.as_mut() {
        c.reply_to = None;
        c.composing = false;
        c.send_error.clear();
        c.needs_login = false;
    }
    refresh(ui, shared);
}

/// Opens the dialog for a new comment.
fn on_comment_compose(ui: &AppWindow, shared: &Shared) {
    if let Some(c) = lock(shared).comments.as_mut() {
        c.reply_to = None;
        c.composing = true;
        c.send_error.clear();
    }
    refresh(ui, shared);
}

/// Shows the addon of a website link in Browse, in front of the other windows.
fn open_link(ui: &AppWindow, shared: &Shared, id: String) {
    if ui.get_needs_setup() {
        return;
    }
    if lock(shared).registry.get(&id).is_none() {
        lock(shared).notify(format!("The addon \"{id}\" from the link is not in the list of verified addons."), Kind::Error);
        refresh(ui, shared);
        return;
    }
    ui.set_page(1);
    on_row_details(ui, shared, id);
    ui.window().set_minimized(false);
    ui.window().with_winit_window(|w| w.focus_window());
}

#[cfg(test)]
mod card_tests {
    use super::*;

    #[test]
    fn a_new_issue_frame_replaces_what_was_stored() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::new(dir.path().to_path_buf(), None);
        core.platform.info.insert("dragon".into(), controller::AddonInfo::default());
        core.platform.issues_ready.insert("dragon".into(), false);
        assert!(core.take_issue_frame("dragon", true, true), "the tab was turned on and the App may write issues");
        assert!(core.platform.info["dragon"].issues_on);
        assert!(core.platform.issues_ready["dragon"]);
        assert!(!core.take_issue_frame("dragon", true, true), "the same answer changes nothing");
    }

    #[test]
    fn running_work_shows_in_one_toast_that_turns_into_its_result() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::new(dir.path().to_path_buf(), None);
        // The empty data folder has no AddOns folder to read.
        core.toasts = Toasts::default();
        let id = core.set_status("Updating 2 addons...".into());
        assert_eq!(core.set_status("Updating 1 of 2: Lucky".into()), id, "the next step changes the same toast");
        core.notify("2 addons updated".into(), Kind::Success);
        assert_eq!(core.toasts.items(), [slint_toast::Toast { id, text: "2 addons updated".into(), kind: Kind::Success }]);
        core.notify("Azeroth Survivors was closed.".into(), Kind::Info);
        assert_eq!(core.toasts.items().len(), 2, "without running work a result comes as its own toast");
    }

    #[test]
    fn an_installed_addon_shows_the_same_card_in_browse_and_installed() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::new(dir.path().to_path_buf(), None);
        let (installed, other) = (core.registry.addons[0].clone(), core.registry.addons[1].clone());
        core.state.upsert(survivorsforge::state::Entry {
            id: installed.id.clone(),
            name: installed.name.clone(),
            source: Source::Verified { registry_id: installed.id.clone() },
            folders: installed.folders.clone(),
            version: "v1".into(),
            installed_at: 0,
        });
        core.statuses.insert(installed.id.clone(), UpdateStatus::UpToDate);
        let rows = controller::installed_rows(&core.state, &core.reconciled, &core.scan, &core.registry, &core.statuses);
        let browse = |id: &str| {
            let b = controller::browse_rows(&core.registry, &core.state).into_iter().find(|b| b.id == id).unwrap();
            browse_row(b, &core, &rows)
        };
        let card = installed_row(rows.iter().find(|r| r.id == installed.id).unwrap(), &core);
        let shown = browse(&installed.id);
        assert!(shown.can_remove, "Remove shows in Browse");
        // As text: two empty images never compare equal.
        assert_eq!(format!("{:?}", AddonRow { can_pick: card.can_pick, picked: card.picked, ..shown }), format!("{card:?}"));
        assert_eq!(browse(&other.id).action, "Install", "an addon that is not installed keeps its Browse card");
    }

    #[test]
    fn saving_settings_again_replaces_the_saved_toast() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::new(dir.path().to_path_buf(), None);
        core.toasts = Toasts::default();
        core.settings_saved("Settings saved.".into(), Kind::Success);
        core.settings_saved("Settings saved.".into(), Kind::Success);
        assert_eq!(core.toasts.items().len(), 1, "one tick after another: one toast");
    }

    #[test]
    fn a_description_that_did_not_load_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut core = Core::new(dir.path().to_path_buf(), None);
        let id = core.registry.addons[0].id.clone();
        core.detail = Some((id.clone(), true));
        core.fetching.insert(id.clone());
        assert!(!detail(&core, &[]).unwrap().about_failed, "still asking the platform");
        core.fetching.remove(&id);
        assert!(detail(&core, &[]).unwrap().about_failed, "the platform did not answer and nothing was stored");
    }

    #[test]
    fn the_splash_shows_its_bar_after_400_ms() {
        i_slint_backend_testing::init_no_event_loop();
        let splash = Splash::new().unwrap();
        splash.show().unwrap();
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(399));
        assert!(!splash.get_bar_shown(), "a quick start shows only the icon");
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(2));
        assert!(splash.get_bar_shown());
    }

    #[test]
    fn a_card_says_why_it_cannot_be_installed() {
        assert_eq!(not_installable(Some(forge_core::trust::Trust::Blocked)), "Blocked by the admins");
        assert_eq!(not_installable(Some(forge_core::trust::Trust::SourceChanged)), "Repo changed: waits for a new check");
    }
}

#[cfg(test)]
mod layout_tests {
    use i_slint_backend_testing::ElementRoot;
    use slint::{ComponentHandle, ModelRc, VecModel};

    // The description and the first fact of the detail view, in a plain layout (no spare room
    // hides a short measurement there) and in the ScrollPanel the detail view uses.
    slint::slint! {
        import { AboutBlockView, Fact, ScrollPanel, AboutBlock } from "ui/components.slint";
        export component Probe inherits Window {
            in property <[AboutBlock]> about;
            in property <bool> scroll;
            width: 1100px;
            height: 6000px;
            if !root.scroll : VerticalLayout {
                spacing: 18px;
                alignment: start;
                VerticalLayout {
                    spacing: 10px;
                    for block in root.about : AboutBlockView { block: block; }
                }
                Fact { label: "Safety check"; text: "The automatic check found nothing."; }
            }
            if root.scroll : ScrollPanel {
                spacing: 18px;
                VerticalLayout {
                    spacing: 10px;
                    for block in root.about : AboutBlockView { block: block; }
                }
                Fact { label: "Safety check"; text: "The automatic check found nothing."; }
            }
        }
    }

    fn block(kind: i32, marker: &str, text: &str) -> AboutBlock {
        AboutBlock { kind, marker: marker.into(), text: text.into(), ..Default::default() }
    }

    /// The room between the end of the description and the fact below; negative when they
    /// overlap. `None` without element names, which only `SLINT_EMIT_DEBUG_INFO=1` builds have.
    fn gap(scroll: bool, about: Vec<AboutBlock>) -> Option<f32> {
        let ui = Probe::new().unwrap();
        ui.set_about(ModelRc::new(VecModel::from(about)));
        ui.set_scroll(scroll);
        ui.show().unwrap();
        // Conditional and repeated elements exist once the layout ran: ask sizes until nothing is new.
        let mut seen = 0;
        let all = loop {
            let all = ui.root_element().query_descendants().find_all();
            all.iter().for_each(|e| { e.size(); });
            if all.len() == seen {
                break all;
            }
            seen = all.len();
        };
        let of_type = |name: &str| all.iter().filter(|e| e.type_name().is_some_and(|t| t == name)).cloned().collect::<Vec<_>>();
        let top = of_type("Fact").first()?.absolute_position().y;
        let bottom = of_type("AboutBlockView").iter().map(|b| b.absolute_position().y + b.size().height).fold(0.0, f32::max);
        Some(top - bottom)
    }

    #[test]
    fn a_long_description_never_runs_into_the_facts_below() {
        i_slint_backend_testing::init_no_event_loop();
        let long = "ASDrops listens to the loot chat messages and to the boss-kill banners, and records each drop with its rarity and type. ".repeat(4);
        let cases: Vec<(&str, Vec<AboutBlock>)> = vec![
            ("paragraphs", (0..8).map(|_| block(0, "", &long)).collect()),
            ("line breaks", (0..8).map(|_| block(0, "", &format!("{long}\nmore"))).collect()),
            ("headings", (0..8).map(|n| block(1, "", &format!("Heading {n}"))).collect()),
            ("list items", (0..8).map(|_| block(2, "\u{2022}", &long)).collect()),
            ("quotes", (0..8).map(|_| block(4, "", &long)).collect()),
            ("code", (0..8).map(|_| block(3, "", &long)).collect()),
        ];
        for (name, about) in cases {
            for scroll in [false, true] {
                let Some(gap) = gap(scroll, about.clone()) else {
                    eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                    return;
                };
                assert!(gap >= 0.0, "{name} (scroll panel: {scroll}) runs {} px into the facts", -gap);
            }
        }
    }

    mod comments {
        use i_slint_backend_testing::ElementHandle;
        use slint::{ComponentHandle, ModelRc, VecModel};

        slint::slint! {
            import { CommentsView, CommentRow } from "ui/components.slint";
            export component CommentsProbe inherits Window {
                in property <[CommentRow]> rows;
                width: 700px;
                height: 4000px;
                CommentsView { name: "Lucky"; rows: root.rows; }
            }
        }

        #[test]
        fn a_long_comment_never_runs_into_the_next() {
            i_slint_backend_testing::init_no_event_loop();
            let long = "Works great with the Mage, and the loot filter is much faster since the last version. ".repeat(6);
            let row = |author: &str, body: &str, depth: i32| CommentRow { author: author.into(), initial: "B".into(), date: "2026-11-01".into(), body: body.into(), depth, ..Default::default() };
            let ui = CommentsProbe::new().unwrap();
            ui.set_rows(ModelRc::new(VecModel::from(vec![row("bob", &long, 0), row("carol", "Same here.", 1)])));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            let (Some(body), Some(next)) = (find(&long), find("carol")) else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            let bottom = body.absolute_position().y + body.size().height;
            assert!(body.size().height > 40.0, "the long comment wraps onto several lines: {}", body.size().height);
            assert!(next.absolute_position().y > bottom, "the reply starts {} px into the comment above", bottom - next.absolute_position().y);
            assert!(next.absolute_position().x > body.absolute_position().x, "the reply is indented");
        }
    }

    mod detail {
        use i_slint_backend_testing::ElementHandle;
        use slint::ComponentHandle;

        slint::slint! {
            import { DetailView, AddonDetail, Shot } from "ui/components.slint";
            export component DetailProbe inherits Window {
                in property <AddonDetail> detail;
                width: 1100px;
                height: 900px;
                DetailView { detail: root.detail; }
            }
        }

        #[test]
        fn the_detail_actions_are_icon_buttons() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = DetailProbe::new().unwrap();
            let shots = slint::ModelRc::new(slint::VecModel::from(vec![Shot::default()]));
            let mut detail = AddonDetail { on_website: true, issues_ready: true, comments_on: true, bug_label: "Report a bug".into(), shots, ..Default::default() };
            detail.row.name = "Lucky".into();
            detail.row.can_open = true;
            detail.row.can_remove = true;
            ui.set_detail(detail);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("< Back to the list").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            for label in ["Open on the website", "GitHub", "Scan locally", "Remove Lucky", "Report a bug", "Report abuse"] {
                let button = find(label).unwrap_or_else(|| panic!("{label} is there"));
                let size = button.size();
                assert!((size.width - size.height).abs() < 1.0, "{label} is a square icon button: {size:?}");
            }
            let role = |label: &str, role| ElementHandle::find_by_accessible_label(&ui, label).any(|e| e.accessible_role() == Some(role));
            assert!(!role("Comments", i_slint_backend_testing::AccessibleRole::Button), "Comments is a tab now, not also a button");
            let tab_x = |label: &str| {
                ElementHandle::find_by_accessible_label(&ui, label)
                    .find(|e| e.accessible_role() == Some(i_slint_backend_testing::AccessibleRole::Tab))
                    .unwrap_or_else(|| panic!("{label} tab"))
                    .absolute_position()
                    .x
            };
            let order: Vec<f32> = ["Overview", "Screenshots", "Issues", "Comments", "Versions"].iter().map(|t| tab_x(t)).collect();
            assert!(order.windows(2).all(|w| w[0] < w[1]), "tabs in the website's order: {order:?}");        }
    }

    mod dropdown {
        use slint::platform::{Key, WindowEvent};
        use slint::ComponentHandle;

        slint::slint! {
            import { Dropdown } from "ui/components.slint";
            export component DropdownProbe inherits Window {
                in-out property <int> picked;
                width: 300px;
                height: 400px;
                Dropdown {
                    y: 10px;
                    height: 38px;
                    label: "Sort by";
                    model: ["Most downloads", "Recently updated", "Name"];
                    current-index <=> root.picked;
                }
            }
        }

        fn press(ui: &DropdownProbe, key: Key) {
            let text: slint::SharedString = key.into();
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text });
        }

        #[test]
        fn the_open_list_answers_the_keys() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = DropdownProbe::new().unwrap();
            ui.show().unwrap();
            press(&ui, Key::Tab);
            press(&ui, Key::Return);
            press(&ui, Key::DownArrow);
            press(&ui, Key::DownArrow);
            press(&ui, Key::Return);
            assert_eq!(ui.get_picked(), 2, "Return opens the list, Down walks it, Return picks");
            press(&ui, Key::Return);
            press(&ui, Key::UpArrow);
            press(&ui, Key::Escape);
            assert_eq!(ui.get_picked(), 2, "Escape closes it unchanged");
        }
    }

    mod settings {
        use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementRoot};
        use slint::platform::{Key, PointerEventButton, WindowEvent};
        use slint::ComponentHandle;
        use std::{cell::Cell, rc::Rc};

        fn press(ui: &crate::AppWindow, text: &str) {
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: text.into() });
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: text.into() });
        }

        #[test]
        fn the_toolbar_has_icon_buttons_that_name_themselves_on_hover() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_update_count(3);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).collect::<Vec<_>>();
            if find("Installed (0)").is_empty() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            for label in ["Tiles", "List", "Check for updates", "Install zip", "Add GitHub repo", "Update all (3)"] {
                let button = find(label).into_iter().next().unwrap_or_else(|| panic!("{label} is there"));
                let size = button.size();
                assert!((size.width - size.height).abs() < 1.0, "{label} is a square icon button: {size:?}");
            }
            let search = ui.root_element().query_descendants().match_accessible_role(AccessibleRole::TextInput).find_first().unwrap();
            let tiles = find("Tiles").remove(0);
            let search_bottom = search.absolute_position().y + search.size().height;
            assert!(tiles.absolute_position().y >= search_bottom, "the view toggle is in the filter row, under the search");
            let right = |e: &ElementHandle| e.absolute_position().x + e.size().width;
            let (list, update) = (find("List").remove(0), find("Update all (3)").remove(0));
            assert!((right(&list) - right(&update)).abs() < 1.0, "at the right end of that row, under the last icon button");
            let check = find("Check for updates").remove(0);
            let at = check.absolute_position();
            let middle = slint::LogicalPosition::new(at.x + check.size().width / 2.0, at.y + check.size().height / 2.0);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: middle });
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
            let tip = find("Check for updates").into_iter().find(|e| e.accessible_role() == Some(AccessibleRole::Text));
            let tip = tip.expect("hovering shows the tooltip");
            assert!(tip.absolute_position().y >= at.y + check.size().height, "the tooltip sits under the button");
        }

        #[test]
        fn fields_and_choices_say_what_they_are() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.show().unwrap();
            let role = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).find_map(|e| e.accessible_role());
            if ElementHandle::find_by_accessible_label(&ui, "Installed (0)").next().is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert_eq!(role("Search addons"), Some(AccessibleRole::TextInput), "the search names itself, not only by its placeholder");
            for choice in ["Safety check", "Sort by"] {
                assert_eq!(role(choice), Some(AccessibleRole::Combobox), "{choice}");
            }
            let tab = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).find(|e| e.accessible_role() == Some(AccessibleRole::Tab)).unwrap();
            assert_eq!(tab("Installed (0)").accessible_item_selected(), Some(true), "the open page is the selected tab");
            assert_eq!(tab("Settings").accessible_item_selected(), Some(false));
            // Tab walks onto the page tabs (after the first focusable control); Return opens one.
            press(&ui, "\t");
            press(&ui, "\t");
            press(&ui, "\t");
            press(&ui, &slint::SharedString::from(Key::Return));
            assert_ne!(ui.get_page(), 0, "the keyboard reaches another page tab and opens it");
        }

        #[test]
        fn escape_closes_a_dialog_and_return_keeps_a_dangerous_one_safe() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let (cancelled, accepted) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let (c, a) = (cancelled.clone(), accepted.clone());
            ui.on_confirm_cancel(move || c.set(c.get() + 1));
            ui.on_confirm_accept(move || a.set(a.get() + 1));
            ui.show().unwrap();
            ui.set_confirm_danger(true);
            ui.set_confirm_ok("Quit anyway".into());
            ui.set_confirm_open(true);
            press(&ui, &slint::SharedString::from(Key::Return));
            assert_eq!((cancelled.get(), accepted.get()), (1, 0), "Return presses Cancel, which has the focus");
            press(&ui, &slint::SharedString::from(Key::Escape));
            assert_eq!((cancelled.get(), accepted.get()), (2, 0), "Escape cancels too");
        }

        #[test]
        fn offline_stays_said_with_a_way_to_try_again() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let tried = Rc::new(Cell::new(false));
            let t = tried.clone();
            ui.on_check_updates(move || t.set(true));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert!(find("Try again").is_none(), "online: no line");
            ui.set_offline(true);
            find("Try again").expect("offline: the line with its button").invoke_accessible_default_action();
            assert!(tried.get(), "Try again checks again");
        }

        #[test]
        fn ctrl_f_goes_to_the_search_and_f5_checks_again() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let checks = Rc::new(Cell::new(0));
            let c = checks.clone();
            ui.on_check_updates(move || c.set(c.get() + 1));
            ui.show().unwrap();
            let search = ElementHandle::find_by_accessible_label(&ui, "Search addons").next();
            let Some(search) = search else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            press(&ui, &slint::SharedString::from(Key::F5));
            assert_eq!(checks.get(), 1, "F5 checks for updates");
            let control: slint::SharedString = Key::Control.into();
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: control.clone() });
            press(&ui, "f");
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: control });
            press(&ui, "x");
            assert_eq!(ui.get_search().as_str(), "x", "Ctrl+F put the cursor into the search");
            assert_eq!(search.accessible_value().as_deref(), Some("x"));
        }

        #[test]
        fn the_addons_folder_opens_from_the_bottom_line() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let opened = Rc::new(Cell::new(false));
            let o = opened.clone();
            ui.on_open_addons_dir(move || o.set(true));
            ui.set_addons_dir("C:/Games/AzerothSurvivors/AddOns".into());
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            find("Open folder").expect("next to the path").invoke_accessible_default_action();
            assert!(opened.get());
            ui.set_game_mode(true);
            assert!(find("Open folder").is_none(), "Game Mode has no file manager to open");
        }

        #[test]
        fn the_search_clears_with_its_button_and_with_escape() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let searched = Rc::new(std::cell::RefCell::new(None::<String>));
            let s = searched.clone();
            ui.on_search_changed(move |text| *s.borrow_mut() = Some(text.to_string()));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert!(find("Clear search").is_none(), "nothing to clear yet");
            ui.set_search("lucky".into());
            find("Clear search").expect("a typed search can be cleared").invoke_accessible_default_action();
            assert_eq!((ui.get_search().as_str(), searched.borrow().as_deref()), ("", Some("")), "the button empties it");
            ui.set_search("lucky".into());
            *searched.borrow_mut() = None;
            press(&ui, &slint::SharedString::from(Key::Escape));
            assert_eq!((ui.get_search().as_str(), searched.borrow().as_deref()), ("", Some("")), "Escape empties it too");
        }

        #[test]
        fn a_version_opens_its_release_notes() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let opened = Rc::new(std::cell::RefCell::new(String::new()));
            let o = opened.clone();
            ui.on_detail_link(move |page| *o.borrow_mut() = page.to_string());
            let version = crate::VersionLine { tag: "v1.2".into(), name: "Lucky 1.2".into(), can_install: true, ..Default::default() };
            ui.set_versions_rows(slint::ModelRc::new(slint::VecModel::from(vec![version])));
            ui.set_detail_page(0);
            ui.set_detail_tab(4);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("< Back to the list").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            find("Release notes of Lucky 1.2").expect("each version links its notes").invoke_accessible_default_action();
            assert_eq!(*opened.borrow(), "release/v1.2");
            ui.set_game_mode(true);
            assert!(find("Release notes of Lucky 1.2").is_none(), "Game Mode opens no browser");
        }

        #[test]
        fn the_game_waits_while_addons_are_installed() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert_eq!(find("Launch game").expect("the start button").accessible_enabled(), Some(true));
            ui.set_addons_busy(true);
            assert!(find("Launch game").is_none(), "it says what it waits for");
            assert_eq!(find("Installing addons...").expect("the waiting button").accessible_enabled(), Some(false));
            ui.set_game_update("Update to v0.133.0".into());
            assert_eq!(find("Update to v0.133.0").expect("the game update").accessible_enabled(), Some(true), "the game's own update does not touch the addons");
        }

        #[test]
        fn an_empty_filtered_list_offers_to_clear_the_filters() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let cleared = Rc::new(Cell::new(false));
            let c = cleared.clone();
            ui.on_search_changed(move |text| c.set(text.is_empty()));
            ui.show().unwrap();
            if ElementHandle::find_by_accessible_label(&ui, "Installed (0)").next().is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            ui.set_search("zzz".into());
            let clear = ElementHandle::find_by_accessible_label(&ui, "Clear search and filters").next().expect("a way out of an empty filtered list");
            clear.invoke_accessible_default_action();
            assert!(cleared.get() && ui.get_search().is_empty(), "the search is gone");
        }

        #[test]
        fn the_platform_addresses_wait_under_advanced() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_page(2);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert!(find("Platform API address").is_none(), "hidden at first: hardly anyone changes it");
            find("Show advanced settings").expect("a way to them").invoke_accessible_default_action();
            assert!(find("Platform API address").is_some() && find("Platform website address").is_some());
            find("Hide advanced settings").expect("and back").invoke_accessible_default_action();
            assert!(find("Platform API address").is_none());
            ui.set_settings_api_url("http://localhost:3081".into());
            assert!(find("Show advanced settings (another platform is set)").is_some(), "a changed address says so while hidden");
        }

        #[test]
        fn a_refused_setting_says_why_under_its_field() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_page(2);
            ui.set_settings_api_error("\"x\" is not a web address (http:// or https://)".into());
            ui.show().unwrap();
            if ElementHandle::find_by_accessible_label(&ui, "Installed (0)").next().is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            let text = "Error: \"x\" is not a web address (http:// or https://)";
            assert!(ElementHandle::find_by_accessible_label(&ui, text).next().is_some(), "the reason stands under the field");
        }

        #[test]
        fn a_long_failure_stays_in_its_row() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let failure = "Failed: Network error: https://github.com/migwynkriid13/SurvivorsDragonUi/releases/download/build-cb544e7127f49220afa1951053c6e1a32740cb3a/SurvivorsDragonUi.zip: Network Error: Error encountered in the status line: A connection attempt failed because the connected party did not properly respond after a period of time (os error 10060)";
            let rows = vec![
                crate::AddonRow { id: "a".into(), name: "SurvivorsDragonUi".into(), status: failure.into(), status_kind: 3, ..Default::default() },
                crate::AddonRow { id: "b".into(), name: "ASTinyPlates".into(), status: "Up to date".into(), status_kind: 1, ..Default::default() },
            ];
            ui.set_installed_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
            ui.set_installed_tiles(false);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            let Some(status) = find(failure) else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            let next = find("ASTinyPlates").expect("the next row");
            let bottom = status.absolute_position().y + status.size().height;
            assert!(bottom <= next.absolute_position().y - 8.0, "the failure ends inside its row: {bottom} vs next row at {}", next.absolute_position().y);
        }

        #[test]
        fn a_long_failure_on_a_tile_shows_whole_on_hover() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let failure = "Failed: Network error: the connected party did not properly respond after a period of time (os error 10060)";
            let rows = vec![crate::AddonRow { id: "a".into(), name: "SurvivorsDragonUi".into(), status: failure.into(), status_kind: 3, ..Default::default() }];
            ui.set_installed_rows(slint::ModelRc::new(slint::VecModel::from(rows)));
            ui.set_installed_tiles(true);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).collect::<Vec<_>>();
            let Some(status) = find(failure).into_iter().next() else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            let (at, size) = (status.absolute_position(), status.size());
            let middle = slint::LogicalPosition::new(at.x + size.width / 2.0, at.y + size.height / 2.0);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position: middle });
            i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(600));
            let texts = find(failure).into_iter().filter(|e| e.accessible_role() == Some(AccessibleRole::Text)).count();
            assert!(texts > 1, "the shortened status has the whole text as its tooltip");
        }

        #[test]
        fn a_toast_shows_bottom_right_and_closes() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let closed = Rc::new(Cell::new(0));
            let counter = closed.clone();
            ui.on_toast_dismiss(move |id| counter.set(id));
            let toast = crate::Toast { id: 7, text: "Lucky installed".into(), kind: slint_toast::Kind::Busy as i32 };
            ui.set_toasts(slint::ModelRc::new(slint::VecModel::from(vec![toast])));
            ui.show().unwrap();
            let Some(card) = ElementHandle::find_by_accessible_label(&ui, "Lucky installed").next() else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            let window = ui.window().size().to_logical(ui.window().scale_factor());
            let (at, size) = (card.absolute_position(), card.size());
            assert!(size.height > 20.0, "the toast has room for its text: {size:?}");
            assert!(at.x + size.width > window.width / 2.0 && at.y + size.height > window.height / 2.0, "bottom right: {at:?} in {window:?}");
            ElementHandle::find_by_accessible_label(&ui, "Close message").next().unwrap().mock_single_click(PointerEventButton::Left);
            assert_eq!(closed.get(), 7, "the X closes it");
        }

        #[test]
        fn the_github_account_logs_in_waits_and_logs_out() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let (logins, cancels, logouts) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let (l, c, o) = (logins.clone(), cancels.clone(), logouts.clone());
            ui.on_login(move || l.set(l.get() + 1));
            ui.on_login_cancel(move || c.set(c.get() + 1));
            ui.on_logout(move || o.set(o.get() + 1));
            ui.set_page(2);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            find("Log in with GitHub").expect("logged out: a login button").invoke_accessible_default_action();
            assert_eq!(logins.get(), 1);

            ui.set_account_busy(true);
            assert!(find("Log in with GitHub").is_none(), "no second login while the browser is open");
            find("Cancel").expect("the wait can be cancelled").invoke_accessible_default_action();
            assert_eq!(cancels.get(), 1);

            ui.set_account_busy(false);
            ui.set_account_error("Port 47616 is in use by another program, so the login cannot come back".into());
            assert!(find("Error: Port 47616 is in use by another program, so the login cannot come back").is_some(), "the reason stands under the row");

            ui.set_account_error("".into());
            ui.set_account_login("octo".into());
            assert!(find("octo").is_some(), "the login name");
            assert!(find("You write comments and issues in the launcher as octo.").is_some(), "the hint fits the login");
            find("Log out").expect("logged in: a logout button").invoke_accessible_default_action();
            assert_eq!(logouts.get(), 1);
        }

        #[test]
        fn comments_are_written_and_answered_in_a_dialog() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let (logins, sends, replies, composes, cancels) =
                (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let (l, s, r, c, x) = (logins.clone(), sends.clone(), replies.clone(), composes.clone(), cancels.clone());
            ui.on_login(move || l.set(l.get() + 1));
            ui.on_comments_send(move || s.set(s.get() + 1));
            ui.on_comments_reply(move |id| r.set(id));
            ui.on_comments_compose(move || c.set(c.get() + 1));
            ui.on_comments_cancel_reply(move || x.set(x.get() + 1));
            ui.set_page(1);
            ui.set_detail_page(1);
            ui.set_detail_tab(3);
            let row = crate::CommentRow { id: 42, author: "kex".into(), body: "Great".into(), ..Default::default() };
            let mine = crate::CommentRow { id: 43, author: "octo".into(), body: "Mine".into(), own: true, ..Default::default() };
            ui.set_comments_rows(slint::ModelRc::new(slint::VecModel::from(vec![row, mine])));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Great").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert!(find("Write a comment").is_none() && find("Reply").is_none(), "logged out: no button to write, no Reply");
            find("Log in to write").expect("logged out: a way to log in").invoke_accessible_default_action();
            assert_eq!(logins.get(), 1);

            ui.set_account_login("octo".into());
            assert!(find("Your comment").is_none(), "the list shows comments only, no box");
            assert!(find("Edit or delete your comments on the website.").is_none(), "no rule above the list: the link sits at one's own comments");
            assert!(find("octo (you)").is_some() && find("kex (you)").is_none(), "one's own comment is marked");
            find("Write a comment").expect("logged in: a button to write").invoke_accessible_default_action();
            assert_eq!(composes.get(), 1);
            find("Reply").expect("a Reply link on each comment").invoke_accessible_default_action();
            assert_eq!(replies.get(), 42);

            ui.set_comments_compose_open(true);
            assert!(find("New comment").is_some() && find("Your comment").is_some(), "the dialog with its box");
            slint::platform::update_timers_and_animations();
            press(&ui, "x");
            assert_eq!(ui.get_comments_draft(), "x", "the cursor starts in the box");
            let ctrl = slint::SharedString::from(Key::Control);
            ui.window().dispatch_event(WindowEvent::KeyPressed { text: ctrl.clone() });
            press(&ui, &slint::SharedString::from(Key::Return));
            ui.window().dispatch_event(WindowEvent::KeyReleased { text: ctrl });
            assert_eq!(sends.get(), 1, "Ctrl+Enter sends");
            assert_eq!(ui.get_comments_draft(), "x", "and adds no new line");

            ui.set_comments_reply_to("kex".into());
            assert!(find("Reply to kex").is_some(), "the dialog says to whom");
            ui.set_comments_draft("Thanks!".into());
            find("Send reply").expect("a send button for the reply").invoke_accessible_default_action();
            assert_eq!(sends.get(), 2);
            ui.set_comments_send_error("At most 20 comments an hour, try again later".into());
            assert!(find("Error: At most 20 comments an hour, try again later").is_some(), "why it was not sent");
            assert_eq!(ui.get_comments_draft(), "Thanks!", "the text stays");

            // A long text scrolls inside the box: the dialog and its buttons stay in the window.
            ui.set_comments_send_error("".into());
            ui.set_comments_draft((1..=60).map(|n| format!("line {n}")).collect::<Vec<_>>().join("
").into());
            let window = ui.window().size().to_logical(ui.window().scale_factor());
            let send = find("Send reply").unwrap();
            assert!(send.absolute_position().y + send.size().height <= window.height, "Send stays in the window: {:?} in {window:?}", send.absolute_position());
            assert!(find("Reply to kex").unwrap().absolute_position().y >= 0.0, "the title too");
            press(&ui, &slint::SharedString::from(Key::Escape));
            assert_eq!(cancels.get(), 1, "Escape closes the dialog");
            find("Cancel").expect("and so does Cancel").invoke_accessible_default_action();
            assert_eq!(cancels.get(), 2);
        }

        #[test]
        fn issues_are_read_here_and_written_when_the_addon_takes_them() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let (shown, news, submits, replies, logins) =
                (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
            let (s, n, u, r, l) = (shown.clone(), news.clone(), submits.clone(), replies.clone(), logins.clone());
            ui.on_issue_show(move |number| s.set(number));
            ui.on_issues_new(move || n.set(n.get() + 1));
            ui.on_issues_submit(move || u.set(u.get() + 1));
            ui.on_issue_send_reply(move || r.set(r.get() + 1));
            ui.on_login(move || l.set(l.get() + 1));
            ui.set_page(1);
            ui.set_detail_page(1);
            ui.set_detail_tab(2);
            let line = crate::IssueLine { number: 12, title: "Crash on start".into(), author: "kex".into(), comments: 1, date: "2026-11-01".into() };
            ui.set_issues_rows(slint::ModelRc::new(slint::VecModel::from(vec![line])));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            let Some(row) = find("#12 Crash on start") else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            row.invoke_accessible_default_action();
            assert_eq!(shown.get(), 12, "an issue opens in the launcher");
            assert!(find("Report a bug").is_some() && find("New issue").is_none(), "the addon takes no issues here: GitHub's way");

            let mut detail = ui.get_detail();
            detail.issues_ready = true;
            ui.set_detail(detail);
            assert!(find("Log in in Desktop Mode to write.").is_none());
            ui.set_game_mode(true);
            assert!(find("Log in in Desktop Mode to write.").is_some(), "Game Mode: why the login is off");
            ui.set_game_mode(false);
            find("Log in to write").expect("logged out: a way to log in").invoke_accessible_default_action();
            assert_eq!(logins.get(), 1);
            ui.set_account_login("octo".into());
            find("New issue").expect("logged in: a new issue").invoke_accessible_default_action();
            assert_eq!(news.get(), 1);

            ui.set_issues_mode(1);
            assert!(find("Issue title").is_some() && find("Issue description").is_some(), "the form");
            for label in ["Kind", "Title", "Description"] {
                assert!(find(label).is_some(), "a visible label stays while typing: {label}");
            }
            find("Create issue").unwrap().invoke_accessible_default_action();
            assert_eq!(submits.get(), 1);
            ui.set_issues_send_error("Give the issue a title".into());
            assert!(find("Error: Give the issue a title").is_some());
            assert!(find("Log in again").is_none(), "a plain error offers no login");
            ui.set_issues_send_error("Log in again to post on GitHub".into());
            ui.set_issues_needs_login(true);
            find("Log in again").expect("the GitHub login ran out: a way back next to the error").invoke_accessible_default_action();
            assert_eq!(logins.get(), 2);
            ui.set_issues_needs_login(false);

            ui.set_issues_mode(2);
            ui.set_issues_send_error("".into());
            ui.set_issue_number(12);
            ui.set_issue_title("Crash on start".into());
            ui.set_issue_body("It crashes.".into());
            let reply = crate::IssueReplyRow { author: "kex".into(), date: "2026-11-02".into(), body: "Same here".into() };
            ui.set_issue_replies(slint::ModelRc::new(slint::VecModel::from(vec![reply])));
            assert!(find("It crashes.").is_some() && find("Same here").is_some(), "the issue with its replies");
            ui.set_issue_reply_draft("Fixed?".into());
            find("Send reply").unwrap().invoke_accessible_default_action();
            assert_eq!(replies.get(), 1);
            ui.set_issue_locked(true);
            assert!(find("Send reply").is_none(), "no reply box on a locked issue");
            assert!(find("This issue is locked. Only the repo's team can reply.").is_some(), "and it says why");
        }

        #[test]
        fn offline_the_user_logs_in_to_github_alone_with_a_code() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let devices = Rc::new(Cell::new(0));
            let d = devices.clone();
            ui.on_login_device(move || d.set(d.get() + 1));
            ui.set_page(2);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            assert!(find("Log in to GitHub only").is_none(), "online: only the platform login");
            let logins = Rc::new(Cell::new(0));
            let l = logins.clone();
            ui.on_login(move || l.set(l.get() + 1));
            ui.set_offline(true);
            find("Log in with GitHub").expect("the platform login stays in sight").invoke_accessible_default_action();
            assert_eq!(logins.get(), 0, "offline the platform login cannot work: no 5 minute wait for a dead page");
            ui.set_device_login_offered(true);
            find("Log in to GitHub only").expect("offline: the device login").invoke_accessible_default_action();
            assert_eq!(devices.get(), 1);

            ui.set_account_busy(true);
            ui.set_account_device_code("WDJB-MJHT".into());
            assert!(find("Device code WDJB-MJHT").is_some(), "the code to enter");
            let (hint, code) = (find("Enter this code at").unwrap(), find("Device code WDJB-MJHT").unwrap());
            assert!(code.absolute_position().y >= hint.absolute_position().y + hint.size().height, "the code stands on its own line, under the instruction");
            let opened = Rc::new(Cell::new(0));
            let o = opened.clone();
            ui.on_open_device_page(move || o.set(o.get() + 1));
            find("github.com/login/device").expect("the page is a link").invoke_accessible_default_action();
            assert_eq!(opened.get(), 1, "the link opens the page again");
            let copy = find("Copy code").expect("a button copies the code");
            copy.invoke_accessible_default_action();
            assert!(find("Copied").is_some(), "the button says it worked");
            ui.set_account_device_code("ABCD-EFGH".into());
            assert!(find("Copy code").is_some(), "a new code can be copied again");
            assert!(find("Waiting for the browser...").is_none());

            ui.set_account_busy(false);
            ui.set_account_device_code("".into());
            ui.set_account_github_only("octo".into());
            assert!(find("octo (GitHub only)").is_some() && find("Log out").is_some(), "logged in to GitHub alone");
            assert!(find("Log in to GitHub only").is_none());
            assert!(find("Log in with GitHub").is_none(), "not the same words as the login already done");
            assert!(find("Log in to SurvivorsForge too").is_some(), "the full login stays offered");
            ui.set_offline(false);
            find("Log in to SurvivorsForge too").unwrap().invoke_accessible_default_action();
            assert_eq!(logins.get(), 1, "back online it works again");
        }

        #[test]
        fn game_versions_say_loading_before_they_say_failed() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_page(2);
            ui.set_checking(true);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            let failed = "The platform does not answer, so its game versions are not loaded.";
            assert!(find("Loading the game versions...").is_some() && find(failed).is_none(), "while the first check runs");
            ui.set_checking(false);
            ui.set_settings_game_versions_failed(true);
            assert!(find(failed).is_some() && find("Loading the game versions...").is_none(), "after a check without them");
            ui.set_settings_game_versions_failed(false);
            assert!(find(failed).is_none() && find("The platform lists no game versions yet.").is_some(), "the platform answered with an empty list");
        }

        #[test]
        fn the_statistics_say_what_goes_out_point_by_point() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_page(2);
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            if find("Installed (0)").is_none() {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            }
            for line in [
                "Sent after an install, update or rollback of a verified addon: the addon and version, whether it worked (a fixed code if not), the launcher version and the operating system.",
                "Never sent: your name or file contents. No IP address is stored.",
                "The installation ID is a random number that counts active launchers. The platform forgets it after 90 days.",
            ] {
                assert!(find(line).is_some(), "a line of its own: {line}");
            }
            // Wrapped lines take their room: the second point starts below the whole first one.
            let first = find("Sent after an install, update or rollback of a verified addon: the addon and version, whether it worked (a fixed code if not), the launcher version and the operating system.").unwrap();
            let second = find("Never sent: your name or file contents. No IP address is stored.").unwrap();
            assert!(second.absolute_position().y >= first.absolute_position().y + first.size().height - 0.5, "no overlap");
        }

        #[test]
        fn settings_scroll_in_the_smallest_window() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            ui.set_page(2);
            ui.window().set_size(slint::LogicalSize::new(980.0, 660.0));
            ui.show().unwrap();
            let find = |label: &str| ElementHandle::find_by_accessible_label(&ui, label).next();
            let Some(advanced) = find("Show advanced settings") else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            // Open, the platform addresses at the end make the page its longest.
            advanced.invoke_accessible_default_action();
            let last = find("Platform website address").expect("the last setting");
            let before = last.absolute_position().y;
            assert!(before + last.size().height > 660.0, "the page is longer than the smallest window");
            let position = slint::LogicalPosition::new(600.0, 400.0);
            ui.window().dispatch_event(WindowEvent::PointerMoved { position });
            ui.window().dispatch_event(WindowEvent::PointerScrolled { position, delta_x: 0.0, delta_y: -600.0 });
            let after = last.absolute_position().y;
            assert!(after + last.size().height <= 660.0, "the wheel brings the last setting into view: {before} -> {after}");
        }

        #[test]
        fn settings_save_on_their_own_but_not_on_every_key() {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::AppWindow::new().unwrap();
            let saves = Rc::new(Cell::new(0));
            let counter = saves.clone();
            ui.on_settings_save(move || counter.set(counter.get() + 1));
            ui.set_page(2);
            ui.show().unwrap();
            let Some(tick) = ElementHandle::find_by_accessible_label(&ui, "Count my installs in the public statistics").next() else {
                eprintln!("skipped: build with SLINT_EMIT_DEBUG_INFO=1 to find elements");
                return;
            };
            assert!(ElementHandle::find_by_accessible_label(&ui, "Save").next().is_none(), "no Save button");

            tick.mock_single_click(PointerEventButton::Left);
            assert_eq!(saves.get(), 1, "a tick box saves at once");

            let field = ui.root_element().query_descendants().match_accessible_role(AccessibleRole::TextInput).find_first().unwrap();
            field.mock_single_click(PointerEventButton::Left);
            press(&ui, "x");
            press(&ui, "y");
            assert_eq!(saves.get(), 1, "typing alone does not save");
            press(&ui, &slint::SharedString::from(Key::Return));
            assert_eq!(saves.get(), 2, "Enter saves the field");

            press(&ui, "z");
            let installed = ElementHandle::find_by_accessible_label(&ui, "Installed (0)").next().unwrap();
            installed.mock_single_click(PointerEventButton::Left);
            assert_eq!(saves.get(), 3, "leaving the page saves what was typed");
        }
    }
}
