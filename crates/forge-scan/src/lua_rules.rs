//! Rules for Lua code, on the token stream of full_moon. Comments never match, strings only match
//! the string rules, and constant concatenations like `"o".."s"` are folded before the checks.

use crate::entropy::shannon;
use crate::finding::{Report, Severity};
use crate::{lua50, lua_flow, scope};
use full_moon::LuaVersion;
use full_moon::tokenizer::{Lexer, LexerResult, Symbol, TokenType};

/// Globals that reach the file system, the OS, other code or the interpreter itself. `getfenv` and
/// `setfenv` are not among them: 1.12 addons and their libraries use them for the global table and
/// their own environments. `collectgarbage` is not either: it only runs or measures the garbage
/// collector, at worst a stutter, and is a notice.
pub const FORBIDDEN_GLOBALS: &[&str] = &["os", "io", "debug", "package", "require", "loadlib", "dofile", "loadfile", "load", "newproxy"];
/// Strings that only make sense to reach a forbidden global by name. Not "debug": it is an
/// everyday settings key and log level.
pub(crate) const FORBIDDEN_NAME_STRINGS: &[&str] = &[
    "os", "io", "package", "require", "loadlib", "dofile", "loadfile", "load", "newproxy", "loadstring",
    "RunScript",
];
/// `ChatEdit_SendText` sends what is in a chat box, slash commands like `/script` included.
pub const CHAT_SEND: &[&str] = &["SendChatMessage", "SendAddonMessage", "GuildInvite", "InviteByName", "ChatEdit_SendText"];
/// Slash commands that run Lua.
const SCRIPT_COMMANDS: &[&str] = &["/script", "/run"];
/// How deep code in constant strings, run by `loadstring` or `RunScript`, is scanned in turn.
const MAX_NESTED_CODE: usize = 3;
/// Game functions that 1.12 addons commonly hook by replacing them and calling the original;
/// replacing them is only noted.
pub const USUAL_HOOKS: &[&str] = &["ChatFrame_OnEvent"];
/// Game functions an addon should hook, not replace.
pub const API_OVERRIDE: &[&str] = &[
    "SendChatMessage", "SendAddonMessage", "ChatFrame_OnEvent", "ChatEdit_SendText", "GuildInvite", "InviteByName", "RunScript",
    "loadstring", "getglobal", "setglobal", "CreateFrame",
];
/// The game's addon API: functions and tables it fills before any addon loads, for all of them.
pub const GAME_API_PREFIX: &str = "AZS_";

/// A global an addon should hook or read, not replace.
/// How replacing or writing the game global `name` is reported. The AZS_ tables (all capitals)
/// hold data the game sends again, so writing one is noted; a hook that keeps the original too.
pub(crate) fn api_change(name: &str, kept: bool) -> (Severity, String) {
    if name.strip_prefix(GAME_API_PREFIX).is_some_and(|rest| !rest.chars().any(|c| c.is_ascii_lowercase())) {
        (Severity::Notice, format!("Writes {name} of the game's addon API; other addons read it too"))
    } else if USUAL_HOOKS.contains(&name) || kept {
        (Severity::Info, format!("Replaces the game function {name}"))
    } else {
        (Severity::Warn, format!("Replaces the game function {name}"))
    }
}

/// The code names `__newindex`, which on the global table sees every global any addon sets. Only
/// `__index` answers for globals that do not exist, like SurvivorUI's jump counter.
// ponytail: a name built with string.char or escapes slips by; obfuscation_char and obfuscation_escape warn on those.
pub(crate) fn mentions_newindex(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("newindex") || text.contains("__new")
}

/// How a metatable on the global table is reported.
pub(crate) fn env_metatable(newindex: bool) -> (Severity, &'static str) {
    if newindex {
        (Severity::Critical, "Changes how every global is looked up and set")
    } else {
        (Severity::Notice, "Changes how globals that do not exist are looked up")
    }
}

pub fn is_api(name: &str) -> bool {
    API_OVERRIDE.contains(&name) || name.starts_with(GAME_API_PREFIX)
}
const URL_MARKERS: &[&str] = &["http://", "https://", "www.", "discord.gg"];

