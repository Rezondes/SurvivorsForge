mod common;

use common::make_zip;
use std::path::{Path, PathBuf};
use survivorsforge::install::{install_zip, install_zip_with, uninstall, work_dir, Request};
use survivorsforge::state::{Source, State};
use survivorsforge::Error;

struct Env {
    _root: tempfile::TempDir,
    addons: PathBuf,
    zips: PathBuf,
}

fn env() -> Env {
    let root = tempfile::tempdir().unwrap();
    let addons = root.path().join("AddOns");
    let zips = root.path().join("zips");
    std::fs::create_dir_all(&addons).unwrap();
    std::fs::create_dir_all(&zips).unwrap();
    Env { addons, zips, _root: root }
}

fn request<'a>(zip: &'a Path, id: &'a str) -> Request<'a> {
    Request { zip, id, name: id, source: Source::Zip { file_name: "x.zip".into() }, expected_sha256: None, version: "v1".into() }
}

fn staging_is_empty(addons: &Path) -> bool {
    let staging = work_dir(addons).join("staging");
    !staging.exists() || std::fs::read_dir(staging).unwrap().count() == 0
}

#[test]
fn fresh_install_creates_folders_and_state() {
    let e = env();
    let zip = make_zip(&e.zips, "ui.zip", &[("SurvivorUI/SurvivorUI.toc", ""), ("SurvivorPlates/SurvivorPlates.toc", "")]);
    let mut state = State::default();
    let entry = install_zip(&e.addons, &mut state, request(&zip, "ui")).unwrap();
    assert!(e.addons.join("SurvivorUI/SurvivorUI.toc").is_file());
    assert!(e.addons.join("SurvivorPlates/SurvivorPlates.toc").is_file());
    assert_eq!(entry.folders, ["SurvivorPlates", "SurvivorUI"]);
    assert_eq!(state.get("ui").unwrap().version, "v1");
    assert!(staging_is_empty(&e.addons));
}

#[test]
fn replaces_folder_with_different_case() {
    let e = env();
    std::fs::create_dir_all(e.addons.join("luckyodds")).unwrap();
    std::fs::write(e.addons.join("luckyodds/luckyodds.toc"), "old").unwrap();
    std::fs::write(e.addons.join("luckyodds/old.lua"), "x").unwrap();
    let zip = make_zip(&e.zips, "a.zip", &[("LuckyOdds/LuckyOdds.toc", "new")]);
    install_zip(&e.addons, &mut State::default(), request(&zip, "lucky")).unwrap();
    let folders: Vec<String> = survivorsforge::scan::scan_addons(&e.addons).unwrap().into_iter().map(|a| a.folder).collect();
    assert_eq!(folders, ["LuckyOdds"], "no second folder next to the old one");
    assert!(!e.addons.join("LuckyOdds/old.lua").exists());
}

#[test]
fn work_dir_lives_in_addons_and_is_never_listed() {
    let e = env();
    assert!(work_dir(&e.addons).starts_with(&e.addons), "same file system as the addons");
    let zip = make_zip(&e.zips, "a.zip", &[("A/A.toc", "")]);
    install_zip(&e.addons, &mut State::default(), request(&zip, "a")).unwrap();
    std::fs::create_dir_all(work_dir(&e.addons).join("downloads")).unwrap();
    let folders: Vec<String> = survivorsforge::scan::scan_addons(&e.addons).unwrap().into_iter().map(|a| a.folder).collect();
    assert_eq!(folders, ["A"]);
}

#[test]
fn update_removes_stale_files() {
    let e = env();
    let mut state = State::default();
    let v1 = make_zip(&e.zips, "v1.zip", &[("A/A.toc", "1"), ("A/old.lua", "x")]);
    install_zip(&e.addons, &mut state, request(&v1, "a")).unwrap();
    let v2 = make_zip(&e.zips, "v2.zip", &[("A/A.toc", "2"), ("A/new.lua", "x")]);
    install_zip(&e.addons, &mut state, Request { expected_sha256: None, version: "v2".into(), ..request(&v2, "a") }).unwrap();
    assert!(!e.addons.join("A/old.lua").exists());
    assert!(e.addons.join("A/new.lua").is_file());
    assert_eq!(state.entries.len(), 1);
    assert_eq!(state.get("a").unwrap().version, "v2");
    assert!(staging_is_empty(&e.addons));
}

