//! Rules for WoW frame XML: Lua inside `<Script>` and `On...` handlers gets the Lua rules with the
//! line numbers of the XML file; `<Script file>` must point at a file in the zip; no DTDs.

use crate::finding::{Report, Severity};
use crate::lua_rules;
use crate::zip_rules::{normal_path, resolve};
use quick_xml::Reader;
use quick_xml::events::Event;

fn line_at(source: &[u8], pos: usize) -> u32 {
    source[..pos.min(source.len())].iter().filter(|b| **b == b'\n').count() as u32 + 1
}

/// Element names without a namespace prefix, in any case: a scanner that is stricter than the
/// client about case would skip code the client runs.
fn is_script_element(name: &[u8], in_scripts: bool) -> bool {
    name.eq_ignore_ascii_case(b"Script") || (in_scripts && name.len() > 2 && name[..2].eq_ignore_ascii_case(b"On"))
}

/// The lowercase zip paths `path` loads through `<Script file>` and `<Include file>`.
pub fn referenced_files(path: &str, source: &[u8]) -> Vec<String> {
    let dir = path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
    let mut reader = Reader::from_reader(source);
    let mut buf = Vec::new();
    let mut files = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.local_name().as_ref().eq_ignore_ascii_case(b"Script") || e.local_name().as_ref().eq_ignore_ascii_case(b"Include") =>
            {
                if let Some(file) = e.try_get_attribute("file").ok().flatten() {
                    files.extend(resolve(&dir, &String::from_utf8_lossy(&file.value)));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    files
}

/// Checks the XML file `path`. `zip_files` are all paths in the zip, for `<Script file>`. Returns
/// the Lua of its handlers with their line offsets, for the rules across files.
pub fn check(path: &str, source: &[u8], zip_files: &[String], report: &mut Report) -> Vec<(String, u32)> {
    let mut scripts = Vec::new();
    let mut reader = Reader::from_reader(source);
    let dir = path.rsplit_once('/').map(|(d, _)| format!("{d}/")).unwrap_or_default();
    let mut in_scripts = 0usize;
    // (element, offset of its text) while inside a script element.
    let mut script: Option<(Vec<u8>, usize, String)> = None;
    let mut buf = Vec::new();
    loop {
        let before = reader.buffer_position() as usize;
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(e) => {
                report.add("xml.parse_error", Severity::Warn, path, Some(line_at(source, before)), format!("The XML cannot be read: {e}"));
                return scripts;
            }
        };
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Eof => break,
            Event::DocType(_) => {
                report.add("xml.doctype", Severity::Warn, path, Some(line_at(source, before)), "DOCTYPE and entity declarations are not allowed");
            }
            Event::Start(e) | Event::Empty(e) => {
                let name = e.local_name().as_ref().to_vec();
                if let Some(file) = e.try_get_attribute("file").ok().flatten().filter(|_| name.eq_ignore_ascii_case(b"Script")) {
                    let file = String::from_utf8_lossy(&file.value).replace('\\', "/");
                    if let Some(wanted) = resolve(&dir, &file)
                        && !zip_files.iter().any(|f| normal_path(f) == wanted)
                    {
                        report.add_with_snippet("xml.missing_script_file", Severity::Warn, path, Some(line_at(source, before)),
                            "<Script file> points at a file that is not in the zip", &file);
                    }
                }
                if empty {
                    buf.clear();
                    continue;
                }
                // `<Script file="a.lua">code</Script>`: the code between the tags is scanned too.
                if name.eq_ignore_ascii_case(b"Scripts") {
                    in_scripts += 1;
                }
                if script.is_none() && is_script_element(&name, in_scripts > 0) {
                    script = Some((name, reader.buffer_position() as usize, String::new()));
                }
            }
            Event::Text(t) => {
                if let Some((_, _, text)) = &mut script {
                    text.push_str(&t.unescape().map(|d| d.into_owned()).unwrap_or_else(|_| String::from_utf8_lossy(&t).into_owned()));
                }
            }
            Event::CData(c) => {
                if let Some((_, _, text)) = &mut script {
                    text.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Event::End(e) => {
                let name = e.local_name().as_ref().to_vec();
                if script.as_ref().is_some_and(|(n, _, _)| *n == name) {
                    let (_, start, text) = script.take().unwrap();
                    lua_rules::check(path, text.as_bytes(), line_at(source, start) - 1, report);
                    scripts.push((text, line_at(source, start) - 1));
                }
                if name.eq_ignore_ascii_case(b"Scripts") {
                    in_scripts = in_scripts.saturating_sub(1);
                }
            }
            _ => {}
        }
        buf.clear();
    }
    scripts
}
