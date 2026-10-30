# Running the SurvivorsForge platform

The platform is three programs and a database, all started by Docker Compose:

| Service | What it does |
|---|---|
| `api` | `forge-api serve`: the public API, GitHub login, webhooks |
| `worker` | `forge-api worker`: syncs releases, scans every new file, checks that files on GitHub stay unchanged |
| `web` | `forge-web`: the website, a client of the API |
| `caddy` | both sites over plain HTTP (website on port 3080, API on 3081), the uploaded images |
| `postgres` | all data except images |

Releases and downloads stay on GitHub. The server keeps metadata, scan results and images.

To try everything on your own machine first, see [local-setup.md](local-setup.md).

## What you need

- A Linux server with Docker and the Compose plugin (2.24 or newer). Ports 3080 (website) and
  3081 (API) reachable for the TLS proxy in front of it, e.g. NGINX Proxy Manager or Cloudflare
  with an origin rule, that sends each domain to its port. Caddy stays behind that proxy: it serves
  images and packs from the volumes and passes the visitor's address on. It trusts
  `CF-Connecting-IP` and `X-Forwarded-For` only from Cloudflare and private networks, so a proxy
  on a public address of its own shows up as one visitor. To let Caddy get certificates itself instead, publish 80 and 443
  in a `compose.override.yaml` and set `DOMAIN_WEB` and `DOMAIN_API` to the bare domains.
- Two DNS `A` (or `AAAA`) records pointing at the server, for example `forge.example.com` for
  the website and `api.forge.example.com` for the API.
- A GitHub account to own the GitHub App.

## 1. Create the GitHub App

On GitHub: *Settings → Developer settings → GitHub Apps → New GitHub App*.

| Field | Value |
|---|---|
| GitHub App name | e.g. `SurvivorsForge`; its URL name becomes `GITHUB_APP_SLUG` |
| Homepage URL | `https://forge.example.com` |
| Callback URL | `https://api.forge.example.com/v1/auth/github/callback` |
| Request user authorization (OAuth) during installation | off |
| Webhook | active, URL `https://api.forge.example.com/v1/github/webhook` |
| Webhook secret | a long random value (`openssl rand -hex 32`), becomes `GITHUB_WEBHOOK_SECRET` |
| Repository permissions | `Contents: Read-only`, `Metadata: Read-only` |
| Account permissions | none |
| Subscribe to events | `Release`, `Create`, `Repository` (`Installation` and `Installation repositories` are always sent) |
| Where can this GitHub App be installed? | Any account |

After saving, note from the App's settings page:

- **App ID** → `GITHUB_APP_ID`
- **Client ID** → `GITHUB_CLIENT_ID`
- **Client secret** (generate one) → `GITHUB_CLIENT_SECRET`
- **Private key** (generate one, a `.pem` file) → see `GITHUB_APP_PRIVATE_KEY_PATH`
- **URL name** of the App (`github.com/apps/<name>`) → `GITHUB_APP_SLUG`

## 2. Configure

The server needs no copy of the repository: `compose.yaml` pulls the images
`ghcr.io/rezondes/survivorsforge-api` and `ghcr.io/rezondes/survivorsforge-web` and holds Caddy's
configuration. Both files come with every release.

```bash
mkdir survivorsforge && cd survivorsforge
curl -fLO https://github.com/Rezondes/SurvivorsForge/releases/latest/download/compose.yaml
curl -fL -o .env https://github.com/Rezondes/SurvivorsForge/releases/latest/download/env.example
mkdir -p secrets
cp ~/Downloads/your-app.private-key.pem secrets/github-app.pem
chmod 644 secrets/github-app.pem
```

The containers run as the unprivileged user `nonroot` (uid 65532), so the key must be readable
for it (`chmod 644`, or `chown 65532` and `chmod 600`). `.env` and `secrets/` are git-ignored;
never commit them.

Your own GitHub user ID for `ADMIN_GITHUB_IDS` is the `id` field of
`https://api.github.com/users/<your-login>`.

### Every variable in `.env`

**Docker Compose**

| Variable | Required | Meaning | Example |
|---|---|---|---|
| `FORGE_VERSION` | no | Release of the images to run, e.g. `0.3.2`; without it `latest` | `0.3.2` |
| `POSTGRES_PASSWORD` | yes | Database password; Compose builds the database URL from it | output of `openssl rand -hex 24` |
| `DOMAIN_WEB` | yes | Site of the website as Caddy serves it | `http://forge.example.com:3080` |
| `DOMAIN_API` | yes | Site of the API as Caddy serves it | `http://api.forge.example.com:3081` |
| `ACME_EMAIL` | yes | Contact for Let's Encrypt, used only when Caddy gets certificates itself | `admin@example.com` |
| `FORGE_EDGE_SUBNET` | no | Subnet of the internal edge network; change it if it clashes with another Docker network | `10.213.17.0/24` |
| `FORGE_CADDY_IP` | no | Caddy's fixed address inside that subnet; the API trusts its `X-Forwarded-For` | `10.213.17.10` |
| `FORGE_WEB_IP` | no | The website's fixed address inside that subnet; the API trusts the client IP it forwards | `10.213.17.11` |
| `GITHUB_APP_PRIVATE_KEY_PATH` | no | Path of the App's `.pem` on the host | `./secrets/github-app.pem` |

