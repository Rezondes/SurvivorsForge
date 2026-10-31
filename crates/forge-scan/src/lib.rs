//! Static security scanner for addon zips. Pure functions on bytes: no files, no network.
//!
//! A clean report means "nothing found", never "safe".

mod entropy;
pub mod finding;
mod hint;
pub mod lua_rules;
pub mod xml_rules;
pub mod zip_rules;

pub use finding::{Finding, Report, Severity};
pub use hint::hint;

/// The version of the rules. A new version lets the platform rescan everything.
pub const SCANNER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Scans `zip` for an addon that registered `expected_folders`. `foreign_folders` belong to other addons.
///
/// The game runs only what a `.toc` loads, directly or through XML. Findings in other Lua and XML
/// files (tests, build scripts of the author) are kept as `Info`: the files sit in the folder but
/// never run.
pub fn scan(zip: &[u8], expected_folders: &[String], foreign_folders: &[String]) -> Report {
    let mut report = Report::default();
    let texts = zip_rules::check(zip, expected_folders, foreign_folders, &mut report);
    let paths: Vec<String> = texts.iter().map(|t| t.path.clone()).collect();
    let loaded = zip_rules::loaded_files(zip, &texts);
    for text in &texts {
        let mut found = Report::default();
        match text.path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
            Some("lua") => lua_rules::check(&text.path, &text.bytes, 0, &mut found),
            Some("xml") => xml_rules::check(&text.path, &text.bytes, &paths, &mut found),
            _ => {}
        }
        if loaded.as_ref().is_some_and(|l| !l.contains(&text.path.to_ascii_lowercase())) {
            for f in &mut found.findings {
                f.severity = f.severity.min(Severity::Info);
                f.message.push_str(" (in a file the game does not load)");
            }
        }
        report.findings.append(&mut found.findings);
    }
    report
}
