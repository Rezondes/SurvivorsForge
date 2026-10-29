# Testing the platform locally

The whole platform (database, API, worker, website, Caddy) runs on your own machine over plain
HTTP, plus the launcher against it. Nothing here needs a domain or a server. For the real server
see [self-hosting.md](self-hosting.md).

| Address | What |
|---|---|
| http://localhost:3080 | Website |
| http://localhost:3081 | API |

## What you need

- Docker Desktop (or Docker with the Compose plugin, 2.24 or newer). Ports 3080 and 3081 free;
  if another program uses them, change the numbers in `compose.local.yaml` and `.env.local`.
- A GitHub account. Logging in, publishing and admin pages need a GitHub App; browsing does not.
- For the launcher: the Rust toolchain (`rustup`), as for any build of this repo.

## 1. Create a test GitHub App

Use a separate App for local tests, not the one of your server, because GitHub allows only one
callback URL per App. On GitHub: *Settings → Developer settings → GitHub Apps → New GitHub App*.

| Field | Value |
|---|---|
| GitHub App name | e.g. `SurvivorsForge Local <your name>` (must be unique on GitHub) |
| Homepage URL | `http://localhost:3080` |
| Callback URL | `http://localhost:3081/v1/auth/github/callback` |
| Request user authorization (OAuth) during installation | off |
| Webhook | **inactive** (GitHub cannot reach your machine) |
| Repository permissions | `Contents: Read-only`, `Metadata: Read-only` |
| Where can this GitHub App be installed? | Only on this account |

After saving, from the App's settings page:

- **App ID** → `GITHUB_APP_ID`
- **Client ID** → `GITHUB_CLIENT_ID`
- **Client secret** (*Generate a new client secret*) → `GITHUB_CLIENT_SECRET`
- **URL name** (the end of `github.com/apps/<name>`) → `GITHUB_APP_SLUG`
- **Private key** (*Generate a private key*, a `.pem` file is downloaded) → step 2

To try publishing your own addon later, use a public repo of your account that has a release
with an addon zip. Repos of your own account need no App; installing it (*Install App* in the
App's settings) brings new releases at once instead of with the poll every 15 minutes. Repos of an
organization need the App and your admin rights there.

## 2. Configure

In the repository folder:

```bash
cp .env.local.example .env.local
mkdir -p secrets
cp ~/Downloads/<your-app>.private-key.pem secrets/github-app-local.pem
```

Fill in the empty values of `.env.local`: the four GitHub values from step 1 and
`ADMIN_GITHUB_IDS`, your numeric GitHub user ID (the `id` field of
`https://api.github.com/users/<your-login>`), which makes you admin. Everything else is ready for
localhost. `.env.local` and `secrets/` are git-ignored.

Copy the key **before** the first start. If `secrets/github-app-local.pem` does not exist then,
Docker creates an empty folder of that name and the API stops with `Is a directory`; delete the
folder, copy the key and start again.

The commands below all need the same three options. A shortcut saves typing:

```bash
# Git Bash / Linux / macOS
alias dcl='docker compose --env-file .env.local -f compose.yaml -f compose.local.yaml'
```

```powershell
# PowerShell
function dcl { docker compose --env-file .env.local -f compose.yaml -f compose.local.yaml @args }
```

## 3. Start

```bash
dcl up -d --build
dcl ps
```

The first build takes a few minutes. All five services should become `healthy`. Then open
http://localhost:3080. The local setup is its own Compose project (`survivorsforge-local`), so its
database is separate from the Postgres that `cargo test` uses.

## 4. Fill it with the verified addons

```bash
dcl cp registry/verified.json api:/tmp/verified.json
dcl exec api forge-api import-verified /tmp/verified.json
```

In Git Bash on Windows, put `MSYS_NO_PATHCONV=1` in front of the second command, otherwise Git
Bash turns `/tmp/verified.json` into a Windows path.

The worker now fetches the releases of the 12 addons and scans them; that takes a few minutes
(`dcl logs -f worker`). Afterwards they are listed on the website.

The scanner is strict, so most of them come out *Flagged*. Log in (*Log in with GitHub*, top
right), open *Admin → Review* and verify the versions you trust. Only checked or verified versions
can be downloaded without a warning and go into addon packs.

## 5. Things to try

- **Search and filter**: http://localhost:3080/addons, search field, *Check*, *Category*, *Sort*.
- **Addon packs**: *Build a pack*, tick some addons, *Create the pack*, then *Download the pack*.
  The first download builds the zip (a few seconds), later ones come straight from the file. Open
  the pack link in a private window: it works without login.
- **Publish an addon**: *My addons → Publish an addon*, with the repo you installed the App on.
- **Admin**: *Admin* in the top bar: review queue, reports, users, categories, game versions,
  audit log.

Without webhooks, a new release of an addon shows up after the next poll, every 15 minutes. A
newly added addon is synced at once. Creators check one of their addons at once with *Force
Refresh* in *My addons* (once every 5 minutes per user), admins check all of them with *Check all
for updates* under *Admin > Addons*. Repos without the App installed are read through GitHub's
API without a token, which allows 60 requests per hour per IP; the 12 verified addons need about
25 of them per poll, so four polls an hour run out. Set `GITHUB_READ_TOKEN` (5000 per hour) when
you keep the stack running.

## 6. The launcher against the local platform

A debug build talks to the local platform on its own (`http://localhost:3081` for the API,
`http://localhost:3080` for the website):

```bash
cargo run
```

The launcher then reads the addon list, releases and game versions from the local API; the zips
still come from GitHub. *Browse* shows the category filter and the tick boxes for addon packs
(*Save as zip*, *Share as link*). To see the offline mode, stop the API (`dcl stop api`) and press
*Check for updates*.

Another address (other ports, a test server) goes into *Settings → Platform API* and *Platform
website*. A release build (`cargo run --release`) talks to the official platform
(`https://forgeapi.rezondes.net`) unless these settings or the `SURVIVORSFORGE_API_URL` and
`SURVIVORSFORGE_WEB_URL` environment variables point elsewhere.

## 7. Logs, stop, start over

```bash
dcl logs -f api worker web    # follow the logs
dcl stop                      # stop, data stays
dcl up -d --build             # start again after code changes
dcl down -v                   # remove everything, including the database and the volumes
```
