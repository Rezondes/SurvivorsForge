//! Rules for Lua code, on the token stream of full_moon. Comments never match, strings only match
//! the string rules, and constant concatenations like `"o".."s"` are folded before the checks.

use crate::entropy::shannon;
use crate::finding::{Report, Severity};
use full_moon::LuaVersion;
use full_moon::tokenizer::{Lexer, LexerResult, Symbol, TokenType};

/// Globals that reach the file system, the OS, other code or the interpreter itself. `getfenv` and
/// `setfenv` are not among them: 1.12 addons and their libraries use them for the global table and
/// their own environments. `collectgarbage` is not either: it only runs or measures the garbage
/// collector, at worst a stutter, and is a notice.
pub const FORBIDDEN_GLOBALS: &[&str] = &["os", "io", "debug", "package", "require", "loadlib", "dofile", "loadfile", "load", "newproxy"];
/// Strings that only make sense to reach a forbidden global by name. Not "debug": it is an
/// everyday settings key and log level.
const FORBIDDEN_NAME_STRINGS: &[&str] = &[
    "os", "io", "package", "require", "loadlib", "dofile", "loadfile", "load", "newproxy", "loadstring",
    "RunScript",
];
pub const CHAT_SEND: &[&str] = &["SendChatMessage", "SendAddonMessage", "GuildInvite", "InviteByName"];
/// Game functions that 1.12 addons commonly hook by replacing them and calling the original;
/// replacing them is only noted.
pub const USUAL_HOOKS: &[&str] = &["ChatFrame_OnEvent"];
/// Game functions an addon should hook, not replace.
pub const API_OVERRIDE: &[&str] = &[
    "SendChatMessage", "SendAddonMessage", "ChatFrame_OnEvent", "ChatEdit_SendText", "GuildInvite", "InviteByName", "RunScript",
    "loadstring", "getglobal", "setglobal", "CreateFrame",
];
const URL_MARKERS: &[&str] = &["http://", "https://", "www.", "discord.gg"];

pub const MAX_LINE: usize = 2000;
pub const MIN_CHAR_ARGS: usize = 8;
pub const MIN_ESCAPE_RUN: usize = 8;
pub const ENTROPY_MIN_LEN: usize = 200;
/// Base64 and similar encodings have about 6 bits per character, English text about 4.2.
pub const ENTROPY_LIMIT: f64 = 5.0;
/// Encoded data has (almost) no spaces; any text has far more than this share.
pub const MAX_ENCODED_SPACES: f64 = 0.02;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    /// Raw text between the quotes, and the value with escapes resolved.
    Str(String, String),
    Num,
    Sym(Symbol),
}

struct Lua<'a> {
    path: &'a str,
    lines: Vec<&'a str>,
    offset: u32,
    toks: Vec<(Tok, usize)>,
}

