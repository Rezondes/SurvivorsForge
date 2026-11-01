use forge_scan::{Report, Severity, lua_rules};

fn check(src: &str) -> Report {
    let mut report = Report::default();
    lua_rules::check("A/a.lua", src.as_bytes(), 0, &mut report);
    report
}

fn rules(src: &str) -> Vec<(String, Severity, Option<u32>)> {
    check(src).findings.into_iter().map(|f| (f.rule, f.severity, f.line)).collect()
}

fn has(src: &str, rule: &str, severity: Severity) {
    let found = rules(src);
    assert!(found.iter().any(|(r, s, _)| r == rule && *s == severity), "{rule}/{severity:?} not in {found:?} for {src}");
}

fn lacks(src: &str, rule: &str) {
    let found = rules(src);
    assert!(found.iter().all(|(r, _, _)| r != rule), "unexpected {rule} in {found:?} for {src}");
}

#[test]
fn forbidden_global_positive() {
    for src in ["os.execute('x')", "local f = io.open('x')", "debug.getinfo(1)", "require('socket')", "loadlib('a', 'b')",
        "dofile('x')", "local s = string.dump(f)"] {
        has(src, "lua.forbidden_global", Severity::Critical);
    }
}

#[test]
fn collectgarbage_is_only_a_notice() {
    for src in ["collectgarbage()", "local kb = collectgarbage('count')"] {
        has(src, "lua.forbidden_global", Severity::Notice);
        assert!(rules(src).iter().all(|(_, s, _)| *s < Severity::Warn), "{:?} for {src}", rules(src));
    }
    lacks("local n = 'collectgarbage'", "lua.forbidden_name_string");
}

#[test]
fn forbidden_global_negative() {
    for src in ["x.os = 1", "self:io()", "local t = { os = 1 }", "local myos = 1", "iostat()", "-- os.execute('x')", "local s = 'os.exit()'"] {
        lacks(src, "lua.forbidden_global");
    }
}

#[test]
fn locals_shadow_forbidden_globals() {
    for src in [
        "local load = GetLoadout()\nif load and load.abilities then x = load.abilities[1] end",
        "local function f(os, io) return os + io end",
        "for load = 1, 3 do print(load) end",
        "for _, load in ipairs(t) do print(load.name) end",
        "local function load() end\nload()",
        "local os\nos = 1",
        "repeat local load = next() until load",
        "do local io = 1 end\nlocal io = 2\nprint(io)",
    ] {
        lacks(src, "lua.forbidden_global");
    }
}

#[test]
fn globals_outside_a_local_scope_still_count() {
    for src in [
        // The value of `local x = x` is the global.
        "local os = os",
        "local load = 1, load",
        // The local ends with its block.
        "do local os = 1 end\nos.exit()",
        "if a then local load = 1 else load('x') end",
        "local function f(os) end\nos.exit()",
        "for io = 1, 2 do end\nio.open('x')",
        "local function f()\n  local load = 1\nend\nload('x')",
        // A file that does not parse has no locals.
        "local load = 1\nload('x') end end",
    ] {
        has(src, "lua.forbidden_global", Severity::Critical);
    }
}

#[test]
fn brackets_are_read_like_lua_5_0() {
    // Lua 5.0 has no `[==[`: `--[==[` is a line comment and the code after it runs.
    has("--[==[\nRunScript(arg1)\n--]==]", "lua.dynamic_code", Severity::Critical);
    has("--[=[ x\nos.exit()", "lua.forbidden_global", Severity::Critical);
    // Lua 5.0 nests `[[`: all of this is one string or comment.
    for src in ["local s = [[ a [[ b ]] c ]]", "local s = [[ x [[ ]] os.execute() ]]", "--[[ a [[ b ]] os.exit() ]]"] {
        assert!(rules(src).is_empty(), "{:?} for {src}", rules(src));
    }
    let found = rules("local s = [[\n[[\n]]\n]]\nos.exit()\n");
    assert!(found.contains(&("lua.forbidden_global".into(), Severity::Critical, Some(5))), "{found:?}");
    // Brackets inside strings are text.
    assert!(rules("local s = \"[[\" local t = '--[=['").is_empty());
}

#[test]
fn line_numbers_are_reported() {
    let found = rules("local a = 1\n\nos.exit()\n");
    assert!(found.contains(&("lua.forbidden_global".into(), Severity::Critical, Some(3))), "{found:?}");
}

