mod common;

use common::make_zip;
use forge_core::archive::{analyze, extract_addons};
use forge_core::Error;

fn names(zip: &std::path::Path) -> Vec<String> {
    analyze(zip).unwrap().into_iter().map(|a| a.name).collect()
}

#[test]
fn single_addon_folder() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("LuckyOdds/", ""),
        ("LuckyOdds/LuckyOdds.toc", "## Version: 0.6.0"),
        ("LuckyOdds/LuckyOdds.lua", "x"),
    ]);
    let found = analyze(&zip).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "LuckyOdds");
    assert_eq!(found[0].prefix, "LuckyOdds");
    assert_eq!(found[0].toc.version.as_deref(), Some("0.6.0"));
}

#[test]
fn two_addons_and_root_file() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("LICENSE", "mit"),
        ("SurvivorPlates/SurvivorPlates.toc", ""),
        ("SurvivorUI/SurvivorUI.toc", ""),
        ("SurvivorUI/Art/x.tga", "x"),
    ]);
    assert_eq!(names(&zip), ["SurvivorPlates", "SurvivorUI"]);
}

#[test]
fn backslash_paths_are_normalized() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("BordoUI\\BordoUI.toc", ""),
        ("BordoUI\\Art\\Coin.tga", "c"),
    ]);
    assert_eq!(names(&zip), ["BordoUI"]);
    let out = tempfile::tempdir().unwrap();
    extract_addons(&zip, &analyze(&zip).unwrap(), out.path()).unwrap();
    assert!(out.path().join("BordoUI/Art/Coin.tga").is_file());
}

#[test]
fn github_source_zip_uses_toc_stem() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("threenox-BordoUI-abc123/", ""),
        ("threenox-BordoUI-abc123/BordoUI.toc", ""),
        ("threenox-BordoUI-abc123/Core.lua", "x"),
        ("threenox-BordoUI-abc123/Libs/Lib/Lib.toc", ""),
    ]);
    let found = analyze(&zip).unwrap();
    assert_eq!(found.len(), 1, "nested library toc does not count");
    assert_eq!(found[0].name, "BordoUI");
    let out = tempfile::tempdir().unwrap();
    extract_addons(&zip, &found, out.path()).unwrap();
    assert!(out.path().join("BordoUI/Core.lua").is_file());
    assert!(out.path().join("BordoUI/Libs/Lib/Lib.toc").is_file());
}

#[test]
fn client_variant_tocs_prefer_vanilla() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("pfUI-abc/pfUI-tbc.toc", "## Interface: 20400"),
        ("pfUI-abc/pfUI-wotlk.toc", "## Interface: 30300"),
        ("pfUI-abc/pfUI.toc", "## Interface: 11200"),
    ]);
    assert_eq!(names(&zip), ["pfUI"]);
}

#[test]
fn flat_zip_with_toc_at_root() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[("Tickfire.toc", ""), ("Tickfire.lua", "x")]);
    let found = analyze(&zip).unwrap();
    assert_eq!(found[0].name, "Tickfire");
    let out = tempfile::tempdir().unwrap();
    extract_addons(&zip, &found, out.path()).unwrap();
    assert!(out.path().join("Tickfire/Tickfire.lua").is_file());
}

#[test]
fn zip_slip_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[("A/A.toc", ""), ("../evil.lua", "x")]);
    assert!(matches!(analyze(&zip), Err(Error::UnsafePath(_))));
    let zip = make_zip(dir.path(), "b.zip", &[("A/A.toc", ""), ("C:/evil.lua", "x")]);
    assert!(matches!(analyze(&zip), Err(Error::UnsafePath(_))));
}

#[test]
fn no_addon_found() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[("README.md", "hi"), ("src/main.rs", "")]);
    assert_eq!(analyze(&zip), Err(Error::NoAddonFound));
}

#[test]
fn extract_writes_only_addon_folders() {
    let dir = tempfile::tempdir().unwrap();
    let zip = make_zip(dir.path(), "a.zip", &[
        ("LICENSE", "mit"),
        ("installer/setup.exe", "x"),
        ("SurvivorUI/SurvivorUI.toc", ""),
    ]);
    let out = tempfile::tempdir().unwrap();
    extract_addons(&zip, &analyze(&zip).unwrap(), out.path()).unwrap();
    let entries: Vec<_> = std::fs::read_dir(out.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(entries, ["SurvivorUI"]);
}

#[test]
fn toc_larger_than_the_limit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let big = format!("## Interface: 11200\n{}", " ".repeat(forge_core::archive::MAX_TOC_BYTES as usize));
    let zip = make_zip(dir.path(), "a.zip", &[("X/X.toc", big.as_str())]);
    assert!(matches!(analyze(&zip), Err(Error::TocTooLarge(p)) if p == "X/X.toc"), "{:?}", analyze(&zip).map(|a| a.len()));
}

#[test]
fn big_but_normal_toc_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let lines: String = (0..500).map(|i| format!("Modules/Module{i:03}.lua\n")).collect();
    let toc = format!("## Interface: 11200\n## Title: Big\n{lines}");
    let zip = make_zip(dir.path(), "a.zip", &[("Big/Big.toc", toc.as_str())]);
    let found = analyze(&zip).unwrap();
    assert_eq!(found[0].name, "Big");
}