#[test]
fn failed_move_rolls_back() {
    let e = env();
    let mut state = State::default();
    let v1 = make_zip(&e.zips, "v1.zip", &[("A/A.toc", "1"), ("A/old.lua", "x")]);
    install_zip(&e.addons, &mut state, request(&v1, "a")).unwrap();
    let v2 = make_zip(&e.zips, "v2.zip", &[("A/A.toc", "2"), ("B/B.toc", "")]);
    let mut calls = 0;
    let result = install_zip_with(&e.addons, &mut state, request(&v2, "a"), &mut |from, to| {
        calls += 1;
        if calls == 3 {
            return Err(std::io::Error::other("disk full"));
        }
        std::fs::rename(from, to)
    });
    assert!(result.is_err());
    assert!(e.addons.join("A/old.lua").is_file(), "old version restored");
    assert!(!e.addons.join("B").exists(), "half-moved new folder removed");
    assert_eq!(state.get("a").unwrap().version, "v1");
    assert!(staging_is_empty(&e.addons));
}

#[test]
fn refuses_folder_owned_by_other_addon() {
    let e = env();
    let mut state = State::default();
    let a = make_zip(&e.zips, "a.zip", &[("Shared/Shared.toc", "")]);
    install_zip(&e.addons, &mut state, request(&a, "first")).unwrap();
    let b = make_zip(&e.zips, "b.zip", &[("Shared/Shared.toc", "other")]);
    assert_eq!(
        install_zip(&e.addons, &mut state, request(&b, "second")),
        Err(Error::FolderOwnedBy("first".into()))
    );
    assert_eq!(std::fs::read_to_string(e.addons.join("Shared/Shared.toc")).unwrap(), "");
}

#[test]
fn trailing_dot_alias_of_an_owned_folder_is_refused() {
    // Windows opens "Shared." as the folder "Shared" of the first addon.
    let e = env();
    let mut state = State::default();
    let a = make_zip(&e.zips, "a.zip", &[("Shared/Shared.toc", "")]);
    install_zip(&e.addons, &mut state, request(&a, "first")).unwrap();
    assert_eq!(state.owner_of("shared.").map(|o| o.id.as_str()), Some("first"));
    let b = make_zip(&e.zips, "b.zip", &[("Shared./Shared..toc", "other")]);
    assert!(install_zip(&e.addons, &mut state, request(&b, "second")).is_err());
    assert_eq!(std::fs::read_to_string(e.addons.join("Shared/Shared.toc")).unwrap(), "");
}

#[test]
fn windows_unsafe_folder_is_not_installed() {
    let e = env();
    let mut state = State::default();
    let zip = make_zip(&e.zips, "a.zip", &[("NUL/NUL.toc", "")]);
    assert!(matches!(install_zip(&e.addons, &mut state, request(&zip, "nul")), Err(Error::NotInstallable(_))));
    assert!(state.get("nul").is_none());
}

#[test]
fn uninstall_removes_folders_and_entry() {
    let e = env();
    let mut state = State::default();
    let zip = make_zip(&e.zips, "ui.zip", &[("X/X.toc", ""), ("Y/Y.toc", "")]);
    install_zip(&e.addons, &mut state, request(&zip, "ui")).unwrap();
    let mut deleted = Vec::new();
    uninstall(&e.addons, &mut state, "ui", &mut |p| {
        deleted.push(p.file_name().unwrap().to_string_lossy().into_owned());
        std::fs::remove_dir_all(p).map_err(Into::into)
    })
    .unwrap();
    assert_eq!(deleted, ["X", "Y"]);
    assert!(!e.addons.join("X").exists());
    assert!(state.get("ui").is_none());
}

#[test]
fn hash_mismatch_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let addons = dir.path().join("AddOns");
    std::fs::create_dir_all(&addons).unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[("Lucky/", ""), ("Lucky/Lucky.toc", "## Interface: 11200")]);
    let mut state = State::default();
    let request = Request {
        zip: &zip,
        id: "lucky",
        name: "Lucky",
        source: Source::Verified { registry_id: "lucky".into() },
        version: "v1".into(),
        expected_sha256: Some("00".repeat(32)),
    };
    let result = install_zip(&addons, &mut state, request);
    assert!(matches!(result, Err(Error::HashMismatch { .. })), "{result:?}");
    assert!(!addons.join("Lucky").exists());
    assert!(state.entries.is_empty());
    assert!(!zip.exists(), "the wrong file is deleted");
}

#[test]
fn matching_hash_installs() {
    let dir = tempfile::tempdir().unwrap();
    let addons = dir.path().join("AddOns");
    std::fs::create_dir_all(&addons).unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[("Lucky/", ""), ("Lucky/Lucky.toc", "## Interface: 11200")]);
    let sha = survivorsforge::install::sha256_of(&zip).unwrap();
    let mut state = State::default();
    let request = Request {
        zip: &zip,
        id: "lucky",
        name: "Lucky",
        source: Source::Verified { registry_id: "lucky".into() },
        version: "v1".into(),
        expected_sha256: Some(sha.to_uppercase()),
    };
    install_zip(&addons, &mut state, request).unwrap();
    assert!(addons.join("Lucky/Lucky.toc").exists());
}
