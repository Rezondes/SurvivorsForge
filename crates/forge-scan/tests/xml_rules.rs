mod common;

use common::{folders, zip};
use forge_scan::{Report, Severity, scan, xml_rules};

fn check(src: &str, files: &[&str]) -> Report {
    let mut report = Report::default();
    let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
    xml_rules::check("Lucky/Lucky.xml", src.as_bytes(), &files, &mut report);
    report
}

#[test]
fn inline_script_line_numbers() {
    let mut xml = String::from("<Ui>\n");
    for _ in 0..9 {
        xml.push_str("  <!-- filler -->\n");
    }
    // Line 11 opens the frame, line 12 the handler, line 13 holds the call.
    xml.push_str("  <Frame name=\"F\"><Scripts>\n    <OnLoad>\n      os.exit()\n    </OnLoad>\n  </Scripts></Frame>\n</Ui>\n");
    let report = check(&xml, &[]);
    let f = report.findings.iter().find(|f| f.rule == "lua.forbidden_global").expect("found");
    assert_eq!((f.line, f.file.as_str()), (Some(13), "Lucky/Lucky.xml"));
}

#[test]
fn script_element_and_entities() {
    let report = check("<Ui><Script>if a &lt; b then loadstring(c)() end</Script></Ui>", &[]);
    assert!(report.findings.iter().any(|f| f.rule == "lua.dynamic_code" && f.severity == Severity::Warn), "{:#?}", report.findings);
    assert!(report.findings.iter().all(|f| f.rule != "lua.parse_error"), "entities resolved before parsing");
}

#[test]
fn inline_code_next_to_a_file_and_any_case_is_scanned() {
    let report = check("<Ui><Script file=\"Lucky.lua\">os.exit()</Script></Ui>", &["Lucky/Lucky.lua"]);
    assert!(report.findings.iter().any(|f| f.rule == "lua.forbidden_global"), "{:#?}", report.findings);
    let report = check("<Ui><Frame name=\"F\"><scripts><onevent>os.exit()</onevent></scripts></Frame>\n<script>io.open('x')</script></Ui>", &[]);
    assert_eq!(report.findings.iter().filter(|f| f.rule == "lua.forbidden_global").count(), 2, "{:#?}", report.findings);
}

#[test]
fn doctype_warns() {
    let report = check("<!DOCTYPE x [<!ENTITY e SYSTEM \"file:///etc/passwd\">]><Ui>&e;</Ui>", &[]);
    assert!(report.findings.iter().any(|f| f.rule == "xml.doctype" && f.severity == Severity::Warn));
}

#[test]
fn missing_script_file_warns() {
    let report = check("<Ui><Script file=\"Gone.lua\"/><Script file=\"Libs\\Here.lua\"/></Ui>", &["Lucky/Libs/Here.lua"]);
    let hits: Vec<_> = report.findings.iter().filter(|f| f.rule == "xml.missing_script_file").collect();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].snippet.as_deref(), Some("Gone.lua"));
}

#[test]
fn parse_error_warns() {
    let report = check("<Ui><Frame></Ui>", &[]);
    assert!(report.findings.iter().any(|f| f.rule == "xml.parse_error"));
}

#[test]
fn clean_frame_xml_has_no_findings() {
    let xml = "<Ui><Script file=\"Lucky.lua\"/><Frame name=\"LuckyFrame\"><Scripts><OnShow>this:SetAlpha(1)</OnShow></Scripts></Frame></Ui>";
    let report = check(xml, &["Lucky/Lucky.lua"]);
    assert!(report.findings.is_empty(), "{:#?}", report.findings);
}

#[test]
fn scan_runs_lua_and_xml_rules_on_the_zip() {
    let bytes = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\nLucky.lua\nLucky.xml\n"),
        ("Lucky/Lucky.lua", b"io.write('x')"),
        ("Lucky/Lucky.xml", b"<Ui><Frame><Scripts><OnLoad>SendChatMessage('hi')</OnLoad></Scripts></Frame></Ui>"),
    ]);
    let report = scan(&bytes, &folders(&["Lucky"]), &[]);
    let rules: Vec<&str> = report.findings.iter().map(|f| f.rule.as_str()).collect();
    assert!(rules.contains(&"lua.forbidden_global"), "{rules:?}");
    assert!(rules.contains(&"lua.chat_send"), "{rules:?}");
}