impl Lua<'_> {
    fn tok(&self, i: usize) -> Option<&Tok> {
        self.toks.get(i).map(|(t, _)| t)
    }

    fn is_sym(&self, i: usize, symbol: Symbol) -> bool {
        matches!(self.tok(i), Some(Tok::Sym(s)) if *s == symbol)
    }

    fn is_ident(&self, i: usize, name: &str) -> bool {
        matches!(self.tok(i), Some(Tok::Ident(n)) if n == name)
    }

    fn is_str(&self, i: usize) -> bool {
        matches!(self.tok(i), Some(Tok::Str(..)))
    }

    /// `x.name` or `x:name`: a field, not a global.
    fn is_member(&self, i: usize) -> bool {
        i > 0 && (self.is_sym(i - 1, Symbol::Dot) || self.is_sym(i - 1, Symbol::Colon))
    }

    /// `{ name = ... }` or `{ a, name = ... }`.
    fn is_table_key(&self, i: usize) -> bool {
        i > 0 && (self.is_sym(i - 1, Symbol::LeftBrace) || self.is_sym(i - 1, Symbol::Comma)) && self.is_sym(i + 1, Symbol::Equal)
    }

    /// `f(...)`, `f "..."` or `f {...}`.
    fn is_call(&self, i: usize) -> bool {
        self.is_sym(i + 1, Symbol::LeftParen) || self.is_str(i + 1) || self.is_sym(i + 1, Symbol::LeftBrace)
    }

    /// The call at `i` has exactly one argument, a string literal.
    fn constant_string_arg(&self, i: usize) -> bool {
        (self.is_sym(i + 1, Symbol::LeftParen) && self.is_str(i + 2) && self.is_sym(i + 3, Symbol::RightParen)) || self.is_str(i + 1)
    }

    /// The first argument of the call at `i` is a string literal.
    fn first_arg_constant(&self, i: usize) -> bool {
        self.is_sym(i + 1, Symbol::LeftParen)
            && self.is_str(i + 2)
            && (self.is_sym(i + 3, Symbol::RightParen) || self.is_sym(i + 3, Symbol::Comma))
            || self.is_str(i + 1)
    }

    fn line(&self, i: usize) -> usize {
        self.toks[i].1
    }

    fn add(&self, report: &mut Report, rule: &str, severity: Severity, i: usize, message: impl Into<String>) {
        let line = self.line(i);
        let text = self.lines.get(line.saturating_sub(1)).copied().unwrap_or_default();
        report.add_with_snippet(rule, severity, self.path, Some(line as u32 + self.offset), message, text);
    }
}

/// Resolves the escapes of a short Lua string.
fn decode(raw: &str) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('x') => {
                let hex: String = (0..2).filter_map(|_| chars.next_if(char::is_ascii_hexdigit)).collect();
                if let Ok(b) = u8::from_str_radix(&hex, 16) {
                    out.push(b as char);
                }
            }
            Some(d) if d.is_ascii_digit() => {
                let mut digits = d.to_string();
                while digits.len() < 3 {
                    match chars.next_if(char::is_ascii_digit) {
                        Some(d) => digits.push(d),
                        None => break,
                    }
                }
                if let Ok(b) = digits.parse::<u8>() {
                    out.push(b as char);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// The longest run of `\ddd` or `\xHH` escapes in a raw string.
fn escape_run(raw: &str) -> usize {
    let bytes = raw.as_bytes();
    let (mut best, mut run, mut i) = (0, 0, 0);
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1].is_ascii_digit() || bytes[i + 1] == b'x') {
            let mut j = i + 2;
            while j < bytes.len() && j < i + 4 && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            run += 1;
            best = best.max(run);
            i = j;
        } else {
            run = 0;
            i += 1;
        }
    }
    best
}

fn tokens(source: &str) -> Vec<(Tok, usize)> {
    let tokens = match Lexer::new(source, LuaVersion::lua51()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => return Vec::new(),
    };
    tokens
        .into_iter()
        .filter_map(|t| {
            let line = t.start_position().line();
            let tok = match t.token_type() {
                TokenType::Identifier { identifier } => Tok::Ident(identifier.to_string()),
                TokenType::StringLiteral { literal, multi_line_depth, .. } => {
                    let raw = literal.to_string();
                    let value = if *multi_line_depth == 0 && !raw.contains('\n') { decode(&raw) } else { raw.clone() };
                    Tok::Str(raw, value)
                }
                TokenType::Number { .. } => Tok::Num,
                TokenType::Symbol { symbol } => Tok::Sym(*symbol),
                _ => return None,
            };
            Some((tok, line))
        })
        .collect()
}