#[test]
fn forbidden_name_string_positive() {
    // Critical next to a lookup of a global by a name the scanner cannot follow, else only noted.
    has("local n = 'os'\nlocal v = _G[f(n)]", "lua.forbidden_name_string", Severity::Critical);
    has("local n = \"\\108\\111\\097\\100\\115\\116\\114\\105\\110\\103\"\nlocal v = getglobal(f(n))", "lua.forbidden_name_string", Severity::Critical);
    has("local n = 'Run' .. 'Script'", "lua.forbidden_name_string", Severity::Info);
}

#[test]
fn forbidden_name_string_negative() {
    lacks("local n = 'osmosis'", "lua.forbidden_name_string");
    lacks("local n = 'o' .. x .. 's'", "lua.forbidden_name_string");
}

#[test]
fn dynamic_code_positive() {
    has("loadstring('return 1')()", "lua.dynamic_code", Severity::Warn);
    // Code built at run time is common in 1.12 (macros, profiles, generated handlers): flagged
    // for a look, not a strong sign of harm.
    has("loadstring(code)()", "lua.dynamic_code", Severity::Warn);
    has("RunScript(a .. b)", "lua.dynamic_code", Severity::Warn);
    has("local l = loadstring", "lua.dynamic_code", Severity::Warn);
}

#[test]
fn a_parameter_named_like_an_event_argument_is_no_chat() {
    let found = check("function DM_ConsolePrint(arg1)\n  RunScript('DMPrint((' .. arg1 .. '))')\nend").findings;
    let f = found.iter().find(|f| f.rule == "lua.dynamic_code").expect("found");
    assert_eq!(f.severity, Severity::Warn);
    assert!(f.message.contains("built at run time"), "{}", f.message);
}

#[test]
fn dynamic_code_negative() {
    lacks("frame.loadstring = nil", "lua.dynamic_code");
    lacks("-- loadstring(x)", "lua.dynamic_code");
}

#[test]
fn folded_concat_and_dynamic_global() {
    let src = r#"local o = "o".."s"; _G[o].execute("x")"#;
    has(src, "lua.forbidden_global", Severity::Critical);
    has(src, "lua.dynamic_global", Severity::Info);
}

#[test]
fn dynamic_global_positive() {
    has("local f = getglobal(name)", "lua.dynamic_global", Severity::Info);
    has("local g = getfenv(0)", "lua.environment", Severity::Info);
    has("setfenv(1, env)", "lua.environment", Severity::Info);
    lacks("local g = getfenv(0)", "lua.forbidden_global");
    lacks("setfenv(1, env)", "lua.forbidden_global");
    lacks("if opts['debug'] then end", "lua.forbidden_name_string");
    has("setglobal(n, 1)", "lua.dynamic_global", Severity::Notice);
    // Reading by a computed name is everyday 1.12 code, as with getglobal; writing one is rarer.
    has("local v = _G[k]", "lua.dynamic_global", Severity::Info);
    has("_G[k] = v", "lua.dynamic_global", Severity::Notice);
    // A frame's own name, like getglobal(this:GetName() .. "Text").
    lacks("local s = _G[scroll:GetName() .. \"ScrollBar\"]", "lua.dynamic_global");
    lacks("local s = _G[self.frame:GetName() .. \"Text\"]", "lua.dynamic_global");
}

#[test]
fn dynamic_global_negative() {
    lacks("local f = getglobal('LuckyFrame')", "lua.dynamic_global");
    lacks("local v = _G['LuckyDB']", "lua.dynamic_global");
    lacks("local v = t[k]", "lua.dynamic_global");
}

#[test]
fn global_metatable_positive() {
    // __newindex sees every global any addon sets.
    has("setmetatable(_G, { __newindex = f })", "lua.global_metatable", Severity::Critical);
    has("local m = {}\nm[\"__new\" .. \"index\"] = f\nsetmetatable(_G, m)", "lua.global_metatable", Severity::Critical);
    // __index only answers for globals that do not exist (SurvivorUI counts jumps this way).
    has("setmetatable(_G, { __index = f })", "lua.global_metatable", Severity::Notice);
    let jumps = "local globals = getfenv(0)\nlocal meta = getmetatable(globals) or {}\nmeta.__index = function(t, k) end\nsetmetatable(globals, meta)";
    has(jumps, "lua.global_metatable", Severity::Notice);
    assert!(!check(jumps).flagged(), "{:?}", rules(jumps));
}

