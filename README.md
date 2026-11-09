# SurvivorsForge

An addon manager for [Azeroth Survivors](https://wow.aethus.pl), a private World of Warcraft 1.12.1 instance, in the spirit of CurseForge but scoped to this one game. Creators publish their addons from GitHub, every release is scanned for harmful code and reviewed by admins, and players install, update and remove them.

SurvivorsForge has these parts, all in this repository:

| Part | What | Where |
|---|---|---|
| Launcher | Desktop app for Windows, Linux, Steam Deck and macOS: installs, updates and removes addons in the game's `AddOns` folder, installs dependencies with them and starts the game | `src/`, `ui/`, download from the [releases](https://github.com/Rezondes/SurvivorsForge/releases/latest) |
| Website | Browse addons, publish and edit them as a creator, review as an admin, build addon packs for the browser version, public statistics | `crates/forge-web`, <https://forge.rezondes.net> |
| API and worker | Serves the launcher and the website, syncs releases from GitHub, scans them, checks that scanned files stay unchanged; a public part for other launchers | `crates/forge-api`, <https://forgeapi.rezondes.net>, [docs/public-api.md](docs/public-api.md) |
| Scanner | Static checks of the Lua, XML and zip contents of a release, also as the `forge-scan` command | `crates/forge-scan` |
| Homepage | Static page with the downloads, published with GitHub Pages | branch `gh-pages`, <https://rezondes.github.io/SurvivorsForge/> |

The launcher installs addons from three sources:

- **Platform addons** from the addon list of the platform (`/v1/registry`), so the list changes without a new release.
- **GitHub repos** added by hand. SurvivorsForge detects whether a repo is an addon and tracks it for updates.
- **Local zip files**, installed or updated by hand.

Built with Rust: the launcher with [Slint](https://slint.dev), the platform with axum, Postgres and htmx. Licensed under AGPL-3.0-or-later. Security reports: [SECURITY.md](SECURITY.md).

> **Linux and Steam Deck support of the launcher is experimental.**

## Requirements

The sections up to "Window frame" are about the launcher. For the platform see [Platform](#platform).

- Rust (stable, 2024 edition). Developed against 1.98.
- Windows 10/11, or Linux x86_64 with glibc 2.35 or newer (SteamOS, Ubuntu 22.04+, Arch, Fedora, ...).
- Linux build dependencies (Ubuntu names): `libfontconfig1-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev libegl1-mesa-dev`. At runtime Linux needs at least one installed system font: Slint panics with `NoMatch` without any.

## Build and run

```bash
cargo run
```

```bash
cargo build --release
```

The release binary is `target/release/SurvivorsForge.exe` (Linux: `target/release/SurvivorsForge`). On Windows it is a GUI app and opens no console window.

On first start the app asks for the `AddOns` folder, e.g. `<game>\benilla-config\AddOns`. It also searches the home, Games, Downloads and Desktop folders, removable media (Linux) and drive roots (Windows) up to three levels deep and offers what it finds.

A debug build (`cargo run`) talks to a local platform (`http://localhost:3081`, website `:3080`) and does not register the `survivorsforge://` links. Against the official one: `SURVIVORSFORGE_API_URL=https://forgeapi.rezondes.net SURVIVORSFORGE_WEB_URL=https://forge.rezondes.net cargo run`, or set both in *Settings*.

### macOS app

Build a local app for Apple Silicon with bundled Wine. It requires macOS 26.2 or
later and Rosetta 2. See the [macOS build and setup instructions](packaging/macos/README.md).

### Linux AppImage

```bash
packaging/linux/build-appimage.sh target/release/SurvivorsForge <version> dist
```

The AppImage holds only the binary, icon and `.desktop` file: the binary links nothing but glibc and fontconfig/freetype, and winit loads the window and GL libraries at runtime. On Wayland the taskbar icon comes from the `.desktop` file matching the app id `survivorsforge`.

File dialogs go through xdg-desktop-portal. Where none runs, "Browse..." says so and the path can be typed in; "Install zip" is then unavailable.

## Tests

```bash
cargo test
```

```bash
cargo clippy --all-targets -- -D warnings
```

Both cover only the launcher. Its layout tests (`layout_tests` in `src/main.rs`, e.g. that a long description never runs into the facts below it) find elements by name, which Slint only keeps in builds with `SLINT_EMIT_DEBUG_INFO=1`; without it they skip, CI sets it. The platform crates are tested with `--workspace`; their API and website tests need a Postgres in `DATABASE_URL` (see [docs/local-setup.md](docs/local-setup.md)).

Tests run offline. Tests that need the network are marked `#[ignore]`:

```bash
cargo test -- --ignored
```

To try the app against a copy of an AddOns folder without touching your real settings, point the data folder somewhere else:

```bash
SURVIVORSFORGE_DATA_DIR=/path/to/test-data cargo run
```

### Testing on Linux with Docker

`docker/` holds two images for the launcher (the `forge-*.Dockerfile` files build the platform). With Docker Desktop on WSL2, their windows appear on the Windows desktop through WSLg.

| Image | Purpose |
|---|---|
| `docker/linux-dev.Dockerfile` | Ubuntu 22.04 with Rust: build, test, package the AppImage (same base as the release build) |
| `docker/arch-run.Dockerfile` | Arch Linux (base of SteamOS) with only desktop runtime libraries: runs the AppImage |

```bash
docker build -t survivorsforge-dev -f docker/linux-dev.Dockerfile docker
```

```bash
docker run --rm -v "$PWD:/src" -v survivorsforge-target:/target survivorsforge-dev cargo test
```

`docker/run-gui.ps1 -AddOns <copy of an AddOns folder>` starts the app from the container with the WSLg mounts. Never mount the real game folder: the container installs and removes addons in it.

To approximate a Steam Deck in Game Mode, set `XDG_CURRENT_DESKTOP=gamescope` (fullscreen, no window frame) and `SLINT_SCALE_FACTOR` so the screen comes out near 1280x800. Game Mode itself, Steam Input and the on-screen keyboard need a real Deck.

## Project layout

| Path | Purpose |
|---|---|
| `src/main.rs` | UI glue: callbacks, background threads, window frame |
| `src/lib.rs` | Shared `Error` type, constants (`APP_REPO`, `APP_VERSION`), small helpers |
| `src/config.rs` | `config.json`, data folder, AddOns folder validation and search |
| `src/scan.rs` | Scans the AddOns folder for installed addons |
| `src/state.rs` | `state.json`: which addon came from which source in which version |
| `src/install.rs` | Atomic install/update, uninstall to the recycle bin |
| `src/registry.rs` | Addon list: load order, cache, platform addresses |
| `src/github.rs` | The `GitHub` trait: metadata through the platform API (`ApiGitHub`) or straight from github.com (`WebGitHub`), zip downloads from GitHub |
| `src/updates.rs` | Resolves the remote version per source, decides "update available" |
| `src/repo_detect.rs` | Detection for manually added repos |
| `src/controller.rs` | Pure view logic: rows, badges, filters, compatibility warnings, zip install plan, platform cache |
| `src/pictures.rs` | Icons and screenshots from the platform: decoded with limits, scaled, cached by hash |
| `src/deep_link.rs` | `survivorsforge://addon/<id>` links from the website |
| `src/telemetry.rs` | Anonymous install and update events (see Statistics) |
| `src/self_update.rs` | Updates SurvivorsForge itself from its GitHub releases |
| `ui/*.slint` | Theme tokens, components (title bar, rows, tiles, dialogs), main window |
| `assets/icon.png`, `assets/icon.ico` | App icon: window and taskbar (PNG via Slint), `.exe` icon (ICO via `winresource` in `build.rs`) |
| `assets/survivorsforge.desktop` | Linux desktop entry: name, icon and app id for the AppImage |
| `packaging/linux/` | `AppRun` and `build-appimage.sh` for the Linux AppImage |
| `packaging/windows/` | `AppxManifest.xml` and tile icons for the Microsoft Store package |
| `docker/` | Linux build and runtime containers for testing the launcher on Windows (see Tests), the platform images (`forge-api`, `forge-web`, `forge-grafana`), the monitoring configuration |
| `ui/fonts/` | Morpheus (headings, shareware by Kiwi Media, `README-morpheus.txt`) and Marcellus (text, SIL Open Font License, `OFL-marcellus.txt`), embedded at build time |
| `registry/verified.json` | Built-in copy of the verified addons (see Verified addon list) |
| `tests/` | Integration tests for installation, GitHub fixtures |
| `crates/slint-toast/` | Toasts for any Rust + Slint app: `Toasts` (info, success, warning, error, busy that turns into its result), `ui/toast.slint` with its own style, `sync` into a `VecModel`; how to use it in `src/lib.rs` |
| `crates/forge-core/` | Shared with the platform: `.toc` parser, zip analysis and safe extraction, registry types and validation, asset glob matching, addon packs, dependency order, game version compatibility, trust levels |
| `crates/` | The platform (see Platform) |
| `compose.yaml` (server), `compose.local.yaml` (whole platform locally), `compose.dev.yaml` (Postgres for `cargo test`), `.env.example`, `.env.local.example` | Running the platform (see [docs/self-hosting.md](docs/self-hosting.md), [docs/local-setup.md](docs/local-setup.md)) |
| `scripts/compose-smoke.sh` | Smoke test of the API stack in CI |
| `docs/` | Local setup, self-hosting, public API, guide for addon creators |

## How it works

### Addon detection

WoW 1.12 loads a folder `<Name>/` that holds `<Name>.toc`. The scanner and the zip analysis follow that rule:

- A zip folder `D` containing `D/D.toc` is an addon named `D`.
- A `.toc` in a folder with another name (GitHub source zips such as `owner-repo-<sha>/BordoUI.toc`) makes that folder an addon named after the `.toc` stem.
- If one folder holds several `.toc` files (`pfUI.toc`, `pfUI-tbc.toc`), the one with `## Interface: 11200` wins, then the shortest name.
- `.toc` files nested inside an addon (libraries) don't count as separate addons.
- Backslash paths in zips are normalized. Entries with `..`, absolute paths or drive letters abort the install (zip slip protection).
- Files directly in the AddOns folder (e.g. `account.txt`) are never read or changed.

### Sources and versions

Each managed addon has a source in `state.json`:

| Source | Update check |
|---|---|
| `verified` | Repo and source spec from the platform's addon list |
| `github` | Repo and source spec stored when the repo was added |
| `zip` | None (manual) |

A source spec says where updates come from:

| Spec | Remote version | Download |
|---|---|---|
| `release` | Tag of the release GitHub marks as "latest"; without one, the newest pre-release with a matching asset | The asset that matches the `asset` glob (`*` wildcard) |
| `tag` | Highest tag by the numbers in its name (`v2.8.10` before `v2.8.9`) | Source zip of that tag |
| `branch` | Newest commit SHA of the default branch | Source zip of that commit |

Tags across the repos are not uniform (`v3.0.36`, `v2.6.5-azs`, `build-<sha>`). An update is available when the remote id differs from the installed one; there is no semver comparison.

For platform addons the platform already knows the newest version it scanned, so the update check asks GitHub nothing. Such a download is checked against the SHA-256 the platform scanned (`HashMismatch` installs nothing). Each version has a trust level:

| Trust | Meaning | Launcher |
|---|---|---|
| `verified` | An admin approved exactly this file | Installs |
| `scanned` | The scan found nothing, no admin looked at it | Installs |
| `flagged` | The scan found something | Asks first |
| `unscanned` | Not scanned yet, or the scan failed | Asks first |
| `source_changed` | The file on GitHub differs from the scanned one | Refuses |
| `blocked` | An admin blocked the version or the addon | Refuses |

The launcher also asks first when a version has no checked file (no SHA-256) or does not support the chosen game version. "Update all" skips the updates that ask first; they install one by one. After "Update all" and after updates that installed on their own, a dialog lists each addon: from which version to which with a link to its release notes, or why it did not update with what helps (*Retry*, *Sign in* when GitHub limits requests, *Open folder* when the folder cannot be written, *Details* when the platform holds the version back, *Review* for an update that asks first). *Retry failed* tries all failures again.

While the launcher is open it looks for updates again on its own, every 30 minutes by default (*Settings > Look for updates while the launcher is open*, 15 to 60 minutes in steps of 5). That check keeps the rows as they are and only shows a toast when it found updates the last check did not know; it waits while installs run or the platform does not answer. Auto-update installs checked updates on its own after each update check, for every addon (*Settings > Update all addons automatically*) or for single platform addons (the box in their details). It only takes versions the platform scanned or verified and found nothing in; a flagged version waits for the user, and a later version that is not flagged installs again.

Required dependencies of a platform addon are installed before it, in dependency order; a failing dependency stops before the addon. Optional ones that are not installed are named in the confirmation.

Addons found on disk that match an entry of the addon list are adopted on startup with version `toc:<Version>`. The first update check replaces that with the release tag when both match (ignoring a leading `v`).

*Settings, Game version* sets the game version the user plays. Addons whose newest version names a game range without it get a compatibility warning.

### Installation

1. The zip is extracted to `AddOns/.survivorsforge/staging/<run>/`, a unique name per install. That folder is always on the same file system as the addons, even when AddOns is a mount or a link to another drive, so `rename` is atomic. The game ignores it, since it holds no matching `.toc`.
2. Existing folders of the addon move to `.survivorsforge/backup/<run>/`.
3. The new folders move into AddOns.
4. On any error, everything is moved back. On success, backup and staging are deleted.

An install never overwrites folders that belong to another managed addon (`FolderOwnedBy`). The power button on a row or tile disables an addon without removing it: its folders move to `AddOns.disabled` beside the AddOns folder, where the game does not load them even when it is started without the launcher, and back when it is enabled again. Disabled addons stay in *Installed* (their status starts with *Disabled*) and update where they wait, unless *Settings > Update disabled addons too* is off: then their update waits until they are enabled. Disabling one that installed addons need asks first; enabling one also enables the disabled addons it needs. Both wait while the game runs, and nothing is moved when a folder of the same name is already on the other side. Uninstalling moves folders to the recycle bin. They first wait in `AddOns/.survivorsforge/removed/` while the toast offers *Undo* (10 seconds); Undo puts the folders and the addon's entry back, unless a folder of the same name was installed meanwhile. Once the toast is gone, or when the launcher closes or starts again after a crash, they go to the recycle bin. The addon settings (`saved/` and `saved-variables.lua` in the game's config folder, the parent of the AddOns folder) are never touched by an install or removal. Before an update or a removal they are backed up as `backups/saved-<date>-<time>.zip` there, at most once a minute, so a run of *Update all* makes one backup; if that backup fails, nothing changes. The newest 10 stay (*Settings > Backups*, 3 to 50), and each one can be restored there; a restore backs up the current settings first and waits until the game is closed.

### Adding a GitHub repo

Accepts `owner/repo` or a GitHub URL. Detection order:

1. Each `.zip` asset of the latest release is downloaded and analyzed; the first one with an addon wins (installer zips without an addon are skipped). The asset name becomes a glob by replacing the version with `*` (`LuckyOdds-v0.6.0.zip` becomes `LuckyOdds-v*.zip`).
2. Otherwise the source zip of the newest tag.
3. Otherwise the source zip of the default branch.

Repos that are already on the platform's addon list are refused with a pointer to Browse.

### GitHub access

The launcher asks the platform API for release and tag metadata (`/v1/github/{owner}/{repo}/releases/latest`, `/releases/{tag}`, `/refs`). The API reads GitHub and caches the answers for 10 minutes. Only the zip files come from GitHub directly, and only from GitHub's download hosts (`github.com`, `codeload.github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`).

When the API cannot be reached, the launcher (from 0.5.1 on) reads the same github.com pages itself (`WebGitHub`), for addon updates and its own new version alike. Launchers up to 0.5.0 have no such fallback: without the server they do not learn about a new release and must be updated by hand once.

Neither uses the GitHub REST API for this. Without a token it allows 60 requests per hour. Everything comes from plain github.com endpoints instead, which have no such budget:

| Need | Endpoint |
|---|---|
| Latest release | `https://github.com/{repo}/releases/latest`, read the `Location` of the redirect (`.../releases/tag/{tag}`; no release redirects to `.../releases`) |
| Release assets | `https://github.com/{repo}/releases/expanded_assets/{tag}`, the `href="/{repo}/releases/download/{tag}/{name}"` links in that HTML fragment |
| Tags, default branch, commit SHAs | `https://github.com/{repo}.git/info/refs?service=git-upload-pack`, the ref list `git ls-remote` reads (`symref=HEAD:refs/heads/...` names the default branch) |
| Downloads | Asset URLs and `https://codeload.github.com/{repo}/zip/{ref}` |

github.com answers `401` for a repo that does not exist (it asks for a login), which maps to "Not found on GitHub".

The asset list is read from HTML (parsers in `crates/forge-core/src/github_web.rs`). If GitHub changes that fragment, `parses_release_assets` in `src/github.rs` fails against a fresh copy of `tests/fixtures/github/assets_survivor_ui.html`, and the app reports "no matching addon zip". The network test checks the whole chain against a running API:

```bash
SURVIVORSFORGE_API_URL=http://127.0.0.1:8080 cargo test --lib -- --ignored api_live
```

## Verified addon list

The addon list lives in the platform's database and comes from `GET /v1/registry`: every addon that is neither deleted nor blocked. `registry/verified.json` is no longer the source of the list. It is an export of the addons whose author had a verified version (`forge-api export-verified`), and it is still read in four places:

- Compiled into the launcher, as the list for a first start without the platform.
- Launchers up to 0.3.2 download it from `main` on raw.githubusercontent.com instead of asking the platform.
- `forge-api import-verified` seeds a new platform with it (see [docs/self-hosting.md](docs/self-hosting.md)).
- The API tests in `crates/forge-api/tests/registry.rs` use it as test data.

Schema version 1 (`/v1/registry` adds platform fields such as the scanned version, its SHA-256 and trust; launchers that read only schema 1 ignore them):

```json
{
  "schema": 1,
  "addons": [
    {
      "id": "survivor-ui",
      "name": "Survivor UI",
      "author": "morderous",
      "repo": "morderous/survivor-ui",
      "description": "Flat, dark interface in the style of EllesmereUI and Plater: Survivor UI plus Survivor Plates (nameplates).",
      "category": "Unit Frames",
      "folders": ["SurvivorUI", "SurvivorPlates"],
      "source": { "type": "release", "asset": "SurvivorUI-v*.zip" }
    }
  ]
}
```

| Field | Meaning |
|---|---|
| `id` | Stable key, stored in `state.json`. Never change it for an existing addon. |
| `category` | A platform category without subcategories: Warrior to Druid (under Classes), Combat, Libraries, Unit Frames, Action Bars or Miscellaneous. Admins change the list on the website. |
| `folders` | Every folder the addon installs. Used for adoption and conflict checks. |
| `source.type` | `release` (needs `asset`), `tag` or `branch` |

Validation rejects unknown schema versions, duplicate ids and folders claimed by two addons.

Load order at runtime:

1. The platform API (`/v1/registry`).
2. The last valid copy in the cache.
3. The copy compiled into the binary.

Without the API the launcher is offline: it shows the cached list with its age and checks every addon on github.com directly. An update to the version the platform scanned last keeps that scan; a newer one is unchecked and asks before it installs. Addon packs, categories and game versions need the API.

`SURVIVORSFORGE_API_URL` and `SURVIVORSFORGE_WEB_URL` choose another platform: at run time first, then *Settings, Platform API / Platform website* (`api_url`, `web_url` in `config.json`), then the value compiled in by the release build. Debug builds fall back to a local platform.

## Platform

| Crate | What |
|---|---|
| `crates/forge-api` | The API (`forge-api serve`), the job worker (`forge-api worker`), and `migrate`, `rescan --all`, `import-verified`, `export-verified` |
| `crates/forge-web` | The website (askama templates, htmx); talks only to the API |
| `crates/forge-scan` | The scanner as a library and as `forge-scan <zip> [--folders A,B] [--foreign C,D]` (JSON report, exit code 0 clean, 1 flagged, 2 error) |
| `crates/forge-core` | Shared with the launcher |
| `crates/forge-trace` | Logs, and OpenTelemetry traces to Tempo when `OTEL_EXPORTER_OTLP_ENDPOINT` is set |

Creators log in with GitHub through a GitHub App and publish addons from their own repos: description in Markdown, screenshots, icon, category, game versions per release, team members and dependencies. Webhooks and a poll every 15 minutes bring new releases in; the worker scans each one. Admins review flagged versions, verify or block them, handle reports and ban users. A recurring check makes sure each scanned file is still the same on GitHub. Downloads stay on GitHub.

Each addon page has an **Issues** tab: the GitHub issues of its repo, read live from GitHub, so issues opened there show too. Logged-in players open bug reports and feature requests, reply, edit and close their own; everything goes to GitHub as their own account (the label `bug`, `enhancement` or `question` is set by the App). The platform stores none of it: the website keeps the player's GitHub token from the login in its encrypted cookie and sends it with each write (`X-GitHub-Token`); the API checks that it belongs to the logged-in user. Writing needs the App installed with `Issues: Read & write`, so creators switch it on themselves; otherwise the tab links to GitHub's own form. "Report a bug" on the addon page (`/addons/<slug>/issues/new`, the link for the launcher too) opens the form here or, without the App, GitHub's form. SurvivorsForge itself works the same way at `/issues` (linked in the footer) with the issues of `LAUNCHER_REPO`, through `/v1/platform/issues`. A ban stops writing through SurvivorsForge only.

Players of the browser version of the game upload addons as one zip. An addon pack bundles at least two addons into that zip, every addon in its own folder, on the website (`/packs/new`) or in the launcher (*Browse*, tick the addons, *Save as zip* or *Share as link*). A shared pack link shows what is inside and gives the same zip.

`compose.yaml` runs Postgres, API, worker, website and Caddy; the `monitoring` profile adds Prometheus, Loki, Tempo, Alloy, cAdvisor, node-exporter, a Docker socket proxy and Grafana (image built from `docker/forge-grafana.Dockerfile`). How to run it: [docs/local-setup.md](docs/local-setup.md) on your own machine, [docs/self-hosting.md](docs/self-hosting.md) on a server. Other launchers may read the addon index without a key: [docs/public-api.md](docs/public-api.md). Addon creators new to GitHub: [docs/creator-guide.md](docs/creator-guide.md), from the repo to one-button releases. The game's own addon API, as its author published it on Discord, is in [docs/addon-api.md](docs/addon-api.md). The website shows the three files as pages (`/docs/api-docs`, `/docs/creator-guide`, `/docs/addon-api`, listed on `/docs`), compiled in with `include_str!`.

## Launcher pages

*Installed*, *Browse* and *Settings*, plus *Quit* in Game Mode. Installed and Browse each show their addons as rows or tiles, chosen on the page and kept in `config.json` (`tile_pages`); rows are the default. The window opens where it was closed, at the same size and on the same page (`window`, `last_page` in `config.json`); a window left on a screen that is gone opens in the middle of the main screen. Game Mode keeps nothing of this. *Settings > After launching the game* lets the launcher stay open (the default), minimize or close once the game it started runs; Game Mode always stays open. The toolbar buttons (tiles or rows, *Check for updates*, *Install zip*, *Add GitHub repo*, *Update all* with the number of updates) are icons that name themselves in a tooltip on hover. Keys: *Ctrl+1*, *Ctrl+2*, *Ctrl+3* (or *Page Up*/*Page Down*) open the pages, *Ctrl+F* goes to the search and *Escape* clears it, *F5* checks for updates, *Ctrl+U* updates all. *Tab* reaches each addon, *Enter* opens its details and *Delete* removes it after asking.

Running work and how it ended show as toasts at the bottom right: *Checking for updates...* runs as a busy toast with a moving bar and no X, so it cannot be closed, and turns into its result (*All addons are up to date*). Info and success toasts go after 4 seconds, warnings and errors stay until closed with their X, at most three at a time. The line at the bottom shows the AddOns folder.

- **Installed**: every addon in the AddOns folder with a badge (`Verified`, `GitHub`, `Zip`, or `Unmanaged` for folders SurvivorsForge did not install), *Check for updates*, *Update all*, *Install zip*, *Add GitHub repo*, update and remove per addon.
- **Browse**: the platform's addons. Browse and Installed filter and sort as the website does: search, category, safety check (*Verified*, *Checked*, *Flagged*, *Not checked*; GitHub and zip addons have none, so a chosen one hides them) and *Most downloads* (the default), *Recently updated* or *Name*. An installed addon has the same card as in Installed, with its status, *Update* and *Remove*. Ticking addons makes an addon pack (*Save as zip*, *Share as link*, *Clear*).
- **Details** of an addon: description and icon buttons named by their tooltips: *Open on the website*, *GitHub*, *Scan locally*, *Remove*, then the bug and abuse buttons. As on the website the details have tabs: *Overview*, *Screenshots* (when there are any), *Issues* (the open issues, each opening on the website), *Comments* (when the creator turned them on) and *Versions* (every version with its date, downloads and trust). Installs and updates take the newest version; *Install* on an older one in *Versions* goes back to it, for an addon that broke. That addon is then held (`held` in `config.json`): it does not update on its own until the newest is installed again by hand. As on the website, *Report a bug* opens the platform's form once the creator turned the Issues tab on and the App may write issues, else *Report on GitHub*; *Issues* and *Report abuse* open the website. The *Comments* tab shows the addon's comments to read; writing and replying need the website's GitHub login. Pages that wait on the platform show placeholders after 150 ms, say after 5 s that the connection is slow and give up after 15 s with *Try again*.
- **Settings**: AddOns folder, game version, platform API and website address, the two statistics switches, the launcher version. There is no Save button: switches and the game version save at once, a text field on Enter, when it loses focus and when the page or the launcher is left.
- **Launch game** starts `AzerothSurvivors.exe` next to `benilla-config`, using bundled Wine on macOS. On Linux it starts a native build whose name starts with `AzerothSurvivors`. Use Steam to start a Windows build under Proton.
- **Game updates**: with each update check the launcher compares the game with the indexes the game updates itself by: `https://wow.aethus.pl/exe/latest.json` for the Windows exe (also under Proton), `exe/linux/latest.json` for the native Linux build `AzerothSurvivors`, `exe/data/patches.json` for the files in `Data`. The exe's SHA-256 is read once per size and time. When something differs, the launch button becomes *Update to v…* (*Start without updating* below it): the parts are joined into one gzip, unpacked, checked against size and SHA-256 and only then put in place, with the old file's permissions (`src/game_update.rs`).

On Windows and Linux, the website opens an addon in the launcher with a `survivorsforge://addon/<id>` link. A release build registers the scheme for the current user on every start, so it follows the executable when it moves (Windows: `HKCU\Software\Classes`, Linux: a `.desktop` file and `xdg-mime`). A launcher that already runs gets the link from the new one over `127.0.0.1:47615`, and the new one quits. On macOS, find and install addons from **Browse** in the app.

## Data locations

| What | Where |
|---|---|
| Settings (`config.json`) | `%APPDATA%\SurvivorsForge\` (Linux: `~/.config/SurvivorsForge/`) |
| Install state (`state.json`) | Same folder, one section per AddOns folder |
| Addon list cache | `cache/verified.json` in the same folder |
| Icon URLs and addon detail pages | `cache/platform.json` |
| Icons and screenshots | `cache/images/<sha256>.webp` |
| Staging, backup, downloads | `AddOns\.survivorsforge\` |

`SURVIVORSFORGE_DATA_DIR` overrides the settings folder. A damaged `state.json` is moved to `state.json.bak` and the app starts with an empty state.

## Statistics

After each install or update of a platform addon the launcher sends one anonymous event to the
platform (`POST /v1/telemetry/events`): the kind (`install`, `update`, or `install_failed` /
`update_failed` with a fixed error code such as `hash_mismatch`), the addon's registry ID and
version, the launcher version and the operating system. No paths, names or error texts leave
the computer. The platform shows the totals on its `/stats` page.

With it comes a random installation ID (`install_id` in `config.json`), made on the first start.
It is sent as `X-SurvivorsForge-Install` with requests to the platform API, never to GitHub, and
helps to find the requests of one installation when something goes wrong. *Settings, Statistics*
turns off the events (and with them the ID) or only the ID.

Every request names the launcher version and operating system in its User-Agent
(`SurvivorsForge/0.6.4 (windows)`). A request with an installation ID marks that launcher active for the
hour (`launcher_active`, forgotten with the IDs after 90 days); Grafana's *Launcher and downloads* shows
the active launchers of this hour and of today (Europe/Berlin) by platform and version.

## Releases and self-update

`.github/workflows/ci.yml` runs on every push to `main` and every pull request: launcher tests and Clippy on Windows, Ubuntu 22.04, and macOS, the platform tests against Postgres, the compose files and images, and a gitleaks secret scan.

`.github/workflows/release.yml` runs on tags `v*` and on its *Run workflow* button:

1. The button only: sets the typed version in `Cargo.toml` and `Cargo.lock`, commits `chore(release): bump version to <version>`, tags `v<version>` and pushes both. The jobs below build from that tag in the same run.
2. Fails if the tag does not match the version in `Cargo.toml` and `Cargo.lock`.
3. Builds the launcher for Windows, Linux and Apple Silicon macOS in parallel.
4. Pushes the platform images (`survivorsforge-api`, `-web`, `-grafana`) to ghcr.io once the Windows and Linux builds succeed.
5. Publishes the GitHub release with `compose.yaml`, `env.example`, and the Windows and Linux launchers under fixed names:
   - `SurvivorsForge-windows-x86_64.zip`
   - `SurvivorsForge-linux-x86_64.AppImage`
6. Attaches `SurvivorsForge-macos-arm64.zip` to the same release when the Mac build succeeds. A failed Mac build or upload can be retried independently.

The self-update looks for these names, and they give stable links (`releases/latest/download/<name>`). Launchers before 0.4.0 looked for versioned names (`SurvivorsForge-<version>-...`), which releases after 0.6.0 no longer carry: they must be downloaded again by hand.

A tag with a hyphen, e.g. `v0.4.0-rc.1` (with `version = "0.4.0-rc.1"` in `Cargo.toml`), makes a
pre-release for testing: installed launchers do not update to it, and the images get only the tag
`0.4.0-rc.1`, not `latest`. Run it on a server with `FORGE_VERSION=0.4.0-rc.1`. The final `v0.4.0`
later updates everyone, the testers included.

To cut a release: on GitHub, *Actions → Release → Run workflow*, type the version (`0.5.2`, or `0.6.0-rc.1` for a pre-release) and run it. Afterwards pull the bump commit. By hand instead: bump `version` in `Cargo.toml`, build once so `Cargo.lock` follows, commit, then tag and push:

```bash
git tag v0.2.0
```

```bash
git push origin v0.2.0
```

On startup the app checks the latest SurvivorsForge release. If its version is higher (semver), a banner offers "Update and restart":

- Windows downloads the zip, writes the new exe next to the running one (`SurvivorsForge.exe.new`), moves the running one aside (`.old`) and the new one into its place, then starts it. Each rename is tried again for about ten seconds while a virus scanner holds the fresh file; if the new exe cannot take the name, the old one moves back. The next start removes `.old`, `.new` and the leftovers of the self-update before 0.5.2 (`.SurvivorsForge.<random>.__relocated__.exe`, `__temp__.exe`).
- Linux downloads the new AppImage next to the running one (`$APPIMAGE`), makes it executable and renames it over the old file. A bare binary outside an AppImage can't replace itself; its banner opens the download page instead.

### Microsoft Store

The release workflow's `store` job packs the Windows exe into `SurvivorsForge.msix` and submits it to the Microsoft Store, which signs it. A Store install shows no SmartScreen warning. Pre-releases skip it, and it runs after the GitHub release, so a Store failure never holds that back (re-run the job alone).

The Store version notices it runs as a package (`self_update::from_store`): it asks the Store for its update instead of GitHub, has the Store install it in the background and restarts (Windows before 2004 opens the Store page instead), and it leaves the `survivorsforge://` link to its manifest instead of writing it to the registry. Its settings in `%APPDATA%\SurvivorsForge` are kept apart from a zip install's by Windows.

Setup, once (Settings, Secrets and variables, Actions):

1. In Partner Center, reserve the name and open *Product identity*. Set these repository variables from it: `MSSTORE_IDENTITY_NAME` (Package/Identity/Name), `MSSTORE_PUBLISHER` (Package/Identity/Publisher, `CN=...`), `MSSTORE_PUBLISHER_DISPLAY_NAME`, and `MSSTORE_PRODUCT_ID` (Store ID, `9N...`). From now on every final release packs the MSIX and keeps it as the `msix` artifact of the run.
2. Make the first submission by hand in Partner Center with that artifact: listing, screenshots, age rating, privacy policy URL (the launcher sends anonymous statistics). The `runFullTrust` capability needs a short reason, for example "Manages addon files in the game folder".
3. For automatic submissions, register an app in Microsoft Entra ID and add it in Partner Center under *Account settings, User management, Microsoft Entra applications* as Manager. Set the variable `MSSTORE_TENANT_ID` and the secrets `MSSTORE_SELLER_ID`, `MSSTORE_CLIENT_ID` and `MSSTORE_CLIENT_SECRET`. Without `MSSTORE_TENANT_ID` the job only packs.

A submission fails while the previous one is still in certification; re-run the job once it is through.

## Steam Deck

Experimental and not yet tested on a real Deck.

Azeroth Survivors is not on Steam. The game creates its `benilla-config/AddOns` folder on first start, so start the game once before setting up SurvivorsForge. The folder is expected next to the game's executable wherever it was unpacked, whether the Linux build runs natively or the Windows build through Proton.

1. In Desktop Mode, download the AppImage and make it executable (file properties, or `chmod +x`). Gear Lever can add it to the application menu.
2. To use it in Game Mode, add it to Steam as a non-Steam game (right-click the AppImage, "Add to Steam").
3. In Game Mode (gamescope) SurvivorsForge starts fullscreen without a window frame; the *Quit* tab (or the Steam menu) closes it. Touchscreen and trackpads work as a mouse, Steam + X opens the keyboard for the search and repo fields.

The portal's file chooser may not show under gamescope, so Game Mode uses no file dialogs: paths are typed, "Install zip" is unavailable (use Desktop Mode for zip installs), and "Save as zip" writes the pack to the Downloads folder.

## Window frame

The window runs with `no-frame`. Title bar, caption buttons and resize edges are Slint components (`ui/components/window-frame.slint`; `ui/components.slint` re-exports the files in `ui/components/`, one per area). Moving and resizing go through winit (`drag_window`, `drag_resize_window`), which needs the Slint feature `unstable-winit-030`. When you upgrade Slint, check that this feature still exists.