/// Checks Lua `source`. `line_offset` shifts line numbers, for Lua inside an XML file.
pub fn check(path: &str, source: &[u8], line_offset: u32, report: &mut Report) {
    if source.starts_with(b"\x1bLua") || source.contains(&0) {
        report.add("lua.binary", Severity::Critical, path, None, "Compiled Lua or binary data instead of source code");
        return;
    }
    let text = String::from_utf8_lossy(source);
    if let Some((n, _)) = text.lines().enumerate().find(|(_, l)| l.chars().count() > MAX_LINE) {
        report.add("lua.long_line", Severity::Notice, path, Some(n as u32 + 1 + line_offset), format!("A line longer than {MAX_LINE} characters"));
    }
    let parsed = full_moon::parse_fallible(&text, LuaVersion::lua51());
    if let Some(error) = parsed.errors().first() {
        report.add("lua.parse_error", Severity::Warn, path, None, format!("The code cannot be analysed: {error}"));
    }
    let lua = Lua { path, lines: text.lines().collect(), offset: line_offset, toks: tokens(&text) };
    for i in 0..lua.toks.len() {
        match lua.tok(i).unwrap() {
            Tok::Ident(name) => check_ident(&lua, i, name, report),
            Tok::Str(raw, value) => check_string(&lua, i, raw, value, report),
            _ => {}
        }
    }
}

fn check_ident(lua: &Lua, i: usize, name: &str, report: &mut Report) {
    if name == "string" && lua.is_sym(i + 1, Symbol::Dot) {
        if lua.is_ident(i + 2, "dump") {
            lua.add(report, "lua.forbidden_global", Severity::Critical, i, "string.dump turns functions into bytecode");
        }
        if lua.is_ident(i + 2, "char") && lua.is_sym(i + 3, Symbol::LeftParen) {
            let mut numbers = 0;
            let mut j = i + 4;
            while matches!(lua.tok(j), Some(Tok::Num)) {
                numbers += 1;
                if !lua.is_sym(j + 1, Symbol::Comma) {
                    break;
                }
                j += 2;
            }
            if numbers >= MIN_CHAR_ARGS {
                lua.add(report, "lua.obfuscation_char", Severity::Warn, i, format!("string.char with {numbers} codes hides text"));
            }
        }
    }
    if lua.is_member(i) || lua.is_table_key(i) {
        return;
    }
    if FORBIDDEN_GLOBALS.contains(&name) {
        lua.add(report, "lua.forbidden_global", Severity::Critical, i, format!("{name} is not allowed in addons"));
    }
    if name == "collectgarbage" {
        lua.add(report, "lua.forbidden_global", Severity::Notice, i, "collectgarbage runs or measures the garbage collector and can make the game stutter");
    }
    match name {
        "loadstring" | "RunScript" => {
            if lua.is_call(i) && lua.constant_string_arg(i) {
                lua.add(report, "lua.dynamic_code", Severity::Warn, i, format!("{name} runs code from a string"));
            } else {
                lua.add(report, "lua.dynamic_code", Severity::Critical, i, format!("{name} runs code that is built at run time"));
            }
        }
        // getglobal(this:GetName() .. "Text") is everyday 1.12 code; writing one is rarer.
        "getglobal" if lua.is_call(i) && !lua.first_arg_constant(i) => {
            lua.add(report, "lua.dynamic_global", Severity::Info, i, "getglobal with a computed name");
        }
        // setglobal(name, wrapper) is how 1.12 addons hook game functions.
        "setglobal" if lua.is_call(i) && !lua.first_arg_constant(i) => {
            lua.add(report, "lua.dynamic_global", Severity::Notice, i, "setglobal with a computed name");
        }
        "getfenv" | "setfenv" if !lua.is_member(i) => {
            lua.add(report, "lua.environment", Severity::Info, i, format!("{name} works with function environments"));
        }
        "_G" if lua.is_sym(i + 1, Symbol::LeftBracket) && !(lua.is_str(i + 2) && lua.is_sym(i + 3, Symbol::RightBracket)) => {
            lua.add(report, "lua.dynamic_global", Severity::Notice, i, "_G with a computed name");
        }
        "setmetatable" if lua.is_sym(i + 1, Symbol::LeftParen) && lua.is_ident(i + 2, "_G") => {
            lua.add(report, "lua.global_metatable", Severity::Critical, i, "Changes how every global is looked up");
        }
        "rawset" if lua.is_sym(i + 1, Symbol::LeftParen) && lua.is_ident(i + 2, "_G") && !(lua.is_sym(i + 3, Symbol::Comma) && lua.is_str(i + 4)) => {
            lua.add(report, "lua.global_metatable", Severity::Critical, i, "rawset on _G with a computed name");
        }
        _ => {}
    }
    if CHAT_SEND.contains(&name) {
        // Addon messages stay between addons and are how groups share data; the rest reaches people.
        let severity = if name == "SendAddonMessage" { Severity::Notice } else { Severity::Warn };
        lua.add(report, "lua.chat_send", severity, i, format!("{name} sends messages or invites in the player's name"));
    }
    let assigned = lua.is_sym(i + 1, Symbol::Equal) && !(i > 0 && lua.is_sym(i - 1, Symbol::Local));
    let declared = i > 0 && lua.is_sym(i - 1, Symbol::Function) && lua.is_sym(i + 1, Symbol::LeftParen);
    if API_OVERRIDE.contains(&name) && (assigned || declared) {
        let severity = if USUAL_HOOKS.contains(&name) { Severity::Info } else { Severity::Warn };
        lua.add(report, "lua.api_override", severity, i, format!("Replaces the game function {name}"));
    }
}