#[test]
fn global_metatable_negative() {
    lacks("setmetatable(t, mt)", "lua.global_metatable");
    lacks("rawset(_G, 'LuckyDB', {})", "lua.global_metatable");
    // The same as `_G[k] = v` unless _G has a metatable, and setting that is Critical already.
    lacks("rawset(_G, k, v)", "lua.global_metatable");
    has("rawset(_G, k, v)", "lua.dynamic_global", Severity::Notice);
}

#[test]
fn chat_send_positive() {
    // Announcing is what raid and boss addons are for: shown, but no flag.
    has("SendChatMessage('hi', 'GUILD')", "lua.chat_send", Severity::Notice);
    has("GuildInvite(name)", "lua.chat_send", Severity::Notice);
    assert!(!check("SendChatMessage('Interrupted!', 'SAY')").flagged());
}

#[test]
fn chat_send_negative() {
    lacks("DEFAULT_CHAT_FRAME:AddMessage('hi')", "lua.chat_send");
    lacks("frame.SendChatMessage = nil", "lua.chat_send");
    // Saving or testing the function sends nothing.
    for src in ["local Orig = SendChatMessage", "if SendChatMessage then end", "local has = GuildInvite ~= nil"] {
        assert!(rules(src).iter().all(|(r, s, _)| r != "lua.chat_send" || *s == Severity::Info), "{:?} for {src}", rules(src));
    }
}

#[test]
fn api_override_positive() {
    has("SendChatMessage = function() end", "lua.api_override", Severity::Warn);
    has("function ChatFrame_OnEvent(event) end", "lua.api_override", Severity::Info);
}

#[test]
fn api_override_negative() {
    lacks("local SendChatMessage = SendChatMessage", "lua.api_override");
    lacks("if CreateFrame == nil then end", "lua.api_override");
    // The usual hook: keep the original, call it from the replacement.
    let hook = "local Orig = SendChatMessage\nSendChatMessage = function(m, a) Orig(m, a) end";
    has(hook, "lua.api_override", Severity::Info);
    assert!(!check(hook).flagged(), "{:?}", rules(hook));
    // The original kept in a global or a field works the same.
    let hook = "MyAddon_Orig = SendChatMessage\nSendChatMessage = MyAddon_Send";
    has(hook, "lua.api_override", Severity::Info);
}

#[test]
fn code_in_constant_strings_is_scanned() {
    has("loadstring([[os.execute('x')]])()", "lua.forbidden_global", Severity::Critical);
    has("RunScript(\"SendChatMessage('buy gold', 'SAY')\")", "lua.chat_send", Severity::Notice);
    // The code keeps the line of the call.
    let found = rules("local a = 1\nRunScript('RunScript(arg1)')");
    assert!(found.contains(&("lua.dynamic_code".into(), Severity::Critical, Some(2))), "{found:?}");
}

#[test]
fn chat_box_and_macros_run_code() {
    has("ChatFrameEditBox:SetText('/script ' .. arg2)", "lua.dynamic_code", Severity::Warn);
    has("local cmd = '/run Foo()'", "lua.dynamic_code", Severity::Warn);
    has("ChatEdit_SendText(ChatFrameEditBox)", "lua.chat_send", Severity::Notice);
    has("CreateMacro('x', 1, '/cast y', 1)", "lua.macro", Severity::Notice);
    has("EditMacro(1, 'x', 1, body)", "lua.macro", Severity::Notice);
    lacks("print('type /lucky to open the options')", "lua.dynamic_code");
}

#[test]
fn chat_frame_hooks_are_noted() {
    has("DEFAULT_CHAT_FRAME.AddMessage = function(self, m) end", "lua.api_override", Severity::Notice);
    has("ChatFrame1.AddMessage = Lucky_AddMessage", "lua.api_override", Severity::Notice);
    lacks("DEFAULT_CHAT_FRAME:AddMessage('hi')", "lua.api_override");
}

#[test]
fn environment_metatable_is_critical() {
    has("setmetatable(getfenv(0), { __newindex = f })", "lua.global_metatable", Severity::Critical);
}

