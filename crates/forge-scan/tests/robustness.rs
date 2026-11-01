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
