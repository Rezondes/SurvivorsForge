//! Inputs that must never make the scanner panic: a panic in the worker stops every job.

mod common;

use common::{folders, zip};
use forge_scan::scan;

#[test]
fn non_ascii_name_before_the_toc_does_not_panic() {
    // "aaé.lua" is cut at byte 3 when compared with the folder "AB/".
    let addon = zip(&[("aa\u{e9}.lua", b"x = 1\n"), ("AB/AB.toc", b"## Interface: 11200\nA.lua\n")]);
    scan(&addon, &folders(&["AB"]), &[]);
}

#[test]
fn names_with_multibyte_characters_at_every_position() {
    for ch in ["\u{e9}", "\u{65e5}", "\u{1f600}"] {
        for at in 0..8 {
            let name: String = "abcdefgh".chars().take(at).chain(ch.chars()).chain("xy.lua".chars()).collect();
            let dir = format!("{}{ch}", "Ab".chars().take(at.min(2)).collect::<String>());
            let toc = format!("{dir}/{dir}.toc");
            let lua = format!("{dir}/{name}");
            let entries: Vec<(&str, &[u8])> =
                vec![(name.as_str(), b"x = 1\n"), (toc.as_str(), b"## Interface: 11200\nx.lua\n"), (lua.as_str(), b"os.exit()\n")];
            scan(&zip(&entries), &folders(&[dir.as_str()]), &folders(&["Other"]));
        }
    }
}

/// Each construct nested or chained `n` times.
fn nested(kind: &str, n: usize) -> String {
    match kind {
        "paren" => format!("x = {}1{}", "(".repeat(n), ")".repeat(n)),
        "table" => format!("x = {}{}", "{".repeat(n), "}".repeat(n)),
        "function" => format!("x = {}1{}", "function() return ".repeat(n), " end".repeat(n)),
        "if" => format!("{}x = 1{}", "if a then ".repeat(n), " end".repeat(n)),
        "not" => format!("x = {}a", "not ".repeat(n)),
        "minus" => format!("x = {}a", "- ".repeat(n)),
        "concat" => format!("x = a{}", " .. a".repeat(n)),
        "pow" => format!("x = a{}", " ^ a".repeat(n)),
        "plus" => format!("x = a{}", " + a".repeat(n)),
        _ => unreachable!(),
    }
}

const KINDS: &[&str] = &["paren", "table", "function", "if", "not", "minus", "concat", "pow", "plus"];

fn rules(src: &str) -> Vec<(String, forge_scan::Severity)> {
    let mut report = forge_scan::Report::default();
    forge_scan::lua_rules::check("A/a.lua", src.as_bytes(), 0, &mut report);
    report.findings.into_iter().map(|f| (f.rule, f.severity)).collect()
}

#[test]
fn deeply_nested_lua_is_reported_not_parsed() {
    // On the 2 MiB stack of a test thread: without the limit, full_moon overflows it.
    for kind in KINDS {
        let found = rules(&nested(kind, 1_000_000));
        assert!(found.contains(&("lua.too_deep".into(), forge_scan::Severity::Critical)), "{kind}: {found:?}");
    }
}

#[test]
fn code_below_the_limit_is_still_checked() {
    // Code just below the limit is parsed; it needs the stack the API and the CLI give the scan.
    let checked = std::thread::Builder::new()
        .stack_size(forge_scan::SCAN_STACK_BYTES)
        .spawn(|| {
            for kind in KINDS {
                let src = format!("{}\nos.exit()", nested(kind, forge_scan::lua_rules::MAX_NESTING - 10));
                let found = rules(&src);
                assert!(found.iter().all(|(r, _)| r != "lua.too_deep"), "{kind}: {found:?}");
                assert!(found.contains(&("lua.forbidden_global".into(), forge_scan::Severity::Critical)), "{kind}: {found:?}");
            }
        })
        .unwrap()
        .join();
    assert!(checked.is_ok());
}

#[test]
fn forbidden_names_in_too_deep_code_are_still_found() {
    let src = format!("x = {}os.exit(){}", "(".repeat(5000), ")".repeat(5000));
    let found = rules(&src);
    assert!(found.contains(&("lua.too_deep".into(), forge_scan::Severity::Critical)), "{found:?}");
    assert!(found.contains(&("lua.forbidden_global".into(), forge_scan::Severity::Critical)), "{found:?}");
}