pub const MAX_LINE: usize = 2000;
pub const MIN_CHAR_ARGS: usize = 8;
pub const MIN_ESCAPE_RUN: usize = 8;
pub const ENTROPY_MIN_LEN: usize = 200;
/// Base64 and similar encodings have about 6 bits per character, English text about 4.2.
pub const ENTROPY_LIMIT: f64 = 5.0;
/// Encoded data has (almost) no spaces; any text has far more than this share.
pub const MAX_ENCODED_SPACES: f64 = 0.02;

/// Where the code that `loadstring` or `RunScript` runs comes from.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Origin {
    /// Only literals, provably: the addon's own text.
    Fixed,
    /// What the player types: an input field or a slash command. They could run it with /script.
    Typed,
    /// Event arguments: chat and addon messages of other players.
    Outside,
    /// Anything the scanner cannot follow.
    Unknown,
}

impl Origin {
    /// Two parts together: outside wins, then unknown, then typed.
    fn and(self, other: Origin) -> Origin {
        match (self, other) {
            (Origin::Outside, _) | (_, Origin::Outside) => Origin::Outside,
            (Origin::Unknown, _) | (_, Origin::Unknown) => Origin::Unknown,
            (Origin::Typed, _) | (_, Origin::Typed) => Origin::Typed,
            _ => Origin::Fixed,
        }
    }
}

/// How many locals deep the origin is followed (`a` from `b` from `c`).
const MAX_ORIGIN_DEPTH: usize = 4;
/// Event arguments of 1.12 handlers: chat and addon messages arrive in them.
const OUTSIDE_VALUES: &[&str] = &["arg", "arg1", "arg2", "arg3", "arg4", "arg5", "arg6", "arg7", "arg8", "arg9"];
/// Functions that only read a table they get.
const TABLE_READERS: &[&str] = &["getn", "ipairs", "pairs", "unpack"];

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    /// Raw text between the quotes, and the value with escapes resolved.
    Str(String, String),
    /// The number as written.
    Num(String),
    Sym(Symbol),
}

