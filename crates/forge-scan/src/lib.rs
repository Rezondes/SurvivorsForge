//! Static security scanner for addon zips. Pure functions on bytes: no files, no network.
//!
//! A clean report means "nothing found", never "safe".

mod entropy;
pub mod finding;
mod hint;
mod lua50;
mod lua_flow;
pub mod lua_rules;
mod scope;
pub mod xml_rules;
pub mod zip_rules;

pub use finding::{Finding, Report, Severity};
pub use hint::hint;

/// The stack a scan needs: full_moon parses recursively, up to `lua_rules::MAX_NESTING` levels
/// deep. Run `scan` on a thread with this much stack.
pub const SCAN_STACK_BYTES: usize = 64 * 1024 * 1024;

/// The version of the rules. A new version lets the platform rescan everything.
pub const SCANNER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Scans `zip` for an addon that registered `expected_folders`. `foreign_folders` belong to other addons.
///
/// The game runs only what a `.toc` loads, directly or through XML. Findings in other Lua and XML
/// files (tests, build scripts of the author) are kept as `Info`: the files sit in the folder but
/// never run. The files the game loads share one global table, so the rules across files run
/// over all of them together.
pub fn scan(zip: &[u8], expected_folders: &[String], foreign_folders: &[String]) -> Report {
    let mut report = Report::default();
    let texts = zip_rules::check(zip, expected_folders, foreign_folders, &mut report);
    let paths: Vec<String> = texts.iter().map(|t| t.path.clone()).collect();
    // After a bomb the raw zip is not read again; everything counts as loaded.
    let bomb = report.findings.iter().any(|f| f.rule == "zip.bomb" && f.severity == Severity::Critical);
    let loaded = if bomb { None } else { zip_rules::loaded_files(&texts) };
    let mut units = Vec::new();
    for text in &texts {
        let mut found = Report::default();
        let runs = !loaded.as_ref().is_some_and(|l| !l.contains(&text.path.to_ascii_lowercase()));
        match text.path.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).as_deref() {
            Some("lua") => {
                lua_rules::check(&text.path, &text.bytes, 0, &mut found);
                units.extend(lua_rules::unit(&text.path, &text.bytes, 0).filter(|_| runs));
            }
            Some("xml") => {
                for (code, offset) in xml_rules::check(&text.path, &text.bytes, &paths, &mut found) {
                    units.extend(lua_rules::unit(&text.path, code.as_bytes(), offset).filter(|_| runs));
                }
            }
            _ => {}
        }
        if !runs {
            for f in &mut found.findings {
                f.severity = f.severity.min(Severity::Info);
                f.message.push_str(" (in a file the game does not load)");
            }
        }
        report.findings.append(&mut found.findings);
    }
    lua_flow::check(&units, &mut report);
    report.dedupe();
    report
}
