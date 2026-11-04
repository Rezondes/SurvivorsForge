#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use survivorsforge::config::{self, Config};
use survivorsforge::controller::{self, Badge, Row, RowState, UNMANAGED_PREFIX};
use survivorsforge::deep_link;
use survivorsforge::game_update::{self, ExeSeen, WebServer};
use survivorsforge::github::{normalize_repo, ApiGitHub, Client, GitHub, WebGitHub};
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

slint::include_modules!();

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
    status: String,
    status_error: bool,
    self_update: Option<Available>,
    /// The platform's game versions, oldest first; empty without the platform.
    game_versions: Vec<controller::GameVersion>,
    /// The last check could not reach the platform: installs and updates wait.
    offline: bool,
    /// The platform's categories; empty offline.
    categories: Vec<controller::Category>,
    /// The Browse category filter, `None` for all.
    category: Option<String>,
    /// Browse addons picked for an addon pack.
    picked: std::collections::BTreeSet<String>,
    pack_busy: bool,
    /// The game started from here still runs: the launch button waits for it to close.
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
            status: String::new(),
            status_error: false,
            self_update: None,
            game_versions: Vec::new(),
            offline: false,
            categories: Vec::new(),
            category: None,
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
            self.set_status("state.json was damaged and saved as state.json.bak; starting fresh.".into(), true);
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
        Client::new().with_install_id(telemetry::install_header(&self.config))
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
        if self.offline { Box::new(WebGitHub::new(self.client())) } else { Box::new(ApiGitHub::new(self.client(), &self.api_url())) }
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
            Err(e) => self.set_status(format!("Cannot read the AddOns folder: {e}"), true),
        }
    }

    fn save_state(&mut self) {
        if let Err(e) = self.state.save(&self.data_dir, &self.config.addons_dir) {
            self.set_status(format!("Could not save state.json: {e}"), true);
        }
    }

    fn set_status(&mut self, text: String, error: bool) {
        self.status = text;
        self.status_error = error;
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
        RowState::UpdateAvailable(label) => (format!("Update: {label}"), 2, "Update"),
        RowState::Manual => ("Installed from zip".into(), 0, ""),
        RowState::CheckFailed(_) => ("Check failed".into(), 3, ""),
        RowState::Missing => ("Folder missing".into(), 3, if row.badge == Badge::Zip { "" } else { "Reinstall" }),
        RowState::Unmanaged => ("Not managed".into(), 0, ""),
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

fn browse_row(b: controller::BrowseRow, core: &Core) -> AddonRow {
    let (status, status_kind) = match (b.installed, b.installable) {
        (true, _) => ("Installed".to_string(), 1),
        (false, false) => ("Not installable".to_string(), 3),
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
        badge: b.trust.map(str::to_string).unwrap_or(b.category).into(),
        description: b.description.into(),
        status: status.into(),
        status_kind,
        progress,
        action: if b.installed || !b.installable { "" } else { "Install" }.into(),
        busy: core.busy.contains(&b.id),
        can_remove: false,
        can_open: true,
        can_pick: core.registry.get(&b.id).is_some_and(controller::packable),
        picked: core.picked.contains(&b.id),
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
        (browse_row(b, core), Vec::new())
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
    let loaded = loaded.unwrap_or_default();
    let base = loaded.image_origin().unwrap_or_default();
    let mut about: Vec<AboutBlock> = forge_core::markdown::blocks(&loaded.about, &base).into_iter().map(|b| about_block(core, b)).collect();
    if about.is_empty() {
        about.push(about_block(core, forge_core::markdown::Block::Paragraph(row.description.to_string())));
    }
    let shots: Vec<Shot> = loaded.shots.iter().map(|(url, caption)| Shot { image: picture(core, Some(url)), caption: caption.into() }).collect();
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
        on_website: entry.is_some(),
        bug_label: entry.map(|a| controller::bug_report("", &a.id, &a.repo, loaded.issues_on, core.platform.issues_ready.get(&a.id).copied()).0).unwrap_or_default().into(),
        issues_ready: loaded.issues_on && entry.is_some_and(|a| core.platform.issues_ready.get(&a.id) == Some(&true)),
        comments_on: loaded.comments_on,
        can_auto_update: !*browse && matches!(core.state.get(id).map(|e| &e.source), Some(Source::Verified { .. })),
        auto_update: core.config.auto_updates(id),
        auto_update_all: core.config.auto_update_all,
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
    AboutBlock { kind, marker: marker.into(), text: text.into(), image: picture(core, url.as_deref()) }
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

/// Opens the detail view of a row. What was stored shows at once, its pictures from the disk; the
/// platform is asked once per start in the background and replaces it when it answers.
fn on_row_details(ui: &AppWindow, shared: &Shared, id: String) {
    ui.set_comments_open(false);
    let job = {
        let mut core = lock(shared);
        core.comments = None;
        core.detail = Some((id.clone(), ui.get_page() == 1));
        registry_entry(&core, &id).map(|a| a.id.clone()).map(|rid| {
            let ask = !core.refreshed.contains(&rid) && core.fetching.insert(rid.clone());
            (core.platform.info.get(&rid).cloned(), ask.then(|| (core.client(), core.api_url())), rid)
        })
    };
    refresh(ui, shared);
    ui.set_detail_page(ui.get_page());
    let Some((stored, ask, rid)) = job else { return };
    let (weak, shared) = (ui.as_weak(), shared.clone());
    let client = ask.as_ref().map_or_else(|| lock(&shared).client(), |(c, _)| c.clone());
    std::thread::spawn(move || {
        if let Some(stored) = &stored {
            let (icon, shots) = detail_pictures(stored);
            load_pictures(&weak, &shared, &client, icon, ICON_EDGE);
            load_pictures(&weak, &shared, &client, shots, SHOT_EDGE);
        }
        let Some((_, api)) = ask else { return };
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
        // Where bugs go: until the platform says it takes them, the button sends them to GitHub.
        if fresh.as_ref().is_some_and(|i| i.issues_on)
            && let Some(ready) = client.get_text_quick(&format!("{api}/v1/addons/{rid}/issues/frame")).ok().and_then(|t| controller::issues_ready(&t))
        {
            let mut core = lock(&shared);
            if core.platform.issues_ready.insert(rid.clone(), ready) != Some(ready) {
                core.save_platform();
            }
            drop(core);
            let redraw = shared.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &redraw));
        }
        if let Some(info) = fresh {
            let (icon, shots) = detail_pictures(&info);
            load_pictures(&weak, &shared, &client, icon, ICON_EDGE);
            load_pictures(&weak, &shared, &client, shots, SHOT_EDGE);
        }
    });
}

fn refresh(ui: &AppWindow, shared: &Shared) {
    let core = lock(shared);
    // The game version list arrives with the update check; fill it once it is there.
    if slint::Model::row_count(&ui.get_settings_game_versions()) != core.game_versions.len() + 1 {
        fill_game_versions(ui, &core);
    }
    let rows = controller::installed_rows(&core.state, &core.reconciled, &core.scan, &core.registry, &core.statuses);
    ui.set_installed_total(rows.len() as i32);
    ui.set_installed_rows(model(
        rows.iter()
            .filter(|r| controller::matches(&core.search, &r.name, &r.author))
            .map(|r| installed_row(r, &core))
            .collect(),
    ));
    let choices = controller::category_choices(&core.categories, &core.registry);
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
    ui.set_browse_rows(model(
        controller::browse_rows(&core.registry, &core.state)
            .into_iter()
            .filter(|b| controller::matches(&core.search, &b.name, &b.author))
            .filter(|b| core.category.as_deref().is_none_or(|w| controller::in_category(&b.category, w, &choices)))
            .map(|b| browse_row(b, &core))
            .collect(),
    ));
    match detail(&core, &rows) {
        Some(detail) => ui.set_detail(detail),
        // Removed meanwhile: back to the list.
        None => ui.set_detail_page(-1),
    }
    if let Some(c) = &core.comments {
        let rows: Vec<CommentRow> = controller::comment_threads(c.loaded.clone()).into_iter().map(|m| comment_row(&core, m)).collect();
        ui.set_comments_rows(ModelRc::new(VecModel::from(rows)));
        // Placeholders stand in for the first page only; older ones load under their button.
        let first = c.loaded.is_empty();
        ui.set_comments_waiting(match c.asked.filter(|_| first).map(|t| controller::waiting(t.elapsed())) {
            Some(controller::Waiting::Placeholders) => 1,
            Some(controller::Waiting::Slow) => 2,
            _ => 0,
        });
        ui.set_comments_loading_more(c.asked.is_some() && !first);
        ui.set_comments_failed(c.failed);
        ui.set_comments_off(c.off);
        ui.set_comments_more(c.older.is_some());
    }
    ui.set_update_count(controller::update_all_ids(&core.statuses).len() as i32);
    ui.set_checking(core.checking);
    let game_missing = match config::game_executable(core.addons_dir(), cfg!(windows)) {
        Some(_) => "",
        None if cfg!(windows) => "AzerothSurvivors.exe is not next to the benilla-config folder.",
        None => "No Linux build of the game next to benilla-config. Start a Windows build through Steam.",
    };
    ui.set_game_missing(game_missing.into());
    ui.set_game_running(core.game_running);
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
    ui.set_status_text(core.status.clone().into());
    ui.set_status_error(core.status_error);
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
                Ok(message) => core.set_status(message, false),
                Err(e) => core.set_status(e.to_string(), true),
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
    lock(shared).set_status(format!("Scanning {name} locally..."), false);
    refresh(ui, shared);
    let (weak, shared, name) = (ui.as_weak(), shared.clone(), name.to_string());
    std::thread::spawn(move || {
        let result = job(&shared);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            match result {
                Ok(report) => {
                    lock(&shared).set_status(format!("{name} scanned locally"), false);
                    then(&ui, &shared, report);
                }
                Err(e) => lock(&shared).set_status(format!("Could not scan {name}: {e}"), true),
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
fn scan_remote(ui: &AppWindow, shared: &Shared, id: String, deps: Vec<String>, title: String) {
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
            let mut remote = match remote {
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
/// the install or update of a verified addon for the platform's statistics.
fn install_remote(
    shared: &Shared,
    id: &str,
    name: &str,
    repo: &str,
    spec: &SourceSpec,
    source: Source,
    remote: Option<RemoteVersion>,
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
    let event = addon.and_then(|addon| telemetry::event(update, &addon, version.as_deref().unwrap_or("unknown"), result.as_ref().map(|_| ())));
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
                    core.set_status(msg, true);
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
                        scan_remote(&ui, &shared, id, deps, asked);
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
                core.set_status(e.to_string(), true);
                None
            }
            Some(Ok(job)) => {
                core.busy.insert(id.clone());
                core.outcomes.remove(&id);
                core.set_status(format!("Installing {}...", job.0), false);
                Some(job)
            }
        }
    };
    refresh(ui, shared);
    let Some((name, repo, spec, source, remote)) = job else { return };
    let remote = pinned.or(remote);
    run_bg(ui.as_weak(), shared.clone(), Some(id.clone()), move |shared| {
        for dep in &deps {
            let job = remote_job(&lock(shared), dep);
            let Some(Ok((dep_name, dep_repo, dep_spec, dep_source, dep_remote))) = job else {
                return Err(Error::NotInstallable(format!("{dep} cannot be installed, so {name} was not installed")));
            };
            install_remote(shared, dep, &dep_name, &dep_repo, &dep_spec, dep_source, dep_remote)
                .map_err(|e| Error::NotInstallable(format!("{dep_name} failed ({e}), so {name} was not installed")))?;
        }
        install_remote(shared, &id, &name, &repo, &spec, source, remote)
    });
}

/// Starts the game in its own folder, once: until the started game closes the button stays off.
/// A thread waits for the game's process, so the launcher learns at once when it ends. The game
/// keeps running when the launcher closes.
fn on_launch_game(ui: &AppWindow, shared: &Shared) {
    {
        let mut core = lock(shared);
        if core.game_running {
            return;
        }
        match config::game_executable(core.addons_dir(), cfg!(windows)) {
            Some(exe) => match std::process::Command::new(&exe).current_dir(exe.parent().unwrap_or(Path::new("."))).spawn() {
                Ok(mut child) => {
                    core.game_running = true;
                    core.set_status("Azeroth Survivors is running.".into(), false);
                    let (weak, shared) = (ui.as_weak(), shared.clone());
                    std::thread::spawn(move || {
                        let _ = child.wait();
                        {
                            let mut core = lock(&shared);
                            core.game_running = false;
                            core.set_status("Azeroth Survivors was closed.".into(), false);
                        }
                        let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &shared));
                    });
                }
                Err(e) => core.set_status(format!("Could not start the game: {e}"), true),
            },
            None => core.set_status("The game was not found next to the AddOns folder.".into(), true),
        }
    }
    refresh(ui, shared);
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
    let native = if cfg!(windows) { game_update::Build::Windows } else { game_update::Build::Linux };
    match game_update::find_exe(dir, !cfg!(windows)) {
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
            core.game_updating = true;
            core.set_status("Updating the game...".into(), false);
        }
        refresh(&ui, &shared);
        run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
            let (client, dir, seen) = {
                let core = lock(shared);
                (core.client(), game_dir(core.addons_dir()), core.game_exe.clone())
            };
            let result = (|| {
                let dir = dir.ok_or_else(|| Error::Io("The game's folder with its Data folder was not found".into()))?;
                let server = WebServer(client);
                // Asked again right before: the server keeps only the newest parts.
                let (exe, build) = look_at_game(&dir, seen.as_ref());
                let plan = game_update::check(&server, &dir, build, exe.as_ref())?;
                game_update::update(&server, &dir, &plan, &mut |step| {
                    let now = !matches!(step, game_update::Step::Downloading { .. });
                    show_progress(shared, now, |core| core.game_step = Some(game_update::step_label(&step)));
                })?;
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
            controller::update_all_ids(&core.statuses).into_iter().filter(|id| !core.busy.contains(id)).collect();
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
        core.set_status(format!("Updating {} addons...", jobs.len()), false);
        jobs
    };
    refresh(ui, shared);
    let weak = ui.as_weak();
    run_bg(ui.as_weak(), shared.clone(), None, move |shared| {
        let total = jobs.len();
        let mut failed = Vec::new();
        for (i, (id, (name, repo, spec, source, remote))) in jobs.into_iter().enumerate() {
            show_progress(shared, true, |core| core.set_status(format!("Updating {} of {total}: {name}", i + 1), false));
            if let Err(e) = install_remote(shared, &id, &name, &repo, &spec, source, remote) {
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
            Ok(format!("Updated {total} addons{skipped_note}"))
        } else {
            Err(Error::Io(format!("{} of {total} updates failed. {}", failed.len(), failed.join(" | "))))
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
        core.set_status("Checking for updates...".into(), false);
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
        let game_versions = if offline { Vec::new() } else { fetch_game_versions(&client, &api) };
        let categories = if offline { Vec::new() } else { client.get_text(&format!("{api}/v1/categories")).map(|t| controller::parse_categories(&t)).unwrap_or_default() };
        show_progress(shared, true, |core| {
            core.self_update = self_update;
            core.game_versions = game_versions;
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

        let updates = controller::update_all_ids(&core.statuses).len();
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
    let (name, folders) = {
        let core = lock(shared);
        if let Some(folder) = id.strip_prefix(UNMANAGED_PREFIX) {
            let name = core.scan.iter().find(|s| s.folder == folder).map(|s| s.toc.title.clone());
            (name.unwrap_or_else(|| folder.to_string()), vec![folder.to_string()])
        } else if let Some(entry) = core.state.get(&id) {
            (entry.name.clone(), entry.folders.clone())
        } else {
            return;
        }
    };
    let text = format!(
        "These folders are moved to the recycle bin: {}.\n\nYour addon settings (SavedVariables in the WTF folder) stay untouched.",
        folders.join(", ")
    );
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
            lock(shared).set_status(message, true);
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

fn on_settings_save(ui: &AppWindow, shared: &Shared) {
    let path = typed_path(&ui.get_settings_path());
    ui.set_settings_path(path.to_string_lossy().into_owned().into());
    let warning = match config::validate_addons_dir(&path) {
        Ok(warning) => warning,
        Err(e) => {
            ui.set_settings_message(e.to_string().into());
            ui.set_settings_message_error(true);
            return;
        }
    };
    let urls = config::platform_url(&ui.get_settings_api_url()).and_then(|api| Ok((api, config::platform_url(&ui.get_settings_web_url())?)));
    let (api_url, web_url) = match urls {
        Ok(urls) => urls,
        Err(e) => {
            ui.set_settings_message(e.into());
            ui.set_settings_message_error(true);
            return;
        }
    };
    let (saved, recheck) = {
        let mut core = lock(shared);
        let game_index = ui.get_settings_game_index();
        let game_version =
            if game_index > 0 { core.game_versions.get(game_index as usize - 1).map(|g| g.name.clone()) } else { None };
        let platform_changed = api_url != core.config.api_url;
        let telemetry = ui.get_settings_telemetry();
        let send_install_id = ui.get_settings_send_install_id();
        let auto_update_all = ui.get_settings_auto_update();
        let config = Config { addons_dir: path, game_version, api_url, web_url, telemetry, send_install_id, auto_update_all, ..core.config.clone() };
        // Saving runs on every focus change: nothing changed, nothing to write. An error from a
        // value typed back to the saved one is over.
        if config == core.config {
            if ui.get_settings_message_error() {
                ui.set_settings_message(warning.map(|w| w.to_string()).unwrap_or_default().into());
                ui.set_settings_message_error(false);
            }
            return;
        }
        // Another platform: its list, versions and categories replace the old ones.
        let recheck = config.addons_dir != core.config.addons_dir || platform_changed;
        let saved = config.save(&core.data_dir);
        if config.addons_dir != core.config.addons_dir {
            core.switch_config(config);
        } else {
            core.config = config;
        }
        (saved, recheck)
    };
    match saved {
        Ok(()) => {
            let note = warning.map(|w| format!(" {w}")).unwrap_or_default();
            ui.set_settings_message(format!("Saved.{note}").into());
            ui.set_settings_message_error(false);
        }
        Err(e) => {
            ui.set_settings_message(format!("Could not save: {e}").into());
            ui.set_settings_message_error(true);
        }
    }
    refresh(ui, shared);
    if recheck {
        check_updates(ui, shared);
    }
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
    let dest = if ui.get_file_dialog() && !ui.get_game_mode() {
        let Some(dest) = rfd::FileDialog::new().add_filter("Zip archive", &["zip"]).set_file_name(name).save_file() else { return };
        dest
    } else {
        match directories::UserDirs::new().and_then(|d| d.download_dir().map(|p| p.join(name))) {
            Some(dest) => dest,
            None => return lock(shared).set_status("No Downloads folder found".into(), true),
        }
    };
    let parts: Vec<(String, String, String)> = {
        let mut core = lock(shared);
        if core.offline {
            return core.set_status(Error::Offline.to_string(), true);
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
fn fetch_game_versions(client: &Client, api: &str) -> Vec<controller::GameVersion> {
    let Ok(text) = client.get_text(&format!("{api}/v1/game-versions")) else { return Vec::new() };
    let list: Vec<serde_json::Value> = serde_json::from_str(&text).unwrap_or_default();
    list.iter()
        .filter_map(|g| Some(controller::GameVersion { name: g["name"].as_str()?.to_string(), sort_key: g["sort_key"].as_i64()? as i32 }))
        .collect()
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
        lock(shared).set_status(message, false);
        return refresh(ui, shared);
    }
    let (client, update, work) = {
        let mut core = lock(shared);
        if !core.busy.is_empty() {
            core.set_status("Wait until the running installs finish, then update SurvivorsForge.".into(), true);
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
            Ok(false) => lock(&shared).set_status("Opened SurvivorsForge in the Microsoft Store, update it there.".into(), false),
            Err(e) => lock(&shared).set_status(format!("Self-update failed: {e}"), true),
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
    let data_dir = config::data_dir();
    let config = Config::load(&data_dir);
    let needs_setup = config.is_none();
    let shared: Shared = Arc::new(Mutex::new(Core::new(data_dir, config)));

    let ui = AppWindow::new()?;
    {
        let (weak, s) = (ui.as_weak(), shared.clone());
        // ponytail: the closure holds the Arc of the Core it sits in; both live as long as the app.
        lock(&shared).repaint = Some(Arc::new(move || {
            let s = s.clone();
            let _ = weak.upgrade_in_event_loop(move |ui| refresh(&ui, &s));
        }));
    }
    ui.set_app_version(APP_VERSION.into());
    ui.set_self_update_in_place(self_update::can_replace_self());

    let game_mode = config::is_game_mode(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var_os("GAMESCOPE_WAYLAND_DISPLAY").is_some(),
    );
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
    on!(on_detail_link, |ui, shared, page| on_detail_link(&ui, shared, &page));
    on!(on_comments_show, |ui, shared| on_comments_show(&ui, shared));
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
    on!(on_category_changed, |ui, shared, index| {
        {
            let mut core = lock(shared);
            let choices = controller::category_choices(&core.categories, &core.registry);
            core.category = usize::try_from(index - 1).ok().and_then(|i| choices.get(i)).map(|c| c.name.clone());
        }
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
            lock(shared).set_status(format!("Open {link} in your browser"), false);
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
        let _ = slint::quit_event_loop();
    });

    refresh(&ui, &shared);
    if !needs_setup {
        check_updates(&ui, &shared);
        fetch_icons(&ui, &shared);
    }
    let (weak, linked) = (ui.as_weak(), shared.clone());
    deep_link::listen(move |id| {
        let shared = linked.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| open_link(&ui, &shared, id));
    });
    if let Some(id) = link {
        open_link(&ui, &shared, id);
    }
    ui.run()
}

/// One comment for the comments page; its avatar shows once it is loaded.
fn comment_row(core: &Core, c: controller::Comment) -> CommentRow {
    let avatar = (!c.avatar_url.is_empty()).then_some(c.avatar_url.as_str());
    CommentRow {
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
/// does not hold up the window; the status line says it is coming, and where to go if it fails.
fn on_detail_link(ui: &AppWindow, shared: &Shared, page: &str) {
    let url = {
        let core = lock(shared);
        let Some(entry) = core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)) else { return };
        if page == "bug" {
            let issues_on = core.platform.info.get(&entry.id).is_some_and(|i| i.issues_on);
            controller::bug_report(&core.web_url(), &entry.id, &entry.repo, issues_on, core.platform.issues_ready.get(&entry.id).copied()).1
        } else {
            let Some(url) = controller::addon_link(&core.web_url(), &entry.id, page) else { return };
            url
        }
    };
    const OPENING: &str = "Opening the browser";
    lock(shared).set_status(OPENING.into(), false);
    refresh(ui, shared);
    let (weak, shared) = (ui.as_weak(), shared.clone());
    std::thread::spawn(move || {
        let failed = open::that(&url).is_err();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            let mut core = lock(&shared);
            if failed {
                core.set_status(format!("Could not open the browser. The page is {url}"), true);
            } else if core.status == OPENING {
                core.set_status(String::new(), false);
            }
            drop(core);
            refresh(&ui, &shared);
        });
    });
}

/// Opens the comments page of the detail view's addon and loads its newest comments.
fn on_comments_show(ui: &AppWindow, shared: &Shared) {
    let slug = {
        let core = lock(shared);
        core.detail.as_ref().and_then(|(id, _)| registry_entry(&core, id)).map(|a| a.id.clone())
    };
    let Some(slug) = slug else { return };
    lock(shared).comments = Some(Comments { slug, loaded: Vec::new(), older: None, asked: None, failed: false, off: false, request: 0 });
    ui.set_comments_open(true);
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

/// Shows the addon of a website link in Browse, in front of the other windows.
fn open_link(ui: &AppWindow, shared: &Shared, id: String) {
    if ui.get_needs_setup() {
        return;
    }
    if lock(shared).registry.get(&id).is_none() {
        lock(shared).set_status(format!("The addon \"{id}\" from the link is not in the list of verified addons."), true);
        refresh(ui, shared);
        return;
    }
    ui.set_page(1);
    on_row_details(ui, shared, id);
    ui.window().set_minimized(false);
    ui.window().with_winit_window(|w| w.focus_window());
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