**API (`forge-api`)**

| Variable | Required | Meaning | Example |
|---|---|---|---|
| `DATABASE_URL` | yes, set by Compose | Postgres connection | set by `compose.yaml` |
| `PUBLIC_API_URL` | yes | Public base URL of the API, no trailing slash | `https://api.forge.example.com` |
| `PUBLIC_IMAGE_URL` | yes | Public base URL of uploaded images | `https://forge.example.com/img` |
| `IMAGE_DIR` | no, set by Compose | Folder the API stores images in | `/data/images` |
| `PUBLIC_PACK_URL` | yes | Public base URL of built addon packs; Caddy serves them under `/packs-files/` | `https://forge.example.com/packs-files` |
| `PACK_DIR` | no | Folder the worker stores built packs in | `/data/packs` |
| `PACK_MAX_ADDONS` | no | Most addons in one pack | `20` |
| `PACK_MAX_MB` | no | Largest pack zip in MB; all verified addons together are about 37 MB | `50` |
| `PACKS_PER_HOUR` | no | New packs a user may create per hour | `10` |
| `PACK_RETENTION_DAYS` | no | Days a built pack is kept without a download; the next download builds it again | `7` |
| `PACK_STORAGE_MB` | no | All built packs together; above it the least recently downloaded are deleted | `2048` |
| `BIND_ADDR` | no | Address the API listens on in its container | `0.0.0.0:8080` |
| `METRICS_BIND_ADDR` | no | Address of the Prometheus metrics of the API and the worker; never published | `0.0.0.0:9100` |
| `MIGRATE_ON_START` | no | Apply database migrations when the API starts | `true` |
| `RATE_LIMIT_PER_MINUTE` | no | Requests per minute and client IP, for the API and the website each | `120` |
| `CACHE_SECONDS` | no | How long public answers stay in the API's memory cache; a write through the API empties it, worker changes show after this time; `0` turns it off | `60` |
| `TRUSTED_PROXIES` | no, set by Compose | Proxies whose `X-Forwarded-For` counts | API: `FORGE_CADDY_IP` and `FORGE_WEB_IP`; website: `FORGE_CADDY_IP` |
| `WEBSITE_IP` | no, set by Compose | The website's address; it limits each visitor itself, so the API does not count its requests again | `FORGE_WEB_IP` |
| `SOURCE_CHECK_BATCH` | no | Versions compared with GitHub per 6-hourly run | `200` |
| `LAUNCHER_REPO` | no | GitHub repository of the launcher; `/stats` shows its release downloads | `Rezondes/SurvivorsForge` |
| `GITHUB_API_URL` | no | GitHub REST API | `https://api.github.com` |
| `GITHUB_READ_TOKEN` | no | Token for public reads of repos without the App; GitHub then allows 5000 instead of 60 requests per hour. A fine-grained token with *Public repositories (read-only)* and no permissions | `github_pat_...` |
| `GITHUB_APP_ID` | yes | App ID from step 1 | `123456` |
| `GITHUB_APP_PRIVATE_KEY` | no | The key as text (newlines as `\n`) instead of a file | empty |
| `GITHUB_APP_PRIVATE_KEY_FILE` | no, set by Compose | Path of the key inside the container | `/run/secrets/github-app.pem` |
| `GITHUB_WEBHOOK_SECRET` | yes | Webhook secret from step 1 | random value |
| `GITHUB_OAUTH_URL` | no | GitHub's OAuth pages | `https://github.com` |
| `GITHUB_CLIENT_ID` | yes | Client ID from step 1 | `Iv1.abc...` |
| `GITHUB_CLIENT_SECRET` | yes | Client secret from step 1 | secret |
| `AUTH_RETURN_URLS` | yes | Exact callback URLs a login may return to | `https://forge.example.com/auth/callback` |
| `ADMIN_GITHUB_IDS` | no | Comma-separated numeric GitHub IDs of admins | `1234567` |
| `ADMIN_STATS_ROLES` | no | Roles that see `/admin/stats`: `admin`, `moderator` (default both) | `admin` |

**Website (`forge-web`)**

