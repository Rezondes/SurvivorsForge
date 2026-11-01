//! The rules that follow values through a file and an addon: aliases, names built from strings,
//! text from chat, and globals one file sets and another uses.

mod common;

use common::{folders, zip};
use forge_scan::{Report, Severity, lua_rules, scan};

fn rules(src: &str) -> Vec<(String, Severity, Option<u32>)> {
    let mut report = Report::default();
    lua_rules::check("A/a.lua", src.as_bytes(), 0, &mut report);
    report.findings.into_iter().map(|f| (f.rule, f.severity, f.line)).collect()
}

fn has(src: &str, rule: &str, severity: Severity, line: u32) {
    let found = rules(src);
    assert!(found.contains(&(rule.into(), severity, Some(line))), "{rule}/{severity:?} on {line} not in {found:?} for {src}");
}

fn lacks_at_least(src: &str, rule: &str, severity: Severity) {
    let found = rules(src);
    assert!(found.iter().all(|(r, s, _)| r != rule || *s < severity), "unexpected {rule} from {severity:?} on in {found:?} for {src}");
}

#[test]
fn globals_reached_through_the_environment() {
    has("local g = _G\ng.os.execute('x')", "lua.forbidden_global", Severity::Critical, 2);
    has("_G.RunScript(arg2)", "lua.dynamic_code", Severity::Critical, 1);
    has("getfenv(0).loadstring(arg1)()", "lua.dynamic_code", Severity::Critical, 1);
    has("local e = getfenv(0)\nlocal f = e['load' .. 'string']", "lua.dynamic_code", Severity::Critical, 2);
    has("rawget(_G, 'io').open('x')", "lua.forbidden_global", Severity::Critical, 1);
}

#[test]
fn names_built_by_string_functions() {
    for src in [
        "getglobal(string.lower('LOADSTRING'))(arg1)",
        "getglobal(strupper('r') .. 'unScript')(arg1)",
        "getfenv(0)[string.reverse('gnirtsdaol')](arg1)()",
        "_G[string.format('%s%s', 'loadst', 'ring')](arg1)",
        "_G[string.rep('loadstring', 1)](arg1)",
        "_G[string.sub('xloadstringx', 2, 11)](arg1)",
        "_G[string.char(108, 111, 97, 100, 115, 116, 114, 105, 110, 103)](arg1)",
        "_G[table.concat({'load', 'string'})](arg1)",
        "local k = 'RunScript'\n_G[k](arg1)",
    ] {
        let found = rules(src);
        assert!(found.iter().any(|(r, s, _)| r == "lua.dynamic_code" && *s == Severity::Critical), "{found:?} for {src}");
    }
    has("_G[string.reverse('so')].exit()", "lua.forbidden_global", Severity::Critical, 1);
}

#[test]
fn chat_functions_through_other_names() {
    has("getglobal('SendChatMessage')('spam', 'SAY')", "lua.chat_send", Severity::Warn, 1);
    has("_G['InviteByName']('x')", "lua.chat_send", Severity::Warn, 1);
    has("local t = { x = SendChatMessage }\nt.x('hi', 'SAY')", "lua.chat_send", Severity::Warn, 2);
    // A local copy under the same name, or the function read from _G by its name, hides nothing.
    lacks_at_least("local InviteByName = InviteByName\nInviteByName(name)", "lua.chat_send", Severity::Warn);
    lacks_at_least("_G.SendChatMessage(text, 'SAY')", "lua.chat_send", Severity::Warn);
    has("local S\nS = getglobal('Send' .. 'ChatMessage')\nS('x', 'SAY')", "lua.chat_send", Severity::Warn, 3);
}

#[test]
fn computed_globals_that_are_called() {
    has("_G[a .. 'Script'](arg1)", "lua.dynamic_global", Severity::Warn, 1);
    // A name the scanner cannot see is everyday code (Ace2 calls handlers it found in _G by name),
    // so only noted; a name built toward a checked one warns.
    has("getglobal(name)(x)", "lua.dynamic_global", Severity::Notice, 1);
    has("for k, v in pairs(_G) do if v == h then f = function(m) return _G[k](m) end end end", "lua.dynamic_global", Severity::Notice, 1);
    has("_G[arg1](x)", "lua.dynamic_global", Severity::Warn, 1);
    // Reading a frame by name is everyday 1.12 code.
    lacks_at_least("local f = getglobal(this:GetName() .. 'Text')\nf:SetText('x')", "lua.dynamic_global", Severity::Notice);
}

#[test]
fn game_functions_replaced_by_name() {
    has("setglobal('SendChatMessage', function() end)", "lua.api_override", Severity::Warn, 1);
    has("_G.ChatEdit_SendText = f", "lua.api_override", Severity::Warn, 1);
    has("local g = _G\ng['SendChatMessage'] = f", "lua.api_override", Severity::Warn, 2);
    has("rawset(_G, 'RunScript', f)", "lua.api_override", Severity::Warn, 1);
    // The usual chat hook, and an original put back that a library kept as `_G.X`.
    lacks_at_least("setglobal('ChatFrame_OnEvent', function(e) end)", "lua.api_override", Severity::Notice);
    lacks_at_least("Lib.ORIG_SendChatMessage = _G.SendChatMessage\n_G.SendChatMessage = Lib.ORIG_SendChatMessage", "lua.api_override", Severity::Notice);
    // A hook that keeps the original, and the original called from the hook.
    let hook = "local orig = SendChatMessage\nfunction SendChatMessage(msg, a) return orig(msg, a) end";
    let found = rules(hook);
    assert!(found.iter().all(|(_, s, _)| *s < Severity::Notice), "{found:?}");
    lacks_at_least("local orig = SendChatMessage\nsetglobal('SendChatMessage', function(m) orig(m) end)", "lua.api_override", Severity::Notice);
}

