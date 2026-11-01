use forge_scan::{Report, lua_rules};

/// Run in CI with `cargo test -p forge-scan --release -- --ignored`.
#[test]
#[ignore]
fn large_file_is_fast() {
    let chunk = "local frame = CreateFrame(\"Frame\")\nframe:SetScript(\"OnEvent\", function() print(arg1) end)\n";
    let source = chunk.repeat(5 * 1024 * 1024 / chunk.len());
    let started = std::time::Instant::now();
    let mut report = Report::default();
    lua_rules::check("big.lua", source.as_bytes(), 0, &mut report);
    // About 1 s on a desktop and up to 2 s on a shared CI runner; a regex that backtracks across
    // the file takes minutes, so the margin stays wide without hiding one.
    assert!(started.elapsed().as_secs_f64() < 5.0, "took {:?}", started.elapsed());
}

/// Run in CI with `cargo test -p forge-scan --release -- --ignored`.
#[test]
#[ignore]
fn deep_inputs_are_fast() {
    for (open, close) in [("(", ")"), ("{", "}"), ("function() return ", " end"), ("not ", ""), (" .. a", "")] {
        let source = format!("x = {}a{}", open.repeat(1_000_000), close.repeat(1_000_000));
        let started = std::time::Instant::now();
        let mut report = Report::default();
        lua_rules::check("deep.lua", source.as_bytes(), 0, &mut report);
        assert!(report.findings.iter().any(|f| f.rule == "lua.too_deep"), "{open}");
        assert!(started.elapsed().as_secs_f64() < 5.0, "{open} took {:?}", started.elapsed());
    }
}