| Variable | Required | Meaning | Example |
|---|---|---|---|
| `API_INTERNAL_URL` | yes, set by Compose | How the website reaches the API | `http://api:8080` |
| `PUBLIC_WEB_URL` | yes | Public base URL of the website | `https://forge.example.com` |
| `COOKIE_KEY` | yes | At least 32 random characters, encrypts the session cookies | output of `openssl rand -hex 32` |
| `GITHUB_APP_SLUG` | yes | URL name of the App, for the "install the App" links (organization repos, instant updates) | `survivorsforge` |
| `WEB_BIND_ADDR` | no | Address the website listens on in its container | `0.0.0.0:8080` |

## 3. First start

```bash
docker compose up -d
docker compose ps
```

All five services should become `healthy`. The API applies the database migrations on start
(`MIGRATE_ON_START=true`); to run them by hand: `docker compose exec api forge-api migrate`.

Carry over the old verified list. The addons are created without an owner; their authors claim
them later on the website by proving admin rights on the repo:

```bash
curl -fLO https://raw.githubusercontent.com/Rezondes/SurvivorsForge/main/registry/verified.json
docker compose cp verified.json api:/tmp/verified.json
docker compose exec api forge-api import-verified /tmp/verified.json
```

Then log in on the website with your GitHub account (it must be in `ADMIN_GITHUB_IDS`), open
*Admin → Review* once the first scans are done and verify the versions you trust.

To publish the verified list for older launchers, export it and commit `registry/verified.json`:

```bash
docker compose exec api forge-api export-verified > registry/verified.json
```

## 4. Updates

Get the `compose.yaml` of the new release (it may add services or settings), then the images:

```bash
curl -fLO https://github.com/Rezondes/SurvivorsForge/releases/latest/download/compose.yaml
docker compose pull
docker compose up -d
```

With `FORGE_VERSION` set, raise it first; without it `docker compose pull` fetches `latest`.
Compare `.env` with the release's `env.example` for new variables.

After a release with new scanner rules, scan all versions again:

```bash
docker compose exec api forge-api rescan --all
```

## 5. Backups

Two things hold data: the database and the image volume. The `packs` volume needs no backup:
the worker builds every pack again on its next download.

```bash
# Database, e.g. daily from cron
docker compose exec -T postgres pg_dump -U forge -Fc forge > backup/forge-$(date +%F).dump
# Images
docker run --rm -v survivorsforge_images:/data -v "$PWD/backup":/backup alpine \
  tar czf /backup/images-$(date +%F).tar.gz -C /data .
```

Restore:

```bash
docker compose up -d postgres
docker compose exec -T postgres pg_restore -U forge -d forge --clean --if-exists < backup/forge-2026-01-01.dump
docker run --rm -v survivorsforge_images:/data -v "$PWD/backup":/backup alpine \
  tar xzf /backup/images-2026-01-01.tar.gz -C /data
docker compose up -d
```

The volume name starts with the Compose project name, by default the folder name.

## 6. The launcher

The launcher talks only to the API: the addon list, release and tag metadata and game versions
come from there. Only the zip files are downloaded from GitHub directly. A release build talks to
the official platform, `https://forgeapi.rezondes.net` and `https://forge.rezondes.net`; a debug
build to the local platform of [local-setup.md](local-setup.md).

To build a launcher for your own platform, set both addresses at build time ("Share as link" opens
the website's pack builder):

```bash
SURVIVORSFORGE_API_URL=https://api.forge.example.com SURVIVORSFORGE_WEB_URL=https://forge.example.com cargo build --release
```

Players of a launcher built for another platform point it at yours in *Settings → Platform API*
and *Platform website*. The order is: the environment variables at run time, then these settings,
then the addresses built in. When the API cannot be reached, the launcher shows its cached list and
installs nothing.

## 7. Addon packs

Players of the browser version upload one zip with all their addons. On the website
(*Build a pack*) and in the launcher (tick boxes in *Browse*) they pick addons; logged-in users
get a link like `https://forge.example.com/packs/k5G6tFV3H1` that anyone can open to see the
addons and versions and download the zip. Only checked or verified versions with a known hash go
into a pack.

The worker builds a pack's zip on its first download: it fetches each version from GitHub,
checks its SHA-256 against the scanned one and puts every addon folder at the root of one zip.
Packs with the same versions share the file. Builds run one after the other; the limits above
bound their size and number. An hourly job deletes zips that nobody downloaded for
`PACK_RETENTION_DAYS` and, above `PACK_STORAGE_MB`, the least recently downloaded ones. The
launcher's *Save as zip* builds the same zip on the player's computer and needs no server space.

## Running only the API

The website and Caddy are optional; the API runs on its own:

```bash
docker compose up -d postgres api worker
```

Its port is not published then; put your own proxy in front or add a `ports` entry for `api`.

## Local development

```bash
POSTGRES_PASSWORD=forge POSTGRES_HOST_PORT=15432 docker compose -f compose.yaml -f compose.dev.yaml up -d postgres
DATABASE_URL=postgres://forge:forge@127.0.0.1:15432/forge cargo test --workspace
bash scripts/compose-smoke.sh
```