#[test]
fn environment_metatable_through_a_local() {
    has("local g = getfenv(0)\nsetmetatable(g, { __newindex = f })", "lua.global_metatable", Severity::Critical, 2);
    has("local g = getfenv(0)\nsetmetatable(g, { __index = f })", "lua.global_metatable", Severity::Notice, 2);
}

#[test]
fn script_slash_command_runs_code() {
    has("SlashCmdList['SCRIPT'](arg1)", "lua.dynamic_code", Severity::Critical, 1);
    has("SlashCmdList.RUN('print(1)')", "lua.dynamic_code", Severity::Warn, 1);
}

#[test]
fn name_strings_only_count_next_to_computed_lookups() {
    for src in ["if msg == 'load' then end", "btn:SetText('load')", "local cmds = { 'load', 'save' }", "local kind = 'os'"] {
        lacks_at_least(src, "lua.forbidden_name_string", Severity::Notice);
    }
    let found = rules("local n = 'load' .. 'string'\nlocal f = _G[pick(n)]");
    assert!(found.contains(&("lua.forbidden_name_string".into(), Severity::Critical, Some(1))), "{found:?}");
}

#[test]
fn forwarded_whispers() {
    let src = "f:RegisterEvent('CHAT_MSG_WHISPER')\nf:SetScript('OnEvent', function()\n  SendAddonMessage('X', arg1 .. ':' .. arg2, 'RAID')\nend)";
    has(src, "lua.chat_send", Severity::Warn, 3);
    // Answering the sender of a whisper passes on nothing they wrote.
    let reply = "f:RegisterEvent('CHAT_MSG_WHISPER')\nf:SetScript('OnEvent', function() SendChatMessage('The group is full.', 'WHISPER', nil, arg2) end)";
    lacks_at_least(reply, "lua.chat_send", Severity::Warn);
    // Any chat of other players passed on counts, not only whispers.
    let say = "f:RegisterEvent('CHAT_MSG_SAY')\nf:SetScript('OnEvent', function() SendChatMessage(arg1, 'YELL') end)";
    has(say, "lua.chat_send", Severity::Warn, 2);
    // The combat log names other players too, but announcing it is everyday raid code.
    let cast = "f:RegisterEvent('CHAT_MSG_SPELL_HOSTILEPLAYER_DAMAGE')\nf:SetScript('OnEvent', function() SendChatMessage('Cast: ' .. arg1, 'RAID') end)";
    lacks_at_least(cast, "lua.chat_send", Severity::Warn);
    // Sharing own data with the group stays a notice.
    lacks_at_least("SendAddonMessage('X', 'v1', 'RAID')", "lua.chat_send", Severity::Warn);
}

#[test]
fn locals_named_like_sinks_are_not_sinks() {
    lacks_at_least("local loadstring = print\nloadstring(x)", "lua.dynamic_code", Severity::Info);
    lacks_at_least("local t = {}\nt.SendChatMessage = 1", "lua.chat_send", Severity::Info);
}

#[test]
fn one_finding_per_rule_and_line() {
    let found = rules("loadstring(x) loadstring(y)");
    assert_eq!(found.iter().filter(|(r, _, _)| r == "lua.dynamic_code").count(), 1, "{found:?}");
}

#[test]
fn a_newindex_in_another_file_counts() {
    let addon = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\nA.lua\nB.lua\n"),
        ("Lucky/A.lua", b"Lucky_Meta = {}\nLucky_Meta.__newindex = function(t, k, v) end\n"),
        ("Lucky/B.lua", b"setmetatable(getfenv(0), Lucky_Meta)\n"),
    ]);
    let report = scan(&addon, &folders(&["Lucky"]), &[]);
    assert!(report.findings.iter().any(|f| f.rule == "lua.global_metatable" && f.severity == Severity::Critical), "{:#?}", report.findings);
}

#[test]
fn globals_set_in_one_file_and_used_in_another() {
    let addon = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\nA.lua\nB.lua\nFrames.xml\n"),
        ("Lucky/A.lua", b"Lucky_Send = SendChatMessage\nLucky_Env = getfenv(0)\n"),
        ("Lucky/B.lua", b"Lucky_Send('buy gold', 'SAY')\n"),
        ("Lucky/Frames.xml", b"<Ui><Frame name=\"F\"><Scripts><OnEvent>Lucky_Env.RunScript(arg1)</OnEvent></Scripts></Frame></Ui>"),
    ]);
    let report = scan(&addon, &folders(&["Lucky"]), &[]);
    let hit = |rule: &str, file: &str, severity: Severity| report.findings.iter().any(|f| f.rule == rule && f.file == file && f.severity == severity);
    assert!(hit("lua.chat_send", "Lucky/B.lua", Severity::Warn), "{:#?}", report.findings);
    assert!(hit("lua.dynamic_code", "Lucky/Frames.xml", Severity::Critical), "{:#?}", report.findings);
    // A file the game does not load sets nothing for the others.
    let addon = zip(&[
        ("Lucky/Lucky.toc", b"## Interface: 11200\nB.lua\n"),
        ("Lucky/tests/A.lua", b"Lucky_Send = SendChatMessage\n"),
        ("Lucky/B.lua", b"Lucky_Send('hi', 'SAY')\n"),
    ]);
    let report = scan(&addon, &folders(&["Lucky"]), &[]);
    assert!(!report.flagged(), "{:#?}", report.findings);
}
