# SurvivorsForge

A desktop mod manager for the addons of [Azeroth Survivors](https://wow.aethus.pl), a private World of Warcraft 1.12.1 instance. It installs, updates and removes addons in the game's `AddOns` folder, in the spirit of CurseForge but scoped to this one game.

Addon sources:

- **Verified addons** from a central JSON list (`registry/verified.json`), so the list can change without a new release.
- **GitHub repos** added by hand. SurvivorsForge detects whether a repo is an addon and tracks it for updates.
- **Local zip files**, installed or updated by hand.

Built with Rust and [Slint](https://slint.dev).

Homepage and download: <https://rezondes.github.io/SurvivorsForge/>. Addon platform: <https://forge.rezondes.net>.

> **Linux and Steam Deck support is experimental.** 

## Requirements

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

Tests run offline. Tests that need the network are marked `#[ignore]`:

```bash
cargo test -- --ignored
```

To try the app against a copy of an AddOns folder without touching your real settings, point the data folder somewhere else:

```bash
SURVIVORSFORGE_DATA_DIR=/path/to/test-data cargo run
```

### Testing on Linux with Docker

`docker/` holds two images. With Docker Desktop on WSL2, their windows appear on the Windows desktop through WSLg.

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
| `src/config.rs` | `config.json`, data folder, AddOns folder validation |
| `src/toc.rs` | `.toc` parser (`## Key: Value`, color code stripping) |
| `src/scan.rs` | Scans the AddOns folder for installed addons |
| `src/archive.rs` | Finds addon folders inside a zip, extracts them safely |
| `src/state.rs` | `state.json`: which addon came from which source in which version |
| `src/install.rs` | Atomic install/update, uninstall to the recycle bin |
| `src/registry.rs` | Verified list: load order, validation, asset glob matching |
| `src/github.rs` | Release and tag metadata through the platform API, zip downloads from GitHub, the `GitHub` trait |
| `src/updates.rs` | Resolves the remote version per source, decides "update available" |
| `src/repo_detect.rs` | Detection for manually added repos |
| `src/controller.rs` | Pure view logic: rows, badges, filters, zip install plan |
| `src/self_update.rs` | Updates SurvivorsForge itself from its GitHub releases |
| `ui/*.slint` | Theme tokens, components (title bar, rows, dialogs), main window |
| `assets/icon.png`, `assets/icon.ico` | App icon: window and taskbar (PNG via Slint), `.exe` icon (ICO via `winresource` in `build.rs`) |
| `assets/survivorsforge.desktop` | Linux desktop entry: name, icon and app id for the AppImage |
| `packaging/linux/` | `AppRun` and `build-appimage.sh` for the Linux AppImage |
| `docker/` | Linux build and runtime containers for testing on Windows (see Tests) |
| `ui/fonts/` | Morpheus (headings, shareware by Kiwi Media, `README-morpheus.txt`) and Marcellus (text, SIL Open Font License, `OFL-marcellus.txt`), embedded at build time |
| `registry/verified.json` | The verified addon list |
| `tests/` | Integration tests for zip analysis and installation |

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
| `verified` | Repo and source spec from the verified list |
| `github` | Repo and source spec stored when the repo was added |
| `zip` | None (manual) |

A source spec says where updates come from:

| Spec | Remote version | Download |
|---|---|---|
| `release` | Tag of the release GitHub marks as "latest" | The asset that matches the `asset` glob (`*` wildcard) |
| `tag` | Highest tag by the numbers in its name (`v2.8.10` before `v2.8.9`) | Source zip of that tag |
| `branch` | Newest commit SHA of the default branch | Source zip of that commit |

Tags across the repos are not uniform (`v3.0.36`, `v2.6.5-azs`, `build-<sha>`). An update is available when the remote id differs from the installed one; there is no semver comparison.

Addons found on disk that match a verified entry are adopted on startup with version `toc:<Version>`. The first update check replaces that with the release tag when both match (ignoring a leading `v`).

### Installation

1. The zip is extracted to `AddOns/.survivorsforge/staging/<id>/`. That folder is always on the same file system as the addons, even when AddOns is a mount or a link to another drive, so `rename` is atomic. The game ignores it, since it holds no matching `.toc`.
2. Existing folders of the addon move to `.survivorsforge/backup/<id>/`.
3. The new folders move into AddOns.
4. On any error, everything is moved back. On success, backup and staging are deleted.

An install never overwrites folders that belong to another managed addon (`FolderOwnedBy`). Uninstalling moves folders to the recycle bin. SavedVariables in `WTF` are never touched.

### Adding a GitHub repo

Accepts `owner/repo` or a GitHub URL. Detection order:

1. Each `.zip` asset of the latest release is downloaded and analyzed; the first one with an addon wins (installer zips without an addon are skipped). The asset name becomes a glob by replacing the version with `*` (`LuckyOdds-v0.6.0.zip` becomes `LuckyOdds-v*.zip`).
2. Otherwise the source zip of the newest tag.
3. Otherwise the source zip of the default branch.

Repos that are already in the verified list are refused with a pointer to Browse.

### GitHub access

The launcher asks the platform API for release and tag metadata (`/v1/github/{owner}/{repo}/releases/latest`, `/releases/{tag}`, `/refs`). The API reads GitHub and caches the answers for 10 minutes. Only the zip files come from GitHub directly, and only from GitHub's download hosts (`github.com`, `codeload.github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com`).

The API does not use the GitHub REST API for this. Without a token it allows 60 requests per hour. Everything comes from plain github.com endpoints instead, which have no such budget:

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

`registry/verified.json`, schema version 1:

```json
{
  "schema": 1,
  "addons": [
    {
      "id": "survivor-ui",
      "name": "Survivor UI",
      "author": "morderous",
      "repo": "morderous/survivor-ui",
      "description": "Complete Survivor UI including Survivor Plates (nameplates).",
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

Without the API the launcher is offline: it shows the cached list with its age, and installs and updates wait until the API answers again. The addon list is managed on the platform; this file is the built-in copy for the first start without network.

## Platform

Besides the launcher, this repository holds the SurvivorsForge platform: an API (`crates/forge-api`),
a website (`crates/forge-web`), the addon scanner (`crates/forge-scan`) and the code they share
with the launcher (`crates/forge-core`). Creators log in with GitHub and publish addons from their
own repos, every release is scanned for harmful code, and admins verify versions. Downloads stay
on GitHub. Players of the browser version bundle several addons into one zip, an addon pack, on
the website or in the launcher (*Browse*, tick the addons, *Save as zip* or *Share as link*). How to
run it: [docs/local-setup.md](docs/local-setup.md) on your own machine, [docs/self-hosting.md](docs/self-hosting.md) on a server. Other launchers may read the addon index: [docs/public-api.md](docs/public-api.md).

## Data locations

| What | Where |
|---|---|
| Settings (`config.json`) | `%APPDATA%\SurvivorsForge\` (Linux: `~/.config/SurvivorsForge/`) |
| Install state (`state.json`) | Same folder, one section per AddOns folder |
| Verified list cache | `cache/verified.json` in the same folder |
| Staging, backup, downloads | `AddOns\.survivorsforge\` |

`SURVIVORSFORGE_DATA_DIR` overrides the settings folder. A damaged `state.json` is moved to `state.json.bak` and the app starts with an empty state.

## Releases and self-update

`.github/workflows/ci.yml` runs tests and Clippy on Windows and Ubuntu 22.04 for every push and pull request.

`.github/workflows/release.yml` runs on tags `v*`:

1. Fails if the tag does not match the `Cargo.toml` version.
2. Builds on Windows and on Ubuntu 22.04 in parallel.
3. Publishes one GitHub release with both assets, only if both builds succeeded:
   - `SurvivorsForge-<version>-x86_64-pc-windows-msvc.zip`
   - `SurvivorsForge-<version>-x86_64.AppImage`
4. Pushes the platform images to ghcr.io and attaches `compose.yaml` and `env.example`.

A tag with a hyphen, e.g. `v0.4.0-rc.1` (with `version = "0.4.0-rc.1"` in `Cargo.toml`), makes a
pre-release for testing: installed launchers do not update to it, and the images get only the tag
`0.4.0-rc.1`, not `latest`. Run it on a server with `FORGE_VERSION=0.4.0-rc.1`. The final `v0.4.0`
later updates everyone, the testers included.

To cut a release: bump `version` in `Cargo.toml`, commit, then tag and push:

```bash
git tag v0.2.0
```

```bash
git push origin v0.2.0
```

On startup the app checks the latest SurvivorsForge release. If its version is higher (semver), a banner offers "Update and restart":

- Windows downloads the zip, swaps the running executable with `self-replace` and starts the new one.
- Linux downloads the new AppImage next to the running one (`$APPIMAGE`), makes it executable and renames it over the old file. A bare binary outside an AppImage can't replace itself; its banner opens the download page instead.

## Steam Deck

Experimental and not yet tested on a real Deck.

Azeroth Survivors is not on Steam. The game creates its `benilla-config/AddOns` folder on first start, so start the game once before setting up SurvivorsForge. The folder is expected next to the game's executable wherever it was unpacked, whether the Linux build runs natively or the Windows build through Proton.

1. In Desktop Mode, download the AppImage and make it executable (file properties, or `chmod +x`). Gear Lever can add it to the application menu.
2. To use it in Game Mode, add it to Steam as a non-Steam game (right-click the AppImage, "Add to Steam").
3. In Game Mode (gamescope) SurvivorsForge starts fullscreen without a window frame; quit it from the Steam menu. Touchscreen and trackpads work as a mouse, Steam + X opens the keyboard for the search and repo fields.

"Install zip" needs a file dialog, which Game Mode may not provide: use Desktop Mode for zip installs.

## Window frame

The window runs with `no-frame`. Title bar, caption buttons and resize edges are Slint components (`ui/components.slint`). Moving and resizing go through winit (`drag_window`, `drag_resize_window`), which needs the Slint feature `unstable-winit-030`. When you upgrade Slint, check that this feature still exists.
