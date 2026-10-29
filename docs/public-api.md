# Public API for other launchers

Other launchers and tools may read the SurvivorsForge addon index and the addon details listed here. No account and no key are needed. The base URL is `https://forgeapi.rezondes.net`.

Only the four endpoints on this page are public for third parties. Every other route of the API can change without notice.

## Conditions

Anyone may use this API on these conditions:

1. **Open source.** The source code of your launcher or tool is public on GitHub.
2. **Credit.** Your README and your app link to the [SurvivorsForge repository](https://github.com/Rezondes/SurvivorsForge) and to the [SurvivorsForge website](https://rezondes.github.io/SurvivorsForge/).
3. **Rules.** Your client follows the rules on this page.

| Endpoint | What it gives you |
|---|---|
| `GET /v1/registry` | Every addon with its latest installable version. The one list to poll. |
| `GET /v1/addons/{slug}` | Details of one addon: description, images, icon, dependencies. |
| `GET /v1/addons/{slug}/versions` | All versions of one addon, newest first. |
| `GET /v1/addons/{slug}/versions/{tag}/findings` | The latest scan of one version with every finding. |

`{slug}` is the `id` from the registry. Downloads are never served by SurvivorsForge: every `asset_url` points to the author's release on GitHub.

## How often

- **`/v1/registry`:** fetch it when your launcher starts, then at most once every 5 minutes. Once an hour is plenty. Always send the `ETag` of your last copy as `If-None-Match`: an unchanged list answers `304 Not Modified` without a body. The answer carries `Cache-Control: public, max-age=300`.
- **The other three:** fetch them when a user opens an addon or is about to install a version. Do not poll them.
- **Limit:** 120 requests per minute and IP address, counted over all routes. Above it the API answers `429` with the error code `rate_limited`. Wait a minute before the next request, and back off the same way on `5xx` answers.
- **User-Agent:** send one with your launcher's name, version and a URL, e.g. `MyLauncher/1.2 (+https://github.com/me/mylauncher)`. That lets us reach you before a change hits you.

## Rules for clients

1. **Check the hash.** Install a zip only when its SHA-256 matches `sha256`. When `sha256` is `null`, the file was not hashed yet: do not install it.
2. **Respect the trust level.** Never install `blocked` or `source_changed`. Ask the user before installing `flagged` or `unscanned`. Show the trust level next to the addon.
3. **Follow removals.** An addon that disappears from `/v1/registry` was deleted or blocked. Stop offering it and do not keep installing it from an older copy of the list.
4. **Link, do not rehost.** Download from `asset_url` and do not mirror the zips.
5. **Do not pose as SurvivorsForge.** Saying that your launcher reads the SurvivorsForge index is fine; looking like the official launcher is not.

## Stability

- The registry carries `"schema": 1`. Within schema 1, fields are only added, never renamed, removed or given a new meaning. Ignore fields you do not know.
- The three detail endpoints follow the same rule within `/v1`.
- A breaking change comes as a new schema number or a new `/v2` path. The old one keeps working for at least three months after the announcement in the release notes on GitHub.

## Code examples

These examples are only a starting point, not tested ready-to-use code. Read them, adapt them to your launcher and test your client yourself.

Each example polls the registry with a `User-Agent` and `If-None-Match`, and downloads a version only when its trust level allows it and its SHA-256 matches. Call `poll` at most every 5 minutes. Addons missing from a new list were deleted or blocked: stop offering them. `latest` is a registry entry's `latest` or an entry of `/v1/addons/{slug}/versions`.

### JavaScript (Node 18+)

```js
const UA = "MyLauncher/1.2 (+https://github.com/me/mylauncher)";
let etag = null, registry = null;

async function poll() {
  const res = await fetch("https://forgeapi.rezondes.net/v1/registry", {
    headers: { "User-Agent": UA, ...(etag && { "If-None-Match": etag }) },
  });
  if (res.status === 304) return registry;
  if (!res.ok) throw new Error(`API answered ${res.status}, wait before the next try`);
  etag = res.headers.get("ETag");
  return (registry = await res.json());
}

async function download(latest, userAgreed = false) {
  if (["blocked", "source_changed"].includes(latest.trust)) throw new Error("not installable");
  if (["flagged", "unscanned"].includes(latest.trust) && !userAgreed) throw new Error("ask the user first");
  if (!latest.sha256) throw new Error("no hash yet");
  const res = await fetch(latest.asset_url, { headers: { "User-Agent": UA } });
  if (!res.ok) throw new Error(`GitHub answered ${res.status}`);
  const zip = new Uint8Array(await res.arrayBuffer());
  const hash = Buffer.from(await crypto.subtle.digest("SHA-256", zip)).toString("hex");
  if (hash !== latest.sha256) throw new Error("SHA-256 mismatch, do not install");
  return zip;
}
```

### TypeScript

```ts
type Trust = "blocked" | "source_changed" | "verified" | "flagged" | "scanned" | "unscanned";
interface Latest { tag: string; asset_url: string; sha256: string | null; trust: Trust }
interface Registry { schema: 1; addons: { id: string; name: string; folders: string[]; latest?: Latest }[] }

const UA = "MyLauncher/1.2 (+https://github.com/me/mylauncher)";
let etag: string | null = null;
let registry: Registry | null = null;

async function poll(): Promise<Registry> {
  const headers: Record<string, string> = { "User-Agent": UA };
  if (etag) headers["If-None-Match"] = etag;
  const res = await fetch("https://forgeapi.rezondes.net/v1/registry", { headers });
  if (res.status === 304 && registry) return registry;
  if (!res.ok) throw new Error(`API answered ${res.status}, wait before the next try`);
  etag = res.headers.get("ETag");
  return (registry = (await res.json()) as Registry);
}

async function download(latest: Latest, userAgreed = false): Promise<Uint8Array> {
  if (latest.trust === "blocked" || latest.trust === "source_changed") throw new Error("not installable");
  if ((latest.trust === "flagged" || latest.trust === "unscanned") && !userAgreed) throw new Error("ask the user first");
  if (!latest.sha256) throw new Error("no hash yet");
  const res = await fetch(latest.asset_url, { headers: { "User-Agent": UA } });
  if (!res.ok) throw new Error(`GitHub answered ${res.status}`);
  const zip = new Uint8Array(await res.arrayBuffer());
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", zip));
  const hash = Array.from(digest, (b) => b.toString(16).padStart(2, "0")).join("");
  if (hash !== latest.sha256) throw new Error("SHA-256 mismatch, do not install");
  return zip;
}
```

### C# (.NET 8+)

```csharp
using System.Net;
using System.Net.Http.Json;
using System.Security.Cryptography;
using System.Text.Json;

var http = new HttpClient();
http.DefaultRequestHeaders.UserAgent.ParseAdd("MyLauncher/1.2 (+https://github.com/me/mylauncher)");
string? etag = null;
JsonElement registry = default;

async Task<JsonElement> Poll()
{
    using var req = new HttpRequestMessage(HttpMethod.Get, "https://forgeapi.rezondes.net/v1/registry");
    if (etag != null) req.Headers.TryAddWithoutValidation("If-None-Match", etag);
    using var res = await http.SendAsync(req);
    if (res.StatusCode == HttpStatusCode.NotModified) return registry;
    res.EnsureSuccessStatusCode(); // 429 or 5xx: wait before the next try
    etag = res.Headers.ETag?.ToString();
    return registry = await res.Content.ReadFromJsonAsync<JsonElement>();
}

async Task<byte[]> Download(JsonElement latest, bool userAgreed = false)
{
    var trust = latest.GetProperty("trust").GetString();
    if (trust is "blocked" or "source_changed") throw new Exception("not installable");
    if (trust is "flagged" or "unscanned" && !userAgreed) throw new Exception("ask the user first");
    var sha = latest.GetProperty("sha256").GetString() ?? throw new Exception("no hash yet");
    var zip = await http.GetByteArrayAsync(latest.GetProperty("asset_url").GetString());
    if (!Convert.ToHexString(SHA256.HashData(zip)).Equals(sha, StringComparison.OrdinalIgnoreCase))
        throw new Exception("SHA-256 mismatch, do not install");
    return zip;
}
```

### Rust (`ureq` 2 with the `json` feature, `serde_json`, `sha2`)

```rust
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::io::Read;

const UA: &str = "MyLauncher/1.2 (+https://github.com/me/mylauncher)";

/// `cache` keeps the last ETag and registry between polls.
fn poll(cache: &mut Option<(String, Value)>) -> Result<Value, Box<dyn Error>> {
    let mut req = ureq::get("https://forgeapi.rezondes.net/v1/registry").set("User-Agent", UA);
    if let Some((etag, _)) = cache.as_ref() {
        req = req.set("If-None-Match", etag);
    }
    let res = req.call()?; // 429 or 5xx: wait before the next try
    if let (304, Some((_, registry))) = (res.status(), cache.as_ref()) {
        return Ok(registry.clone());
    }
    let etag = res.header("ETag").unwrap_or_default().to_string();
    let registry: Value = res.into_json()?;
    *cache = Some((etag, registry.clone()));
    Ok(registry)
}

fn download(latest: &Value, user_agreed: bool) -> Result<Vec<u8>, Box<dyn Error>> {
    let trust = latest["trust"].as_str().unwrap_or_default();
    if matches!(trust, "blocked" | "source_changed") {
        return Err("not installable".into());
    }
    if matches!(trust, "flagged" | "unscanned") && !user_agreed {
        return Err("ask the user first".into());
    }
    let sha = latest["sha256"].as_str().ok_or("no hash yet")?;
    let mut zip = Vec::new();
    ureq::get(latest["asset_url"].as_str().unwrap_or_default()).set("User-Agent", UA).call()?.into_reader().read_to_end(&mut zip)?;
    if format!("{:x}", Sha256::digest(&zip)) != sha {
        return Err("SHA-256 mismatch, do not install".into());
    }
    Ok(zip)
}
```

### Python (standard library)

```python
import hashlib, json, urllib.error, urllib.request

UA = "MyLauncher/1.2 (+https://github.com/me/mylauncher)"
etag, registry = None, None

def poll():
    global etag, registry
    headers = {"User-Agent": UA, **({"If-None-Match": etag} if etag else {})}
    try:
        with urllib.request.urlopen(urllib.request.Request("https://forgeapi.rezondes.net/v1/registry", headers=headers)) as res:
            etag, registry = res.headers["ETag"], json.load(res)
    except urllib.error.HTTPError as e:
        if e.code != 304:
            raise  # 429 or 5xx: wait before the next try
    return registry

def download(latest, user_agreed=False):
    if latest["trust"] in ("blocked", "source_changed"):
        raise ValueError("not installable")
    if latest["trust"] in ("flagged", "unscanned") and not user_agreed:
        raise ValueError("ask the user first")
    if not latest["sha256"]:
        raise ValueError("no hash yet")
    with urllib.request.urlopen(urllib.request.Request(latest["asset_url"], headers={"User-Agent": UA})) as res:
        zip_bytes = res.read()
    if hashlib.sha256(zip_bytes).hexdigest() != latest["sha256"]:
        raise ValueError("SHA-256 mismatch, do not install")
    return zip_bytes
```

## Errors

Errors are JSON with a stable code and a readable message:

```json
{"error": "not_found", "message": "Not found"}
```

| Status | `error` | When |
|---|---|---|
| `404` | `not_found` | Unknown or deleted addon, unknown tag. |
| `422` | `unknown_game_version` | `?game=` names a game version that does not exist. |
| `429` | `rate_limited` | More than 120 requests in a minute. |

## Values

**`trust`**, highest priority first:

| Value | Meaning | Installable |
|---|---|---|
| `blocked` | An admin blocked the version or the whole addon. | no |
| `source_changed` | The file on GitHub differs from the scanned one. | no |
| `verified` | An admin approved exactly this file. | yes |
| `flagged` | The automatic scan found something. | after asking the user |
| `scanned` | The scan found nothing; no admin looked at it. | yes |
| `unscanned` | Not scanned yet, or the scan failed. | after asking the user |

**`scan_status`:** `pending`, `running`, `failed`, `done`.
**`source`:** `available`, or `missing` when the release asset is gone from GitHub.
**Finding `severity`:** `critical` and `warn` make a version `flagged`; `notice` and `info` are shown but the zip still counts as clean.
**Scan `result`:** `clean`, `flagged`, `error`.

## Response types

Types: `string`, `integer`, `boolean`, `T[]` (array) and `object`. "nullable" means the value can be `null`; "optional" means the field can be missing. Dates are a `string` in RFC 3339. The [playground](https://forge.rezondes.net/api-docs/playground) shows real answers.

## `GET /v1/registry`

| Field | Type | Meaning |
|---|---|---|
| `schema` | `integer` | Schema version, `1`. |
| `addons` | `object[]` | Every addon that is neither deleted nor blocked, sorted by `id`. |
| `addons[].id` | `string` | Stable key of the addon. Never changes. Use it as `{slug}` for the other endpoints. |
| `addons[].name` | `string` | Display name. |
| `addons[].author` | `string` | GitHub login of the owner, or the author's name while nobody claimed the addon. |
| `addons[].repo` | `string` | GitHub repository as `owner/name`. |
| `addons[].description` | `string` | One-line summary. |
| `addons[].category` | `string` | Category name, e.g. `Combat`. |
| `addons[].folders` | `string[]` | Every folder the addon installs into the game's addon directory. |
| `addons[].source.type` | `string` | `release` (zip from a release asset) or `tag` (zip of the tag). |
| `addons[].source.asset` | `string`, optional | Name pattern of the release asset; only with `release`. |
| `addons[].trust` | `string`, optional | Trust level of `latest`. Missing while the addon has no version. |
| `addons[].verified_author` | `boolean`, optional | An admin verified at least one version of this addon. |
| `addons[].latest` | `object`, optional | The version to install. Missing while the addon has no installable version. |
| `addons[].latest.tag` | `string` | Git tag of the version. |
| `addons[].latest.asset_url` | `string` | Download URL on GitHub. |
| `addons[].latest.sha256` | `string`, nullable | SHA-256 of the zip, lowercase hex. `null` while the file was not hashed yet. |
| `addons[].latest.size` | `integer`, nullable | Size of the zip in bytes. |
| `addons[].latest.trust` | `string` | Trust level of this version. |
| `addons[].latest.findings` | `object` | `critical` and `warn`, each an `integer`: number of findings of the scan. |
| `addons[].latest.game_min` | `string`, nullable | Oldest compatible game version; `null` means no limit. |
| `addons[].latest.game_max` | `string`, nullable | Newest compatible game version; `null` means no limit. |
| `addons[].dependencies` | `object[]`, optional | Other addons this one needs. Missing when there are none. |
| `addons[].dependencies[].id` | `string` | `id` of the other addon. |
| `addons[].dependencies[].required` | `boolean` | `false` means optional. |

## `GET /v1/addons/{slug}`

Optional query: `?game=<game version>` adds `compatible` for the latest version.

| Field | Type | Meaning |
|---|---|---|
| `slug` | `string` | The addon's `id`. |
| `name` | `string` | Display name. |
| `blocked` | `boolean` | An admin blocked the addon. Then only `slug`, `name`, `blocked` and `block_reason` are sent. |
| `block_reason` | `string`, nullable | Why it was blocked; only on a blocked addon. |
| `summary` | `string` | One-line summary. |
| `description_md` | `string` | Long description as Markdown, may be empty. |
| `description_html` | `string` | `description_md` rendered and sanitized. Prefer it over rendering the Markdown yourself. |
| `category` | `string` | Category name. |
| `repo` | `string` | GitHub repository as `owner/name`. |
| `source_type` | `string` | `release` or `tag`. |
| `asset_pattern` | `string`, nullable | Name pattern of the release asset. |
| `folders` | `string[]` | Every folder the addon installs. |
| `author` | `string`, nullable | GitHub login of the owner, or the author's name while nobody claimed the addon. |
| `owner` | `object`, nullable | `github_id` (`integer`), `login` and `avatar_url` (`string`) of the owner; `null` while nobody claimed the addon. |
| `claimed` | `boolean` | Someone owns the addon. |
| `contributors` | `object[]` | Members who help with the addon: `login` and `avatar_url`, each a `string`. |
| `trust` | `string`, nullable | Trust level of the version the registry offers as `latest`. |
| `latest_tag` | `string`, nullable | Tag of that version. |
| `findings` | `object`, nullable | `critical`, `warn` and `info` (`integer`) of that version; `null` without a version. |
| `dependencies` | `object[]` | `slug`, `name` (`string`), `required` (`boolean`), `trust` (`string`, nullable) and `available` (`boolean`, `false` when deleted or blocked). |
| `install_order` | `string[]` | Slugs of the required dependencies, in the order they have to be installed. |
| `images` | `object[]` | Screenshots in display order: `id`, `width`, `height`, `position` (`integer`), `url` and `caption` (`string`, may be empty). |
| `icon_url` | `string`, nullable | Square icon. |
| `verified_author` | `boolean` | An admin verified at least one version of this addon. |
| `compatible` | `boolean`, nullable, optional | Only with `?game=`. `null` when unknown. |
| `created_at` | `string` | When the addon was added. |
| `updated_at` | `string` | When the addon was last changed. |

## `GET /v1/addons/{slug}/versions`

Optional query: `?game=<game version>` fills `compatible` for every version. The answer is an array, sorted newest first. The rules for clients apply to every version, not only the latest.

| Field | Type | Meaning |
|---|---|---|
| `[].id` | `integer` | Internal id of the version. |
| `[].tag` | `string` | Git tag. |
| `[].name` | `string`, nullable | Release title on GitHub. |
| `[].prerelease` | `boolean` | Marked as pre-release on GitHub. |
| `[].published_at` | `string` | When the release was published. |
| `[].asset_name` | `string`, nullable | File name of the zip. |
| `[].asset_url` | `string` | Download URL on GitHub. |
| `[].size` | `integer`, nullable | Size of the zip in bytes. |
| `[].sha256` | `string`, nullable | SHA-256 of the zip, lowercase hex. |
| `[].downloads` | `integer` | Download count GitHub reports for the asset. |
| `[].scan_status` | `string` | State of the scan. |
| `[].source` | `string` | Whether the asset is still on GitHub. |
| `[].deleted_on_github` | `boolean` | The release is gone from GitHub; the version stays listed for history. |
| `[].trust` | `string` | Trust level of this version. |
| `[].findings` | `object` | `critical`, `warn` and `info`, each an `integer`. |
| `[].game_min` | `string`, nullable | Oldest compatible game version; `null` means no limit. |
| `[].game_max` | `string`, nullable | Newest compatible game version; `null` means no limit. |
| `[].compatible` | `boolean`, nullable | Only filled with `?game=`; `null` otherwise or when unknown. |

## `GET /v1/addons/{slug}/versions/{tag}/findings`

| Field | Type | Meaning |
|---|---|---|
| `tag` | `string` | Git tag of the version. |
| `scan_status` | `string` | State of the scan. |
| `scan` | `object`, nullable | The latest scan; `null` while the version was never scanned. |
| `scan.result` | `string` | Result of the scan. |
| `scan.scanner_version` | `string` | Version of the scanner that ran. |
| `scan.sha256` | `string` | SHA-256 of the file that was scanned. |
| `scan.finished_at` | `string` | When the scan finished. |
| `scan.findings` | `object[]` | Every finding, sorted by severity, `critical` first. |
| `scan.findings[].rule` | `string` | Id of the rule, e.g. `lua.dynamic_global`. |
| `scan.findings[].severity` | `string` | Severity of the finding. |
| `scan.findings[].file` | `string` | Path inside the zip. |
| `scan.findings[].line` | `integer`, nullable | Line in that file. |
| `scan.findings[].message` | `string` | What the rule found. |
| `scan.findings[].snippet` | `string`, nullable | The code at that line. |