#[test]
fn the_games_addon_api_is_not_replaced() {
    // The game fills the AZS_ globals for every addon; replacing one breaks the others.
    has("AZS_GetPhase = function() return 'playing' end", "lua.api_override", Severity::Warn);
    has("function AZS_GetMeter() end", "lua.api_override", Severity::Warn);
    // The tables only hold data the game sends again; writing one is noted.
    has("AZS_RUN = {}", "lua.api_override", Severity::Notice);
    has("AZS_STATS = result", "lua.api_override", Severity::Notice);
    assert!(message("AZS_UNITS = {}", "lua.api_override").contains("Writes AZS_UNITS of the game's addon API"));
    has("setglobal('AZS_GetBosses', f)", "lua.api_override", Severity::Warn);
    // Reading it, keeping a local of the same name, or hooking it is fine.
    for src in ["local p = AZS_GetPhase()", "local AZS_GetPhase = AZS_GetPhase", "local orig = AZS_GetMeter\nAZS_GetMeter = function() return orig() end"] {
        assert!(rules(src).iter().all(|(r, s, _)| r != "lua.api_override" || *s == Severity::Info), "{:?} for {src}", rules(src));
    }
}

#[test]
fn names_as_keys_of_other_tables_are_data() {
    // A file with a lookup by computed name, where name strings would count.
    let src = "local f = getglobal(pick())\nlocal o = q[\"os\"]\nt[\"os\"][i] = 1\nlocal u = { [\"load\"] = 1 }";
    let found = rules(src);
    assert!(found.iter().all(|(r, s, _)| r != "lua.forbidden_name_string" || *s == Severity::Info), "{found:?}");
    // A slash command compared with its keyword names nothing.
    let src = "local f = getglobal(pick())\nif msg == \"load\" or \"os\" ~= cmd then end";
    let found = rules(src);
    assert!(found.iter().all(|(r, s, _)| r != "lua.forbidden_name_string" || *s == Severity::Info), "{found:?}");
}

#[test]
fn backslash_at_the_end_of_a_line_continues_the_string() {
    let src = "local s = 'a\\\r\nb'\r\nos.exit()";
    let found = rules(src);
    assert!(found.iter().all(|(r, _, _)| r != "lua.parse_error"), "{found:?}");
    assert!(found.contains(&("lua.forbidden_global".into(), Severity::Critical, Some(3))), "{found:?}");
}

#[test]
fn table_keys_after_semicolons_are_keys() {
    lacks("Foo_Defaults = { scale = 1; debug = false; load = true }", "lua.forbidden_global");
}