fn check_string(lua: &Lua, i: usize, raw: &str, value: &str, report: &mut Report) {
    // Fold "a" .. "b" .. "c" once, from the first string of the chain.
    if !(i > 0 && lua.is_sym(i - 1, Symbol::TwoDots)) {
        let mut folded = value.to_string();
        let mut j = i;
        while lua.is_sym(j + 1, Symbol::TwoDots) {
            match lua.tok(j + 2) {
                Some(Tok::Str(_, v)) => {
                    folded.push_str(v);
                    j += 2;
                }
                _ => break,
            }
        }
        if FORBIDDEN_NAME_STRINGS.contains(&folded.as_str()) {
            lua.add(report, "lua.forbidden_name_string", Severity::Critical, i, format!("The string \"{folded}\" names a forbidden function"));
        }
    }
    let lower = value.to_ascii_lowercase();
    if URL_MARKERS.iter().any(|m| lower.contains(m)) {
        lua.add(report, "lua.url", Severity::Notice, i, "Contains a web address");
    }
    if escape_run(raw) >= MIN_ESCAPE_RUN {
        lua.add(report, "lua.obfuscation_escape", Severity::Warn, i, "A long run of escaped bytes hides text");
    }
    if looks_encoded(value) {
        lua.add(report, "lua.obfuscation_entropy", Severity::Warn, i, "A long string that looks encrypted or encoded");
    }
}

/// A long string without spaces whose characters are as varied as encoded data, or one made only
/// of hex digits. Prose, even with capitals and punctuation, has spaces.
pub fn looks_encoded(value: &str) -> bool {
    let len = value.chars().count();
    if len <= ENTROPY_MIN_LEN {
        return false;
    }
    let spaces = value.chars().filter(|c| c.is_whitespace()).count();
    if spaces as f64 / len as f64 > MAX_ENCODED_SPACES {
        return false;
    }
    value.chars().all(|c| c.is_ascii_hexdigit()) || shannon(value) > ENTROPY_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode(r"\111\115"), "os");
        assert_eq!(decode(r"\x6f\x73"), "os");
        assert_eq!(decode(r#"a\"b\\c\n"#), "a\"b\\c\n");
    }

    #[test]
    fn escape_runs() {
        assert_eq!(escape_run(r"\104\101\108\108\111\032\119\111"), 8);
        assert_eq!(escape_run(r"a\104b\101"), 1);
    }
}
