# Publishing an addon on SurvivorsForge

This guide takes you from an addon folder on your computer to an addon that players install with
one click. You need no programming tools besides a text editor: everything happens in the
browser on GitHub and in the GitHub Desktop app.

The steps:

1. [Get a GitHub account and GitHub Desktop](#1-get-a-github-account-and-github-desktop)
2. [Create the repository](#2-create-the-repository)
3. [Put your addon in it](#3-put-your-addon-in-it)
4. [Save changes (commit and push)](#4-save-changes-commit-and-push)
5. [Add the release workflow](#5-add-the-release-workflow)
6. [Publish a new version with one button](#6-publish-a-new-version-with-one-button)
7. [Publish the addon on SurvivorsForge](#7-publish-the-addon-on-survivorsforge)
8. [When something goes wrong](#8-when-something-goes-wrong)

A few words that come up:

| Word | Meaning |
|---|---|
| Repository (repo) | A folder on GitHub that holds your addon's files and remembers every change |
| Commit | One saved change, with a short message saying what changed |
| Push | Sending your commits from your computer to GitHub |
| Release | A published version of your addon on GitHub, with a zip file players download |
| Tag | A name such as `v1.2.0` that marks the commit a release was made from |
| Workflow | A small script GitHub runs for you, here to build the zip and make the release |

## 1. Get a GitHub account and GitHub Desktop

1. Sign up on <https://github.com/signup>. Your user name becomes part of your addon's address
   (`github.com/<user name>/<addon>`), so pick one you like.
2. Install GitHub Desktop from <https://desktop.github.com> and sign in with that account
   (*File → Options → Accounts* on Windows).

## 2. Create the repository

1. On GitHub, click **+** at the top right, then **New repository**.
2. **Repository name**: the name of your addon, for example `MyAddon`. Use the same spelling as
   your addon folder and `.toc` file.
3. Choose **Public**. SurvivorsForge only publishes public repos.
4. Tick **Add a README file**. Leave the rest as it is.
5. Click **Create repository**.

## 3. Put your addon in it

1. On the new repo's page, click **Code → Open with GitHub Desktop** and pick a folder on your
   computer. GitHub Desktop now has a local copy of the repo there.
2. Copy the **contents** of your addon folder into that folder, not the addon folder itself. The
   `.toc` file sits at the top level of the repo:

   ```text
   MyAddon/            (the repo folder on your computer)
     MyAddon.toc
     MyAddon.lua
     README.md
   ```

3. Make sure the `.toc` file has a version line. The release button in step 6 changes it for you:

   ```text
   ## Interface: 11200
   ## Title: My Addon
   ## Author: YourName
   ## Version: 1.0.0
   MyAddon.lua
   ```

   `## Interface: 11200` is the game version WoW 1.12 expects. What the game offers addons
   beyond the 1.12 API (the run's clock, bosses, meters, hiding the HUD) is in the
   [addon API of the game](https://forge.rezondes.net/addon-api).

One repo holds one addon. Keep files the game does not need (screenshots, design files, `.exe`
tools) out of the repo or in a folder that does not go into the zip (see step 5).

## 4. Save changes (commit and push)

Every time you change your addon, save the change to GitHub:

1. Open GitHub Desktop. The left side lists the changed files.
2. At the bottom left, write a short **Summary** of what you changed, for example
   `Fix the frame position after a reload`.
3. Click **Commit to main**.
4. Click **Push origin** at the top.

The change is now on GitHub. Players do not get it yet: they get new versions only through a
release (step 6). So you can commit as often as you like.

Before every commit, test the addon in the game. Tip: you can copy your repo folder into the
game's `AddOns` folder under the addon's name, or work directly in a repo cloned there.

## 5. Add the release workflow

You do this once per addon.

1. On GitHub, open your repo and click **Add file → Create new file**.
2. As the file name, type exactly `.github/workflows/release.yml`. The slashes create the folders.
3. Paste the workflow below.
4. In the line `ADDON: MyAddon`, replace `MyAddon` with the name of your `.toc` file without
   `.toc`. That name also becomes the addon folder in the zip.
5. Click **Commit changes...**, then **Commit changes** again.

```yaml
name: Release

# Adds a "Run workflow" button under Actions -> Release. Type the new version there and the
# workflow sets "## Version:" in the .toc, commits, tags, builds the zip and creates the release.
on:
  workflow_dispatch:
    inputs:
      version:
        description: "New version, for example 1.2.0"
        required: true

permissions:
  contents: write

jobs:
  release:
    runs-on: ubuntu-latest
    env:
      ADDON: MyAddon
      VERSION: ${{ inputs.version }}
    steps:
      - uses: actions/checkout@v4

      - name: Check the version number
        run: |
          if ! echo "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
            echo "::error::Write the version as three numbers, for example 1.2.0 (no v in front)"
            exit 1
          fi
          if git ls-remote --exit-code --tags origin "refs/tags/v$VERSION" > /dev/null; then
            echo "::error::Version $VERSION exists already. Pick a higher number"
            exit 1
          fi
          if ! grep -q '^## Version:' "$ADDON.toc"; then
            echo "::error::$ADDON.toc has no '## Version:' line, or ADDON in this file is not the name of the .toc"
            exit 1
          fi

      - name: Set the version in the .toc
        run: perl -pi -e 's/^## Version:[^\r\n]*/## Version: $ENV{VERSION}/' "$ADDON.toc"

      - name: Commit, tag and push
        run: |
          git config user.name "github-actions[bot]"
          git config user.email "41898283+github-actions[bot]@users.noreply.github.com"
          git commit -am "Release v$VERSION" || echo "The .toc already had version $VERSION"
          git tag "v$VERSION"
          git push origin HEAD "v$VERSION"

      - name: Build the zip
        run: git archive --format=zip --prefix="$ADDON/" -o "$ADDON.zip" "v$VERSION"

      - name: Create the release
        env:
          GH_TOKEN: ${{ github.token }}
        run: gh release create "v$VERSION" "$ADDON.zip" --title "$ADDON v$VERSION" --generate-notes
```

Then keep the workflow itself and other files out of the zip. Create one more file the same way,
named `.gitattributes`, with:

```text
.github export-ignore
.gitattributes export-ignore
```

Add a line `<name> export-ignore` for every other file or folder players do not need, for
example `screenshots export-ignore`.

## 6. Publish a new version with one button

1. Commit and push your changes first (step 4). The release is made from what is on GitHub.
2. On GitHub, open your repo and click the **Actions** tab.
3. Click **Release** in the list on the left.
4. Click **Run workflow** on the right, type the new version, for example `1.2.0`, and click the
   green **Run workflow** button.
5. After about half a minute the run shows a green tick. Your repo now has a release `v1.2.0`
   with `MyAddon.zip`, and the `.toc` says `## Version: 1.2.0`.
6. In GitHub Desktop, click **Fetch origin** and then **Pull origin**, so your computer gets the
   commit the workflow made. Do this before your next change.

Picking the number: three numbers, `major.minor.fix`. Raise the last one for bug fixes
(`1.2.0` → `1.2.1`), the middle one for new features (`1.2.1` → `1.3.0`), the first one for big
changes that break settings or old behavior (`1.3.0` → `2.0.0`). A version can only be used once.

The release notes list the commits since the previous release, so short, clear commit messages
pay off. You can edit the notes on the release page afterwards.

## 7. Publish the addon on SurvivorsForge

You do this once. Later releases come in on their own.

1. Open <https://forge.rezondes.net> and click **Log in with GitHub**.
2. Optional but recommended: before the first release, try **My addons → Test a zip** with the
   zip from your release. It runs the same checks as the platform, without publishing anything.
3. Click **Publish an addon**.
4. **Your repo**: type the repo name and pick it from the list. The form fills itself in from
   the newest release.
5. Check the fields:

   | Field | What to enter |
   |---|---|
   | Name | The name players see |
   | Address on this site | Lowercase letters, digits and dashes, e.g. `myaddon`. It cannot be changed later. |
   | Short description | One sentence for lists and search results |
   | Category | Where players find it |
   | Downloads come from | **A zip attached to each release** (what the workflow makes) |
   | Release zip name | `MyAddon.zip`, the name of the zip in your release. `*` stands for a version number if your zip name contains one |
   | Addon folders in the zip | `MyAddon`, one folder per line |

6. Write the description (Markdown, with a preview), add an icon and screenshots if you like,
   and click **Publish**.
7. Install the **SurvivorsForge App** on your repo when the page offers it. With the App, new
   releases appear on the platform right away; without it, they appear a while later when the
   platform checks again.

What happens to every release:

1. The platform downloads the zip and scans its Lua, XML and files for harmful code.
2. A clean release counts as **Checked**. If the scan finds something, the release is **Flagged**
   and an admin looks at it. Admins can mark a release as **Verified** or block it.
3. The launcher installs checked and verified releases directly and asks the player first for
   flagged ones. It also checks that the file on GitHub is exactly the one that was scanned:
   do not replace the zip of a published release. Make a new version instead.

On your addon's edit pages you can also:

- set per version which game versions it supports (**Versions**),
- name libraries or other addons it needs (**Dependencies**; required ones are installed with it),
- let other GitHub users edit the description or images (**Contributors**).

## 8. When something goes wrong

| Problem | Fix |
|---|---|
| No **Run workflow** button | The file must be at exactly `.github/workflows/release.yml` on the `main` branch. Reload the Actions page. |
| Red cross, "Write the version as three numbers" | Type `1.2.0`, not `v1.2.0` or `1.2`. |
| Red cross, "exists already" | That version was released before. Pick a higher number. |
| Red cross, "has no '## Version:' line" | Add `## Version: 1.0.0` to the `.toc`, or fix the name in `ADDON:`. |
| Red cross at "Commit, tag and push" with "Permission denied" or "403" | Repo **Settings → Actions → General → Workflow permissions**: choose **Read and write permissions** and save. If the `main` branch is protected, allow GitHub Actions to push or remove the protection. |
| GitHub Desktop says your branch and the remote have diverged | You committed before pulling the workflow's commit. Click **Pull origin**; GitHub Desktop merges both. |
| A release does not show up on the platform | **Release zip name** on the platform must match the zip name in the release, e.g. `MyAddon.zip`; releases without a matching zip are skipped. Without the SurvivorsForge App on the repo, wait a while, or click **Force Refresh** under **My addons**. |
| The release is flagged | Open the version on the platform to see what the scan found. Remove the code or file, release a new version, or wait for an admin. |