struct Lua<'a> {
    path: &'a str,
    lines: Vec<&'a str>,
    offset: u32,
    toks: Vec<(Tok, usize)>,
    /// How many constant strings of code this code sits in.
    nested: usize,
    /// The code looks up a global by a name the scanner cannot follow.
    computed: bool,
    /// The file names `__newindex`.
    newindex: bool,
    /// Per token: it declares a parameter that holds what the player types, like the text of a
    /// slash command.
    typed: Vec<bool>,
    /// Per token: a name that is a local there, not a global.
    locals: Vec<bool>,
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

    fn is_local(&self, i: usize) -> bool {
        self.locals.get(i).copied().unwrap_or(false)
    }

    /// `x.name` or `x:name`: a field, not a global.
    fn is_member(&self, i: usize) -> bool {
        i > 0 && (self.is_sym(i - 1, Symbol::Dot) || self.is_sym(i - 1, Symbol::Colon))
    }

    /// `{ name = ... }`, `{ a, name = ... }` or `{ a; name = ... }`.
    fn is_table_key(&self, i: usize) -> bool {
        i > 0
            && (self.is_sym(i - 1, Symbol::LeftBrace) || self.is_sym(i - 1, Symbol::Comma) || self.is_sym(i - 1, Symbol::Semicolon))
            && self.is_sym(i + 1, Symbol::Equal)
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

    /// Where the single argument of the call at `i` comes from; `Unknown` unless proven.
    fn call_arg_origin(&self, i: usize) -> Origin {
        if !self.is_sym(i + 1, Symbol::LeftParen) {
            return Origin::Unknown;
        }
        let Some(close) = self.closing(i + 1) else { return Origin::Unknown };
        self.expr_origin(i + 2, close, 0)
    }

    /// The index of the bracket that closes the one at `open`.
    fn closing(&self, open: usize) -> Option<usize> {
        let mut depth = 0i32;
        for j in open..self.toks.len() {
            match self.tok(j) {
                Some(Tok::Sym(Symbol::LeftParen | Symbol::LeftBracket | Symbol::LeftBrace)) => depth += 1,
                Some(Tok::Sym(Symbol::RightParen | Symbol::RightBracket | Symbol::RightBrace)) => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(j);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The origin of the tokens `from..to`: operands joined by `..`, each a literal, a local fixed
    /// at its declaration, an element of a fixed local table, or something from outside.
    fn expr_origin(&self, from: usize, to: usize, depth: usize) -> Origin {
        if from >= to || depth > MAX_ORIGIN_DEPTH {
            return Origin::Unknown;
        }
        let mut origin = Origin::Fixed;
        let mut j = from;
        while j < to {
            let (part, next) = match self.tok(j) {
                Some(Tok::Str(..) | Tok::Num(_)) => (Origin::Fixed, j + 1),
                // `box:GetText()` or `f.scroll.text:GetText()`: what the player typed.
                Some(Tok::Ident(_)) if self.typed_text_end(j).is_some() => (Origin::Typed, self.typed_text_end(j).unwrap()),
                Some(Tok::Ident(name)) if OUTSIDE_VALUES.contains(&name.as_str()) && !self.is_local(j) => (Origin::Outside, j + 1),
                Some(Tok::Ident(name)) if self.is_sym(j + 1, Symbol::LeftBracket) => match self.closing(j + 1) {
                    Some(end) => (self.table_origin(name, j, depth), end + 1),
                    None => (Origin::Unknown, to),
                },
                Some(Tok::Ident(name)) if !self.is_member(j) => (self.local_origin(name, j, depth), j + 1),
                _ => (Origin::Unknown, j + 1),
            };
            origin = origin.and(part);
            if origin == Origin::Outside {
                return origin;
            }
            j = next;
            // Whatever follows a part must be `..` and another part, or the end.
            if j < to {
                if !self.is_sym(j, Symbol::TwoDots) {
                    return origin.and(Origin::Unknown);
                }
                j += 1;
            }
        }
        origin
    }

    /// The index after `a.b.c:GetText()` that starts at `j`.
    fn typed_text_end(&self, j: usize) -> Option<usize> {
        self.method_call_end(j, "GetText")
    }

    /// The index after `a.b.c:method()` that starts at `j`.
    fn method_call_end(&self, j: usize, method: &str) -> Option<usize> {
        let mut k = j;
        while self.is_sym(k + 1, Symbol::Dot) && matches!(self.tok(k + 2), Some(Tok::Ident(_))) {
            k += 2;
        }
        let call = self.is_sym(k + 1, Symbol::Colon) && self.is_ident(k + 2, method);
        (call && self.is_sym(k + 3, Symbol::LeftParen) && self.is_sym(k + 4, Symbol::RightParen)).then_some(k + 5)
    }

    /// A local `name` read at `at`: fixed only when the declaration it reads, the nearest one before
    /// `at` still in scope there, is `local name = <fixed>` and nothing in the file assigns the name.
    fn local_origin(&self, name: &str, at: usize, depth: usize) -> Origin {
        // The declaration, and whether it is a plain `local name = ...`.
        let mut binding = None;
        for j in (0..self.toks.len()).filter(|&j| self.is_ident(j, name) && !self.is_member(j)) {
            let local = j > 0 && self.is_sym(j - 1, Symbol::Local);
            let assigned = self.is_sym(j + 1, Symbol::Equal);
            let list = self.name_list(j);
            let function = j > 0 && self.is_sym(j - 1, Symbol::Function);
            if (assigned && !local) || list == Some(false) || function {
                return Origin::Unknown;
            }
            // A parameter, a loop variable or a list of locals: never provably fixed.
            let declares = local || list.is_some() || (j > 0 && self.is_sym(j - 1, Symbol::For));
            if declares && j < at && self.in_scope(j, at) {
                binding = Some((j, local && assigned && list.is_none()));
            }
        }
        if let Some((j, _)) = binding
            && self.typed[j]
        {
            return Origin::Typed;
        }
        let Some((j, true)) = binding else { return Origin::Unknown };
        // The value runs to the end of the statement: the next line or `;`.
        let line = self.line(j);
        let end = (j + 2..self.toks.len()).find(|&k| self.line(k) != line || self.is_sym(k, Symbol::Semicolon)).unwrap_or(self.toks.len());
        if (end..self.toks.len()).next().is_some_and(|k| self.continues(k)) {
            return Origin::Unknown;
        }
        self.expr_origin(j + 2, end, depth + 1)
    }

    /// The name at `j` in a list that declares it (`Some(true)`: `local a, b`, `for k, v`, a
    /// parameter) or assigns it (`Some(false)`: `a, b = ...`).
    fn name_list(&self, j: usize) -> Option<bool> {
        let names = |k: usize| self.is_sym(k, Symbol::Comma) && matches!(self.tok(k + 1), Some(Tok::Ident(_)));
        let mut first = j;
        while first >= 2 && names(first - 1) {
            first -= 2;
        }
        let mut last = j;
        while names(last + 1) {
            last += 2;
        }
        let opens = first > 0 && (self.is_sym(first - 1, Symbol::Local) || self.is_sym(first - 1, Symbol::For));
        // `function f(a, b)` or `function(a, b)`: back over the name to `function`.
        let parameter = first > 0 && self.is_sym(first - 1, Symbol::LeftParen) && {
            let mut k = first - 1;
            while k > 0 && matches!(self.tok(k - 1), Some(Tok::Ident(_) | Tok::Sym(Symbol::Dot | Symbol::Colon))) {
                k -= 1;
            }
            k > 0 && self.is_sym(k - 1, Symbol::Function)
        };
        if parameter || (first < last && opens) {
            Some(true)
        } else if first < last && self.is_sym(last + 1, Symbol::Equal) && !self.in_expression(first) {
            Some(false)
        } else {
            None
        }
    }

    /// No block that holds the declaration at `decl` ends before `at`; `else` and `elseif` end the
    /// branch before them.
    fn in_scope(&self, decl: usize, at: usize) -> bool {
        let mut depth = 0i32;
        for k in decl..at {
            match self.tok(k) {
                Some(Tok::Sym(Symbol::Function | Symbol::Do | Symbol::Then | Symbol::Repeat)) => depth += 1,
                Some(Tok::Sym(Symbol::End | Symbol::Until | Symbol::Else | Symbol::ElseIf)) => {
                    depth -= 1;
                    if depth < 0 {
                        return false;
                    }
                    if self.is_sym(k, Symbol::Else) {
                        depth += 1;
                    }
                }
                _ => {}
            }
        }
        true
    }

    /// The token at `k` follows something no statement starts after, like `{ a, b = 1 }` or `x = a, b`.
    fn in_expression(&self, k: usize) -> bool {
        k > 0 && (self.continues(k - 1) || matches!(self.tok(k - 1), Some(Tok::Sym(Symbol::Equal | Symbol::LeftBrace | Symbol::LeftParen | Symbol::Return | Symbol::In))))
    }

    /// A binary operator at `k`: the expression before it goes on.
    fn continues(&self, k: usize) -> bool {
        use Symbol::*;
        matches!(self.tok(k), Some(Tok::Sym(TwoDots | Or | And | Plus | Minus | Star | Slash | Caret | Percent)))
    }

    /// An element of the local table `name` read at `at`: fixed when it was built as
    /// `local name = { <literals> }` and is only ever read, counted or walked afterwards.
    fn table_origin(&self, name: &str, at: usize, depth: usize) -> Origin {
        let mut built = None;
        for j in (0..self.toks.len()).filter(|&j| self.is_ident(j, name) && !self.is_member(j)) {
            let local = j > 0 && self.is_sym(j - 1, Symbol::Local);
            if local && self.is_sym(j + 1, Symbol::Equal) && self.is_sym(j + 2, Symbol::LeftBrace) && j < at {
                built = self.in_scope(j, at).then_some(j + 2);
                continue;
            }
            let read = self.is_sym(j + 1, Symbol::LeftBracket) && self.closing(j + 1).is_some_and(|end| !self.is_sym(end + 1, Symbol::Equal));
            let counted = j >= 2 && self.is_sym(j - 1, Symbol::LeftParen) && self.is_sym(j + 1, Symbol::RightParen) && matches!(self.tok(j - 2), Some(Tok::Ident(f)) if TABLE_READERS.contains(&f.as_str()));
            if !(read || counted) {
                return Origin::Unknown;
            }
        }
        let Some(open) = built else { return Origin::Unknown };
        let Some(close) = self.closing(open) else { return Origin::Unknown };
        let literals = (open + 1..close).all(|k| matches!(self.tok(k), Some(Tok::Str(..) | Tok::Num(_) | Tok::Sym(Symbol::Comma | Symbol::Semicolon))));
        if literals && depth <= MAX_ORIGIN_DEPTH { Origin::Fixed } else { Origin::Unknown }
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
pub(crate) fn decode(raw: &str) -> String {
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

/// The longest run of `\ddd` or `\xHH` escapes of printable ASCII in a raw string: code to hide is
/// printable. Escapes from 128 on are UTF-8 text, like Cyrillic written as `\208\159`, and control
/// bytes are pattern data like `"[\000-\031]"`.
fn escape_run(raw: &str) -> usize {
    let bytes = raw.as_bytes();
    let (mut best, mut run, mut i) = (0, 0, 0);
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() && (bytes[i + 1].is_ascii_digit() || bytes[i + 1] == b'x') {
            let hex = bytes[i + 1] == b'x';
            let (from, max) = if hex { (i + 2, i + 4) } else { (i + 1, i + 4) };
            let mut j = from;
            while j < bytes.len() && j < max && if hex { bytes[j].is_ascii_hexdigit() } else { bytes[j].is_ascii_digit() } {
                j += 1;
            }
            let digits = std::str::from_utf8(&bytes[from..j]).unwrap_or_default();
            let value = if hex { u32::from_str_radix(digits, 16).ok() } else { digits.parse().ok() }.unwrap_or(0);
            run = if (32..127).contains(&value) { run + 1 } else { 0 };
            best = best.max(run);
            i = j.max(i + 2);
        } else if bytes[i] == b'\\' {
            // `\\1` in a texture path is a backslash and a digit, not an escape.
            run = 0;
            i += 2;
        } else {
            run = 0;
            i += 1;
        }
    }
    best
}

/// The tokens with their line and byte offset.
fn tokens(source: &str) -> Vec<(Tok, usize, usize)> {
    let tokens = match Lexer::new(source, LuaVersion::lua51()).collect() {
        LexerResult::Ok(tokens) | LexerResult::Recovered(tokens, _) => tokens,
        LexerResult::Fatal(_) => return Vec::new(),
    };
    tokens
        .into_iter()
        .filter_map(|t| {
            let (line, byte) = (t.start_position().line(), t.start_position().bytes());
            let tok = match t.token_type() {
                TokenType::Identifier { identifier } => Tok::Ident(identifier.to_string()),
                TokenType::StringLiteral { literal, multi_line_depth, .. } => {
                    let raw = literal.to_string();
                    let value = if *multi_line_depth == 0 && !raw.contains('\n') { decode(&raw) } else { raw.clone() };
                    Tok::Str(raw, value)
                }
                TokenType::Number { text } => Tok::Num(text.to_string()),
                TokenType::Symbol { symbol } => Tok::Sym(*symbol),
                _ => return None,
            };
            Some((tok, line, byte))
        })
        .collect()
}

/// Checks Lua `source`. `line_offset` shifts line numbers, for Lua inside an XML file.
pub fn check(path: &str, source: &[u8], line_offset: u32, report: &mut Report) {
    check_nested(path, source, line_offset, 0, report);
    report.dedupe();
}

/// UTF-16 and binary files are reported and get `None`; else the source as shipped, without a
/// BOM, and its Lua 5.0 rewrite.
fn prepare(path: &str, source: &[u8], report: &mut Report) -> Option<(String, String)> {
    if source.starts_with(b"\xff\xfe") || source.starts_with(b"\xfe\xff") {
        report.add("lua.encoding", Severity::Warn, path, None, "Saved as UTF-16; the game reads UTF-8, so the file cannot be checked or run");
        return None;
    }
    let source = source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source);
    if source.starts_with(b"\x1bLua") || source.contains(&0) {
        report.add("lua.binary", Severity::Critical, path, None, "Compiled Lua or binary data instead of source code");
        return None;
    }
    let original = String::from_utf8_lossy(source).into_owned();
    let text = lua50::to_lua51(&original);
    Some((original, text))
}

/// The code for the rules across files; `None` when it cannot be parsed.
pub(crate) fn unit(path: &str, source: &[u8], line_offset: u32) -> Option<lua_flow::Unit> {
    let (original, text) = prepare(path, source, &mut Report::default())?;
    let parsed = full_moon::parse_fallible(&text, LuaVersion::lua51());
    parsed.errors().is_empty().then(|| lua_flow::Unit::new(path, line_offset, &original, parsed.into_ast()))
}

fn check_nested(path: &str, source: &[u8], line_offset: u32, nested: usize, report: &mut Report) {
    let Some((original, text)) = prepare(path, source, report) else { return };
    if let Some((n, _)) = text.lines().enumerate().find(|(_, l)| l.chars().count() > MAX_LINE) {
        report.add("lua.long_line", Severity::Notice, path, Some(n as u32 + 1 + line_offset), format!("A line longer than {MAX_LINE} characters"));
    }
    let parsed = full_moon::parse_fallible(&text, LuaVersion::lua51());
    if let Some(error) = parsed.errors().first() {
        report.add("lua.parse_error", Severity::Warn, path, None, format!("The code cannot be analysed: {error}"));
    }
    // Code that does not parse gets no locals and no rules across statements: every name counts as
    // a global, and names of forbidden functions in strings stay critical.
    let unit = parsed.errors().is_empty().then(|| lua_flow::Unit::new(path, line_offset, &original, parsed.into_ast()));
    let no_locals = scope::Locals::default();
    let facts = match &unit {
        Some(unit) => lua_flow::check(std::slice::from_ref(unit), report).remove(0),
        None => lua_flow::Facts { computed: true, ..Default::default() },
    };
    let scope = unit.as_ref().map_or(&no_locals, |u| u.locals());
    let (mut toks, mut locals, mut typed) = (Vec::new(), Vec::new(), Vec::new());
    for (tok, line, byte) in tokens(&text) {
        locals.push(matches!(&tok, Tok::Ident(name) if scope.is_local(name, byte)));
        typed.push(facts.typed.contains(&byte));
        toks.push((tok, line));
    }
    let lua = Lua { path, lines: original.lines().collect(), offset: line_offset, toks, nested, computed: facts.computed, newindex: mentions_newindex(&original), typed, locals };
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
            // Only ASCII codes can hide code; bytes from 128 on are UTF-8 text like `·`.
            let mut numbers = 0;
            let mut j = i + 4;
            while let Some(Tok::Num(n)) = lua.tok(j) {
                if n.parse::<f64>().is_ok_and(|n| n < 128.0) {
                    numbers += 1;
                }
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
    if name == "AddMessage" && lua.is_member(i) && lua.is_sym(i - 1, Symbol::Dot) && lua.is_sym(i + 1, Symbol::Equal) {
        lua.add(report, "lua.api_override", Severity::Notice, i, "Replaces AddMessage of a chat frame, which can hide messages");
    }
    if lua.is_member(i) || lua.is_table_key(i) || lua.is_local(i) {
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
                // The code in the string runs too: scan it, on the line of the string.
                let at = if lua.is_str(i + 1) { i + 1 } else { i + 2 };
                if let (Some(Tok::Str(_, code)), true) = (lua.tok(at), lua.nested < MAX_NESTED_CODE) {
                    check_nested(lua.path, code.as_bytes(), lua.offset + lua.line(at) as u32 - 1, lua.nested + 1, report);
                }
            } else {
                match lua.call_arg_origin(i) {
                    Origin::Fixed => lua.add(report, "lua.dynamic_code", Severity::Warn, i, format!("{name} runs code put together from fixed text")),
                    Origin::Typed => lua.add(report, "lua.dynamic_code", Severity::Notice, i, format!("{name} runs code the player types, as /script does")),
                    Origin::Outside => lua.add(report, "lua.dynamic_code", Severity::Critical, i, format!("{name} runs code from chat or addon messages of other players")),
                    // Macros, profiles and generated handlers do this all the time: a look, not alarm.
                    Origin::Unknown => lua.add(report, "lua.dynamic_code", Severity::Warn, i, format!("{name} runs code that is built at run time")),
                }
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
        // `_G[this:GetName() .. "Text"]` is a frame's own name, as with getglobal.
        "_G" if lua.is_sym(i + 1, Symbol::LeftBracket)
            && !(lua.is_str(i + 2) && lua.is_sym(i + 3, Symbol::RightBracket))
            && lua.method_call_end(i + 2, "GetName").is_none() =>
        {
            let written = lua.closing(i + 1).is_some_and(|end| lua.is_sym(end + 1, Symbol::Equal));
            let severity = if written { Severity::Notice } else { Severity::Info };
            lua.add(report, "lua.dynamic_global", severity, i, "_G with a computed name");
        }
        "setmetatable" if lua.is_sym(i + 1, Symbol::LeftParen) && (lua.is_ident(i + 2, "_G") || lua.is_ident(i + 2, "getfenv")) => {
            let (severity, message) = env_metatable(lua.newindex);
            lua.add(report, "lua.global_metatable", severity, i, message);
        }
        "rawset" if lua.is_sym(i + 1, Symbol::LeftParen) && lua.is_ident(i + 2, "_G") && !(lua.is_sym(i + 3, Symbol::Comma) && lua.is_str(i + 4)) => {
            lua.add(report, "lua.dynamic_global", Severity::Notice, i, "rawset on _G with a computed name");
        }
        "CreateMacro" | "EditMacro" if lua.is_call(i) => {
            lua.add(report, "lua.macro", Severity::Notice, i, format!("{name} writes a macro the player may later run"));
        }
        _ => {}
    }
    if CHAT_SEND.contains(&name) {
        // Saving or testing the function (`local Orig = SendChatMessage`) sends nothing. A call is
        // what raid and boss addons are for; lua_flow warns when it passes on chat of other players
        // or hides behind another name.
        let declared = i > 0 && lua.is_sym(i - 1, Symbol::Function);
        let severity = if !lua.is_call(i) || declared { Severity::Info } else { Severity::Notice };
        lua.add(report, "lua.chat_send", severity, i, format!("{name} sends messages or invites in the player's name"));
    }
    let assigned = lua.is_sym(i + 1, Symbol::Equal) && !(i > 0 && lua.is_sym(i - 1, Symbol::Local));
    let declared = i > 0 && lua.is_sym(i - 1, Symbol::Function) && lua.is_sym(i + 1, Symbol::LeftParen);
    if is_api(name) && (assigned || declared) {
        // `local Orig = name` or `MyAddon_Orig = name` before: the usual hook that keeps the original.
        let kept = (1..lua.toks.len())
            .any(|j| lua.is_ident(j, name) && !lua.is_local(j) && lua.is_sym(j - 1, Symbol::Equal) && !lua.is_call(j) && !lua.is_sym(j + 1, Symbol::Dot));
        let (severity, message) = api_change(name, kept);
        lua.add(report, "lua.api_override", severity, i, message);
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
            // Where the string goes is followed in lua_flow; a string the code may still turn into a
            // global, through a lookup the scanner cannot follow, stays critical. The key of a table
            // (`t["os"]`, `{ ["load"] = 1 }`) is used up right there; lua_flow reports it on _G.
            // A string compared with another (`msg == "load"`, a slash command) is used up too.
            let key = i > 0 && lua.is_sym(i - 1, Symbol::LeftBracket) && lua.is_sym(j + 1, Symbol::RightBracket);
            let compared = [Symbol::TwoEqual, Symbol::TildeEqual].iter().any(|&s| (i > 0 && lua.is_sym(i - 1, s)) || lua.is_sym(j + 1, s));
            let severity = if lua.computed && !key && !compared { Severity::Critical } else { Severity::Info };
            lua.add(report, "lua.forbidden_name_string", severity, i, format!("The string \"{folded}\" names a forbidden function"));
        }
    }
    let lower = value.to_ascii_lowercase();
    if lower.split_whitespace().next().is_some_and(|w| SCRIPT_COMMANDS.contains(&w)) {
        lua.add(report, "lua.dynamic_code", Severity::Warn, i, "A /script or /run command, which runs Lua when it reaches a chat box");
    }
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
    // Base64 and hex are ASCII; Chinese or Korean text has no spaces either. A hex blob has letters
    // and digits, a long number only digits.
    if !value.is_ascii() {
        return false;
    }
    let hex = value.chars().all(|c| c.is_ascii_hexdigit())
        && value.chars().any(|c| c.is_ascii_digit())
        && value.chars().any(|c| c.is_ascii_alphabetic());
    hex || shannon(value) > ENTROPY_LIMIT
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