#[test]
fn text_in_other_scripts_is_not_obfuscation() {
    // Chinese has no spaces; a long number is no hex blob.
    let zh = "这是一个很长的提示文字，用来说明插件的功能，以及如何在游戏中使用它。".repeat(8);
    lacks(&format!("L[\"Tip\"] = \"{zh}\""), "lua.obfuscation_entropy");
    lacks(&format!("local n = '{}'", "1234567890".repeat(21)), "lua.obfuscation_entropy");
    // Escaped UTF-8 is text, not hidden ASCII code.
    lacks(r#"L["Hello"] = "\208\159\209\128\208\184\208\178\208\181\209\130""#, "lua.obfuscation_escape");
    lacks("local dots = string.char(194,183,32,194,183,32,194,183)", "lua.obfuscation_char");
    // Control bytes are pattern data; `\\1` in a texture path is a backslash and a digit.
    lacks(r#"local p = "\000\001\002\003\004\005\006\007""#, "lua.obfuscation_escape");
    lacks(r#"local t = "Interface\\AddOns\\Foo\\1\\2\\3\\4\\5\\6\\7\\8""#, "lua.obfuscation_escape");
}

#[test]
fn file_encodings() {
    assert!(rules("\u{feff}local x = 1").is_empty(), "{:?}", rules("\u{feff}local x = 1"));
    let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("local x = 1".bytes().flat_map(|b| [b, 0])).collect();
    let mut report = Report::default();
    lua_rules::check("A/a.lua", &utf16, 0, &mut report);
    let found: Vec<_> = report.findings.iter().map(|f| (f.rule.as_str(), f.severity)).collect();
    assert_eq!(found, [("lua.encoding", Severity::Warn)]);
}

#[test]
fn url_positive() {
    has("local u = 'https://evil.test/x'", "lua.url", Severity::Notice);
    has("print('join discord.gg/abc')", "lua.url", Severity::Notice);
    let mut report = forge_scan::finding::Report::default();
    forge_scan::lua_rules::check("a.lua", b"local KOFI = \"https://ko-fi.com/morderous\"", 0, &mut report);
    assert!(!report.flagged(), "a web address alone does not flag the addon");
}

#[test]
fn url_negative() {
    lacks("-- see https://example.test", "lua.url");
    lacks("local s = 'httpd'", "lua.url");
}

#[test]
fn obfuscation_char_positive() {
    has("local s = string.char(104,101,108,108,111,32,119,111)", "lua.obfuscation_char", Severity::Warn);
}

#[test]
fn obfuscation_char_negative() {
    lacks("local s = string.char(65, 66)", "lua.obfuscation_char");
}

#[test]
fn obfuscation_escape_positive() {
    has(r#"local s = "\104\101\108\108\111\032\119\111""#, "lua.obfuscation_escape", Severity::Warn);
}

#[test]
fn obfuscation_escape_negative() {
    lacks(r#"local s = "line\nnext\ttab""#, "lua.obfuscation_escape");
}

#[test]
fn obfuscation_entropy_positive() {
    // Base64-looking noise from a small LCG.
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut seed: u32 = 12345;
    let blob: String = (0..300)
        .map(|_| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            ALPHABET[(seed >> 16) as usize % ALPHABET.len()] as char
        })
        .collect();
    has(&format!("local s = [[{blob}]]"), "lua.obfuscation_entropy", Severity::Warn);
}

#[test]
fn long_descriptions_are_not_encoded_data() {
    // A tooltip text from a real addon that the old rule flagged.
    let text = "In a co-op run, a frame for each friend: the health in the class's color, DOWN while down, a gold fill while you                 bring them back; hover one for their damage. It puts the game's party frames away. From the options: Lock, Scale,                 Show in solo runs (Off, 1-5), Colors by class (On).";
    lacks(&format!("local tip = \"{text}\""), "lua.obfuscation_entropy");
    let hex: String = (0..260).map(|i| char::from(b"0123456789abcdef"[(i * 7) % 16])).collect();
    has(&format!("local s = '{hex}'"), "lua.obfuscation_entropy", Severity::Warn);
}

#[test]
fn obfuscation_entropy_negative() {
    let prose = "This addon shows your luck for rare cards and keeps a small history of the last runs. ".repeat(4);
    lacks(&format!("local s = '{prose}'"), "lua.obfuscation_entropy");
}

#[test]
fn long_line_positive() {
    has(&format!("local x = {}", "1 + ".repeat(600)), "lua.long_line", Severity::Notice);
}

#[test]
fn long_line_negative() {
    lacks("local x = 1\n", "lua.long_line");
}

#[test]
fn parse_error_positive() {
    has("local function (", "lua.parse_error", Severity::Warn);
}

#[test]
fn parse_error_negative() {
    lacks("local function f() return 1 end", "lua.parse_error");
}

#[test]
fn binary_positive() {
    let mut report = Report::default();
    lua_rules::check("a.lua", b"\x1bLuaQ\x00\x01", 0, &mut report);
    assert_eq!(report.findings[0].rule, "lua.binary");
    assert_eq!(report.findings[0].severity, Severity::Critical);
}

#[test]
fn binary_negative() {
    lacks("print('no nul here')", "lua.binary");
}

#[test]
fn comments_and_substrings_ignored() {
    for src in ["-- os.execute('rm')", "--[[ io.open('x') loadstring(y) ]]", "local myos, iostat = 1, 2", "local t = 'a'"] {
        let found = rules(src);
        assert!(found.is_empty(), "{src}: {found:?}");
    }
}

#[test]
fn clean_addon_has_no_warnings() {
    let report = check(include_str!("fixtures/lua/clean_addon.lua"));
    assert!(!report.flagged(), "{:#?}", report.findings);
}

fn message(src: &str, rule: &str) -> String {
    check(src).findings.into_iter().find(|f| f.rule == rule).map(|f| f.message).unwrap_or_default()
}

#[test]
fn dynamic_code_from_fixed_text_is_a_warning() {
    // AZTotemManager 1.1.0: names from a local table of fixed strings.
    let probe = r#"
        local probeList = { "AZS_GetTotems", "AZS_GetMeter", "AZS_TOTEMS" }
        for i = 1, table.getn(probeList) do
          local nm = probeList[i]
          local chunk = loadstring("return " .. nm)
        end"#;
    has(probe, "lua.dynamic_code", Severity::Warn);
    lacks_severity(probe, "lua.dynamic_code", Severity::Critical);
    assert!(message(probe, "lua.dynamic_code").contains("fixed text"), "{}", message(probe, "lua.dynamic_code"));

    has(r#"local name = "AZS_GetTotems"; local f = loadstring("return " .. name)"#, "lua.dynamic_code", Severity::Warn);
    has(r#"local n = 3; RunScript("x = " .. n)"#, "lua.dynamic_code", Severity::Warn);

    // The same name read or looped over in other functions of the file does not matter.
    let elsewhere = format!("{probe}\nfunction Show(t) for k, nm in pairs(t) do print(tostring(nm)) end end\nlocal row = {{ label = nm, checks = {{}} }}");
    has(&elsewhere, "lua.dynamic_code", Severity::Warn);
    lacks_severity(&elsewhere, "lua.dynamic_code", Severity::Critical);
}

#[test]
fn dynamic_code_from_outside_stays_critical() {
    let chat = r#"
        function OnEvent()
          if event == "CHAT_MSG_ADDON" then loadstring(arg2)() end
        end"#;
    has(chat, "lua.dynamic_code", Severity::Critical);
    assert!(message(chat, "lua.dynamic_code").contains("chat or addon messages"), "{}", message(chat, "lua.dynamic_code"));
}

#[test]
fn code_the_player_types_is_only_noted() {
    // The player could run it with /script as well.
    for src in [
        "local t = box:GetText(); RunScript(t)",
        "SlashCmdList['MYRUN'] = function(msg) RunScript(msg) end",
        "function Foo_Slash(msg) RunScript('print(' .. msg .. ')') end\nSlashCmdList.FOO = Foo_Slash",
        "local function slash(msg) loadstring(msg)() end\nSlashCmdList[\"FOO\"] = slash",
        // Ace2's /print.
        "self:RegisterChatCommand({ '/print' }, function(text) RunScript('f(' .. text .. ')') end, 'PRINT')",
    ] {
        has(src, "lua.dynamic_code", Severity::Notice);
        assert!(!check(src).flagged(), "{:?} for {src}", rules(src));
    }
    // A slash handler that also gets other text stays a warning.
    has("SlashCmdList.FOO = function(msg) RunScript(msg .. GetSomething()) end", "lua.dynamic_code", Severity::Warn);
}

#[test]
fn dynamic_code_that_cannot_be_proven_fixed_is_not_called_fixed() {
    for src in [
        // A parameter with the same name as an earlier fixed local.
        r#"local nm = "safe"
           local function run(nm) loadstring("return " .. nm)() end"#,
        // The fixed local is changed later.
        r#"local nm = "safe"; nm = GetSomething(); loadstring(nm)()"#,
        // The table gets entries from elsewhere, or goes to code that may change it.
        r#"local list = { "a" }; table.insert(list, arg1); loadstring(list[2])()"#,
        r#"local list = { "a" }; list[2] = arg1; loadstring(list[2])()"#,
        r#"local list = { "a" }; Fill(list); loadstring(list[1])()"#,
        // A table with anything but fixed values.
        r#"local list = { "a", Name() }; loadstring(list[1])()"#,
        // A global or a function result.
        "loadstring(SomeGlobal)()",
        r#"loadstring("return " .. build())()"#,
        // A loop variable or a list of names hides the fixed local.
        "local nm = \"x\"\nfor k, nm in pairs(t) do loadstring(nm)() end",
        r#"local nm = "x"; nm, b = arg1, 2; loadstring(nm)()"#,
        // The fixed local or table is out of scope where it is read: that name is a global.
        "function A() local nm = \"x\" end\nfunction B() loadstring(nm)() end",
        "if c then local nm = \"x\" else loadstring(nm)() end",
        "do local list = { \"a\" } end\nloadstring(list[1])()",
        // The value goes on in the next line.
        "local nm = \"x\"\n  .. arg1\nloadstring(nm)()",
    ] {
        let found = check(src).findings;
        let f = found.iter().find(|f| f.rule == "lua.dynamic_code").expect("found");
        assert!(f.severity >= Severity::Warn && !f.message.contains("fixed text"), "{f:?} for {src}");
    }
}

fn lacks_severity(src: &str, rule: &str, severity: Severity) {
    let found = rules(src);
    assert!(found.iter().all(|(r, s, _)| !(r == rule && *s == severity)), "unexpected {rule}/{severity:?} in {found:?}");
}
