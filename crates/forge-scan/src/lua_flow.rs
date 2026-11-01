//! Rules that follow values through a file and across the files of an addon: which global a local,
//! a table field or a global of the addon holds (`local g = _G; g.os`), names built from strings
//! (`getglobal(strlower("LOADSTRING"))`), and text from chat on its way to a chat function.
//!
//! The analysis ignores the order of statements: every value ever written to a variable counts.
//! That can miss a case, but never reports one that the code does not contain. The rules on plain
//! names (`loadstring(x)`, `os.exit()`) stay in `lua_rules`; this module reports what only shows
//! through another name.

use crate::finding::{Report, Severity};
use crate::lua_rules::{CHAT_SEND, FORBIDDEN_GLOBALS, FORBIDDEN_NAME_STRINGS, api_change, decode, env_metatable, is_api, mentions_newindex};
use crate::scope::{self, Locals};
use full_moon::ast::{self, Ast, Call, Expression, Field, FunctionArgs, Index, Parameter, Prefix, Suffix, Var};
use full_moon::node::Node;
use full_moon::tokenizer::{TokenReference, TokenType};
use full_moon::visitors::Visitor;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// Functions that run the code in a string; the first two are reached by name too.
const CODE_SINKS: &[&str] = &["loadstring", "RunScript", "SlashCmdList.SCRIPT", "SlashCmdList.RUN"];
/// Events that bring chat of other players; an addon that waits for one may pass it on.
const PLAYER_CHAT: &[&str] = &[
    "CHAT_MSG_WHISPER", "CHAT_MSG_SAY", "CHAT_MSG_YELL", "CHAT_MSG_CHANNEL", "CHAT_MSG_GUILD", "CHAT_MSG_OFFICER",
    "CHAT_MSG_PARTY", "CHAT_MSG_RAID", "CHAT_MSG_BATTLEGROUND", "CHAT_MSG_EMOTE",
];
/// Event arguments of 1.12 handlers: chat and addon messages arrive in them.
const OUTSIDE_VALUES: &[&str] = &["arg", "arg1", "arg2", "arg3", "arg4", "arg5", "arg6", "arg7", "arg8", "arg9"];
/// More values than this for one variable, and it counts as unknown.
const MAX_VALUES: usize = 8;
/// How deep values are followed (`a` from `b` from `c`).
const MAX_DEPTH: usize = 16;
/// Longer strings are not built: `string.rep("x", 1e9)` stays unknown.
const MAX_FOLDED: usize = 4096;

/// What an expression may hold.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Unknown,
    Str(String),
    /// A function or table of the game, like `loadstring` or `string.lower`.
    Global(String),
    /// The global table: `_G` or `getfenv()`.
    Env,
    /// Text another player controls: chat and addon messages.
    Tainted,
    /// What the player types: an input field or a slash command.
    Typed,
}

fn join(mut a: Vec<Val>, b: Vec<Val>) -> Vec<Val> {
    for v in b {
        if !a.contains(&v) {
            a.push(v);
        }
    }
    if a.len() > MAX_VALUES { vec![Val::Unknown] } else { a }
}

fn global_named<'v>(values: &'v [Val], names: &[&str]) -> Option<&'v str> {
    values.iter().find_map(|v| match v {
        Val::Global(n) if names.contains(&n.as_str()) => Some(n.as_str()),
        _ => None,
    })
}

fn only_str(values: &[Val]) -> Option<&str> {
    match values {
        [Val::Str(s)] => Some(s),
        _ => None,
    }
}

/// What the rules across statements found out about one unit, for the rules on tokens.
#[derive(Debug, Default)]
pub struct Facts {
    /// It looks up a global by a name the scanner cannot follow.
    pub computed: bool,
    /// Byte offsets of parameters that hold what the player types, like a slash command's text.
    pub typed: HashSet<usize>,
}

/// One piece of Lua: a file, or the code of an XML handler.
pub struct Unit {
    path: String,
    offset: u32,
    lines: Vec<String>,
    ast: Ast,
    locals: Locals,
}

impl Unit {
    /// `original` is the code as shipped, for snippets; `ast` the parse of its Lua 5.0 rewrite.
    pub fn new(path: &str, offset: u32, original: &str, ast: Ast) -> Unit {
        let locals = scope::locals(&ast);
        Unit { path: path.into(), offset, lines: original.lines().map(String::from).collect(), ast, locals }
    }

    pub fn locals(&self) -> &Locals {
        &self.locals
    }
}

fn start(token: &TokenReference) -> usize {
    token.token().start_position().bytes()
}

fn literal(token: &TokenReference) -> Option<String> {
    match token.token_type() {
        TokenType::StringLiteral { literal, multi_line_depth, .. } => {
            Some(if *multi_line_depth == 0 { decode(literal) } else { literal.to_string() })
        }
        TokenType::Number { text } => Some(text.to_string()),
        _ => None,
    }
}

/// The arguments of a call; a string or table argument is one expression.
fn arguments(args: &FunctionArgs) -> Vec<Expression> {
    match args {
        FunctionArgs::Parentheses { arguments, .. } => arguments.iter().cloned().collect(),
        FunctionArgs::String(token) => vec![Expression::String(token.clone())],
        FunctionArgs::TableConstructor(table) => vec![Expression::TableConstructor(table.clone())],
        _ => Vec::new(),
    }
}

/// A part of a `..` chain that builds a name.
enum Part {
    Literal(String),
    /// `x:GetName()`: the name of a frame, the start of everyday names like `this:GetName() .. "Text"`.
    FrameName,
    Other,
}

/// What a global looked up by a computed name may be.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Risk {
    /// None of the checked names: it starts with a frame name, or its literal parts fit none of
    /// them, like `"Button" .. i`.
    Harmless,
    /// Nothing about the name is known, like a handler Ace2 found in _G.
    Unknown,
    /// Built toward a checked name (`a .. "Script"`), or from text of other players.
    Suspicious,
}

fn name_risk(key: Option<&Expression>, value: &[Val]) -> Risk {
    if value.contains(&Val::Tainted) {
        return Risk::Suspicious;
    }
    fn parts(e: &Expression, out: &mut Vec<Part>) {
        match e {
            Expression::BinaryOperator { lhs, binop: ast::BinOp::TwoDots(_), rhs } => {
                parts(lhs, out);
                parts(rhs, out);
            }
            Expression::Parentheses { expression, .. } => parts(expression, out),
            Expression::String(t) => out.push(literal(t).map_or(Part::Other, Part::Literal)),
            Expression::FunctionCall(call) => {
                let frame = matches!(call.suffixes().last(), Some(Suffix::Call(Call::MethodCall(m))) if m.name().token().to_string() == "GetName");
                out.push(if frame { Part::FrameName } else { Part::Other });
            }
            _ => out.push(Part::Other),
        }
    }
    let Some(key) = key else { return Risk::Unknown };
    let mut found = Vec::new();
    parts(key, &mut found);
    if matches!(found.first(), Some(Part::FrameName)) {
        return Risk::Harmless;
    }
    if !found.iter().any(|p| matches!(p, Part::Literal(s) if !s.is_empty())) {
        return Risk::Unknown;
    }
    let last = found.len() - 1;
    let fits = |name: &str| {
        found.iter().enumerate().all(|(i, p)| match p {
            Part::Literal(s) if i == 0 => name.starts_with(s.as_str()),
            Part::Literal(s) if i == last => name.ends_with(s.as_str()),
            Part::Literal(s) => name.contains(s.as_str()),
            _ => true,
        })
    };
    if FORBIDDEN_NAME_STRINGS.iter().chain(CHAT_SEND).any(|n| fits(n)) { Risk::Suspicious } else { Risk::Harmless }
}

/// Where a value can be written: `L<unit>:<binding>` for a local, `G:<name>` for a global, and
/// `.<key>` per field below one of them.
type Slot = String;

/// A write: the unit and the value, or `None` for a value nobody can know.
type Def = (usize, Option<Expression>);

struct Program<'u> {
    units: &'u [Unit],
    defs: HashMap<Slot, Vec<Def>>,
    /// Globals an addon kept in a local: replacing them is the usual hook.
    kept: HashSet<String>,
    /// Some file waits for chat of other players, so event arguments may be their messages.
    player_chat: bool,
    /// Some file names `__newindex`: a metatable on _G may then see every global set.
    newindex: bool,
    /// Parameters of slash command handlers: what the player types.
    typed: HashSet<Slot>,
    /// Functions given to `SlashCmdList` by name.
    handlers: HashSet<Slot>,
    /// Per unit, the byte offsets of those parameters.
    typed_at: Vec<HashSet<usize>>,
    memo: RefCell<HashMap<Slot, Vec<Val>>>,
    busy: RefCell<HashSet<Slot>>,
    /// Per unit: it looks up a global by a name the scanner cannot follow.
    computed: RefCell<Vec<bool>>,
}

impl Program<'_> {
    fn name_slot(&self, unit: usize, token: &TokenReference) -> Slot {
        let name = token.token().to_string();
        match self.units[unit].locals.binding(&name, start(token)) {
            Some(id) => format!("L{unit}:{id}"),
            None => format!("G:{name}"),
        }
    }

    fn def(&mut self, slot: Slot, unit: usize, value: Option<&Expression>) {
        if let Some(Expression::TableConstructor(table)) = value {
            let mut n = 0;
            for field in table.fields() {
                let (key, value) = match field {
                    Field::NameKey { key, value, .. } => (Some(key.token().to_string()), value.as_ref()),
                    Field::ExpressionKey { key, value, .. } => match key.as_ref() {
                        Expression::String(t) | Expression::Number(t) => (literal(t), value.as_ref()),
                        _ => continue,
                    },
                    Field::NoKey(value) => {
                        n += 1;
                        (Some(n.to_string()), value)
                    }
                    _ => continue,
                };
                if let Some(key) = key {
                    self.def(format!("{slot}.{key}"), unit, Some(value));
                }
            }
        }
        self.defs.entry(slot).or_default().push((unit, value.cloned()));
    }

    /// The slot of `a.b["c"]` when every key is a constant and `a` is a name; `_G.x` is the global `x`.
    fn var_slot(&self, unit: usize, var: &Var) -> Option<Slot> {
        let ve = match var {
            Var::Name(token) => return Some(self.name_slot(unit, token)),
            Var::Expression(ve) => ve,
            _ => return None,
        };
        let Prefix::Name(base) = ve.prefix() else { return None };
        let mut slot = self.name_slot(unit, base);
        for suffix in ve.suffixes() {
            let key = match suffix {
                Suffix::Index(Index::Dot { name, .. }) => name.token().to_string(),
                Suffix::Index(Index::Brackets { expression, .. }) => match expression.as_ref() {
                    Expression::String(t) => literal(t)?,
                    _ => return None,
                },
                _ => return None,
            };
            slot = if slot == "G:_G" { format!("G:{key}") } else { format!("{slot}.{key}") };
        }
        Some(slot)
    }

    fn slot(&self, slot: &Slot, depth: usize) -> Vec<Val> {
        if let Some(v) = self.memo.borrow().get(slot) {
            return v.clone();
        }
        if self.typed.contains(slot) {
            return vec![Val::Typed];
        }
        if depth > MAX_DEPTH || !self.busy.borrow_mut().insert(slot.clone()) {
            return vec![Val::Unknown];
        }
        let mut values = Vec::new();
        for (unit, value) in self.defs.get(slot).into_iter().flatten() {
            let v = match value {
                Some(e) => self.eval(*unit, e, depth + 1),
                None => vec![Val::Unknown],
            };
            values = join(values, v);
        }
        // A local nobody writes is a parameter or a loop variable.
        if values.is_empty() && slot.starts_with('L') && !slot.contains('.') {
            values.push(Val::Unknown);
        }
        self.busy.borrow_mut().remove(slot);
        self.memo.borrow_mut().insert(slot.clone(), values.clone());
        values
    }

    fn name(&self, unit: usize, token: &TokenReference, depth: usize) -> Vec<Val> {
        let slot = self.name_slot(unit, token);
        let Some(name) = slot.strip_prefix("G:") else { return self.slot(&slot, depth) };
        let builtin = match name {
            "_G" => Val::Env,
            _ if OUTSIDE_VALUES.contains(&name) => Val::Tainted,
            _ => Val::Global(name.into()),
        };
        join(vec![builtin], self.slot(&slot, depth))
    }

    fn eval(&self, unit: usize, e: &Expression, depth: usize) -> Vec<Val> {
        if depth > MAX_DEPTH {
            return vec![Val::Unknown];
        }
        match e {
            Expression::String(t) | Expression::Number(t) => literal(t).map_or(vec![Val::Unknown], |s| vec![Val::Str(s)]),
            Expression::Parentheses { expression, .. } => self.eval(unit, expression, depth),
            Expression::BinaryOperator { lhs, binop: ast::BinOp::TwoDots(_), rhs } => {
                let (l, r) = (self.eval(unit, lhs, depth + 1), self.eval(unit, rhs, depth + 1));
                if l.contains(&Val::Tainted) || r.contains(&Val::Tainted) {
                    return vec![Val::Tainted];
                }
                // `"print(" .. msg .. ")"` with `msg` typed: still what the player typed.
                let text = |v: &[Val]| v.iter().all(|v| matches!(v, Val::Str(_) | Val::Typed));
                if (l.contains(&Val::Typed) || r.contains(&Val::Typed)) && text(&l) && text(&r) {
                    return vec![Val::Typed];
                }
                match (only_str(&l), only_str(&r)) {
                    (Some(a), Some(b)) if a.len() + b.len() <= MAX_FOLDED => vec![Val::Str(format!("{a}{b}"))],
                    _ => vec![Val::Unknown],
                }
            }
            Expression::Var(Var::Name(t)) => self.name(unit, t, depth),
            Expression::Var(Var::Expression(ve)) => self.chain(unit, ve.prefix(), ve.suffixes(), depth, None),
            Expression::FunctionCall(call) => self.chain(unit, call.prefix(), call.suffixes(), depth, None),
            _ => vec![Val::Unknown],
        }
    }

    /// The value of `prefix` with its suffixes, like `getfenv(0)["os"].exit`; with `report`, also
    /// reports what the chain reaches on the way.
    fn chain<'s>(
        &self,
        unit: usize,
        prefix: &Prefix,
        suffixes: impl Iterator<Item = &'s Suffix>,
        depth: usize,
        mut report: Option<&mut Report>,
    ) -> Vec<Val> {
        // `loadstring(x)` itself: the rules of lua_rules report it under its own name.
        let (mut values, mut slot, mut direct) = match prefix {
            Prefix::Name(t) => {
                let slot = self.name_slot(unit, t);
                let direct = slot.strip_prefix("G:").map(String::from);
                (self.name(unit, t, depth + 1), Some(slot), direct)
            }
            Prefix::Expression(e) => (self.eval(unit, e, depth + 1), None, None),
            _ => (vec![Val::Unknown], None, None),
        };
        // The name the function is called by: `InviteByName` for a local copy under the same name,
        // `SendChatMessage` for `_G.SendChatMessage`.
        let mut called_as = match prefix {
            Prefix::Name(t) => Some(t.token().to_string()),
            _ => None,
        };
        // The value before is a global looked up by a name the scanner cannot follow.
        let mut computed = Risk::Harmless;
        for suffix in suffixes {
            let line = suffix.start_position().map_or(0, |p| p.line());
            let mut looked_up = Risk::Harmless;
            match suffix {
                Suffix::Index(index) => {
                    let (key, key_expr, key_value) = match index {
                        Index::Dot { name, .. } => (Some(name.token().to_string()), None, Vec::new()),
                        Index::Brackets { expression, .. } => {
                            let value = self.eval(unit, expression, depth + 1);
                            (only_str(&value).map(String::from), Some(expression.as_ref()), value)
                        }
                        _ => (None, None, Vec::new()),
                    };
                    let mut next = Vec::new();
                    for v in &values {
                        let to = match (v, &key) {
                            (Val::Env, Some(k)) => {
                                if let Some(r) = report.as_deref_mut() {
                                    self.reached(r, unit, line, k, "_G or getfenv");
                                }
                                Val::Global(k.clone())
                            }
                            (Val::Env, None) => {
                                looked_up = name_risk(key_expr, &key_value);
                                Val::Unknown
                            }
                            (Val::Global(base), Some(k)) => Val::Global(format!("{base}.{k}")),
                            (Val::Tainted, _) => Val::Tainted,
                            _ => Val::Unknown,
                        };
                        next = join(next, vec![to]);
                    }
                    slot = match (slot, &key) {
                        (Some(s), Some(k)) => {
                            let s = if s == "G:_G" { format!("G:{k}") } else { format!("{s}.{k}") };
                            next = join(next, self.slot(&s, depth + 1));
                            Some(s)
                        }
                        _ => None,
                    };
                    // `t.x` where `t` is a table of the addon: only its fields count.
                    if next.len() > 1 {
                        next.retain(|v| *v != Val::Unknown);
                    }
                    values = next;
                }
                Suffix::Call(Call::AnonymousCall(args)) => {
                    let exprs = arguments(args);
                    let args: Vec<Vec<Val>> = exprs.iter().map(|a| self.eval(unit, a, depth + 1)).collect();
                    if let Some(r) = report.as_deref_mut() {
                        self.sinks(r, unit, line, &values, &args, (direct.as_deref(), called_as.as_deref()), computed);
                    }
                    let (mut result, built) = self.call(&values, &args);
                    // `table.concat({"load", "string"})`: the table is only syntax here.
                    if global_named(&values, &["table.concat"]).is_some()
                        && let Some(Expression::TableConstructor(table)) = exprs.first()
                    {
                        let sep = match args.get(1) {
                            None => Some(""),
                            Some(a) => only_str(a),
                        };
                        if let Some(joined) = sep.and_then(|sep| concat_literals(table, sep)) {
                            result = vec![Val::Str(joined)];
                        }
                    }
                    if let (Some(r), Some(k)) = (report.as_deref_mut(), &built) {
                        self.reached(r, unit, line, k, "a lookup by name");
                    }
                    let name_at = match global_named(&values, &["getglobal", "rawget"]) {
                        Some("getglobal") => Some(0),
                        Some(_) if args.first().is_some_and(|a| a.contains(&Val::Env)) => Some(1),
                        _ => None,
                    };
                    if let Some(i) = name_at.filter(|_| built.is_none()) {
                        looked_up = name_risk(exprs.get(i), args.get(i).map_or(&[], Vec::as_slice));
                    }
                    values = result;
                    slot = None;
                }
                Suffix::Call(Call::MethodCall(m)) => {
                    values = if m.name().token().to_string() == "GetText" { vec![Val::Typed] } else { vec![Val::Unknown] };
                    slot = None;
                }
                _ => values = vec![Val::Unknown],
            }
            if looked_up != Risk::Harmless && report.is_some() {
                self.computed.borrow_mut()[unit] = true;
            }
            computed = looked_up;
            direct = None;
            called_as = match suffix {
                Suffix::Index(Index::Dot { name, .. }) => Some(name.token().to_string()),
                _ => None,
            };
        }
        values
    }

    /// What a call of `callee` returns, and the name it looked up, for `getglobal("x")`.
    fn call(&self, callee: &[Val], args: &[Vec<Val>]) -> (Vec<Val>, Option<String>) {
        let arg = |i: usize| args.get(i).and_then(|a| only_str(a));
        let strs: Option<Vec<&str>> = args.iter().map(|a| only_str(a)).collect();
        let mut out = Vec::new();
        let mut built = None;
        for v in callee {
            let Val::Global(f) = v else {
                out = join(out, vec![Val::Unknown]);
                continue;
            };
            let name = match f.as_str() {
                "getglobal" => arg(0),
                "rawget" if args.first().is_some_and(|a| a.contains(&Val::Env)) => arg(1),
                _ => None,
            };
            let value = if f == "getfenv" {
                Val::Env
            } else if let Some(k) = name {
                built = Some(k.to_string());
                if k == "_G" { Val::Env } else { Val::Global(k.into()) }
            } else if let Some(s) = strs.as_deref().and_then(|s| fold(f, s)) {
                Val::Str(s)
            } else if fold_name(f) && args.iter().any(|a| a.contains(&Val::Tainted)) {
                Val::Tainted
            } else {
                Val::Unknown
            };
            out = join(out, vec![value]);
        }
        (out, built)
    }

    /// A global reached by name or through the environment.
    fn reached(&self, report: &mut Report, unit: usize, line: usize, name: &str, how: &str) {
        if FORBIDDEN_GLOBALS.contains(&name) {
            self.add(report, unit, line, "lua.forbidden_global", Severity::Critical, format!("{name} is not allowed in addons, reached through {how}"));
        } else if CODE_SINKS[..2].contains(&name) {
            self.add(report, unit, line, "lua.dynamic_code", Severity::Critical, format!("{name}, reached through {how}, runs code from strings"));
        } else if CHAT_SEND.contains(&name) {
            self.add(report, unit, line, "lua.chat_send", Severity::Info, format!("{name}, reached through {how}"));
        }
    }

    #[allow(clippy::too_many_arguments)]
    /// `names` are the global the call names itself, if plain, and the name it is called by.
    fn sinks(&self, report: &mut Report, unit: usize, line: usize, callee: &[Val], args: &[Vec<Val>], names: (Option<&str>, Option<&str>), computed: Risk) {
        let (direct, called_as) = names;
        let arg = |i: usize| args.get(i).map(Vec::as_slice).unwrap_or(&[]);
        let severity = match computed {
            Risk::Harmless => None,
            Risk::Unknown => Some(Severity::Notice),
            Risk::Suspicious => Some(Severity::Warn),
        };
        if let Some(severity) = severity {
            self.add(report, unit, line, "lua.dynamic_global", severity, "Calls a global whose name is computed at run time".into());
        }
        if let Some(name) = global_named(callee, CODE_SINKS).filter(|n| Some(*n) != direct) {
                let (severity, what) = match arg(0) {
                    [Val::Str(_)] => (Severity::Warn, "code from a string"),
                    a if a.contains(&Val::Tainted) => (Severity::Critical, "code from chat or addon messages of other players"),
                    a if !a.is_empty() && a.iter().all(|v| matches!(v, Val::Str(_) | Val::Typed)) => (Severity::Notice, "code the player types"),
                    _ => (Severity::Warn, "code that is built at run time"),
                };
                self.add(report, unit, line, "lua.dynamic_code", severity, format!("{name}, under another name, runs {what}"));
        }
        if let Some(name) = global_named(callee, CHAT_SEND).filter(|n| Some(*n) != direct) {
            // A hook that replaced the function calls the original it kept: it sends what the
            // player sent.
            let hook = self.kept.contains(name) && self.defs.contains_key(&format!("G:{name}"));
            let severity = if hook {
                Severity::Info
            } else if name == "SendAddonMessage" || called_as == Some(name) {
                Severity::Notice
            } else {
                Severity::Warn
            };
            let message = format!("{name}, under another name, sends messages or invites in the player's name");
            self.add(report, unit, line, "lua.chat_send", severity, message);
        }
        if let Some(name) = global_named(callee, &["SendChatMessage", "SendAddonMessage"])
            && self.player_chat
            // The text: `SendChatMessage(text, ...)`, `SendAddonMessage(prefix, text, ...)`.
            && arg(usize::from(name == "SendAddonMessage")).contains(&Val::Tainted)
        {
            let message = format!("{name} passes on text from an event, in an addon that reads the chat of other players");
            self.add(report, unit, line, "lua.chat_send", Severity::Warn, message);
        }
        match global_named(callee, &["setmetatable", "rawset", "setglobal"]) {
            Some("setmetatable") if arg(0).contains(&Val::Env) => {
                let (severity, message) = env_metatable(self.newindex);
                self.add(report, unit, line, "lua.global_metatable", severity, message.into());
            }
            Some("rawset") if arg(0).contains(&Val::Env) => self.replaced(report, unit, line, only_str(arg(1))),
            Some("setglobal") if only_str(arg(0)).is_some() => self.replaced(report, unit, line, only_str(arg(0))),
            _ => {}
        }
    }

    /// A global written by name: a game function an addon should hook instead, or any name.
    fn replaced(&self, report: &mut Report, unit: usize, line: usize, name: Option<&str>) {
        match name {
            Some(n) if is_api(n) => {
                let (severity, message) = api_change(n, self.kept.contains(n));
                self.add(report, unit, line, "lua.api_override", severity, message);
            }
            Some(_) => {}
            None => self.add(report, unit, line, "lua.dynamic_global", Severity::Notice, "Writes a global whose name is computed at run time".into()),
        }
    }

    fn add(&self, report: &mut Report, unit: usize, line: usize, rule: &str, severity: Severity, message: String) {
        let u = &self.units[unit];
        let snippet = u.lines.get(line.saturating_sub(1)).map_or("", String::as_str);
        report.add_with_snippet(rule, severity, &u.path, Some(line as u32 + u.offset), message, snippet);
    }
}

/// The string functions that are folded, under their 1.12 names too.
fn fold_name(f: &str) -> bool {
    matches!(
        f,
        "string.lower" | "strlower" | "string.upper" | "strupper" | "string.reverse" | "strrev" | "string.rep" | "strrep"
            | "string.sub" | "strsub" | "string.format" | "format" | "string.char" | "strchar" | "table.concat"
    )
}

/// The result of a string function on constants; `None` when it is not one or the result is long.
fn fold(f: &str, args: &[&str]) -> Option<String> {
    let num = |i: usize| args.get(i).and_then(|a| a.parse::<f64>().ok()).map(|n| n as i64);
    let s = *args.first()?;
    let out = match f {
        "string.lower" | "strlower" => s.to_lowercase(),
        "string.upper" | "strupper" => s.to_uppercase(),
        "string.reverse" | "strrev" => s.chars().rev().collect(),
        "string.rep" | "strrep" => {
            let n = num(1)?.max(0) as usize;
            if s.len().saturating_mul(n) > MAX_FOLDED {
                return None;
            }
            s.repeat(n)
        }
        "string.sub" | "strsub" => {
            let len = s.len() as i64;
            let at = |n: i64| if n < 0 { (len + n + 1).max(1) } else { n.max(1) };
            let (from, to) = (at(num(1).unwrap_or(1)), at(num(2).unwrap_or(-1)).min(len));
            if from > to { String::new() } else { s.get(from as usize - 1..to as usize)?.to_string() }
        }
        "string.format" | "format" => {
            let mut out = String::new();
            let mut rest = args[1..].iter();
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match chars.next() {
                    Some('s' | 'd') => out.push_str(rest.next()?),
                    Some('%') => out.push('%'),
                    _ => return None,
                }
            }
            out
        }
        "string.char" | "strchar" => (0..args.len()).map(|i| num(i).and_then(|n| u8::try_from(n).ok()).map(char::from)).collect::<Option<String>>()?,
        _ => return None,
    };
    (out.len() <= MAX_FOLDED).then_some(out)
}

/// `table.concat({"a", "b"}, sep)` of literals: a call whose argument is a table, so `fold` cannot see it.
fn concat_literals(table: &ast::TableConstructor, sep: &str) -> Option<String> {
    let parts: Option<Vec<String>> = table
        .fields()
        .iter()
        .map(|f| match f {
            Field::NoKey(Expression::String(t)) => literal(t),
            _ => None,
        })
        .collect();
    let out = parts?.join(sep);
    (out.len() <= MAX_FOLDED).then_some(out)
}

/// First pass: every write to a variable, field or global.
struct Writes<'p, 'u> {
    program: &'p mut Program<'u>,
    unit: usize,
}

impl Visitor for Writes<'_, '_> {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let exprs: Vec<&Expression> = node.expressions().iter().collect();
        for (k, name) in node.names().iter().enumerate() {
            let slot = self.program.name_slot(self.unit, name);
            match exprs.get(k) {
                Some(e) => {
                    if let Expression::Var(Var::Name(t)) = e
                        && self.program.name_slot(self.unit, t).starts_with("G:")
                    {
                        self.program.kept.insert(t.token().to_string());
                    }
                    self.program.def(slot, self.unit, Some(e));
                }
                // `local a, b = f()`: whatever f returns.
                None if !exprs.is_empty() => self.program.def(slot, self.unit, None),
                None => {}
            }
        }
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        let exprs: Vec<&Expression> = node.expressions().iter().collect();
        for (k, var) in node.variables().iter().enumerate() {
            if let Some(slot) = self.program.var_slot(self.unit, var) {
                // `MyAddon_Orig = SendChatMessage` or `= _G.SendChatMessage`: an original kept for a hook.
                if let Some(Expression::Var(var)) = exprs.get(k)
                    && let Some(name) = self.program.var_slot(self.unit, var).and_then(|s| s.strip_prefix("G:").map(String::from))
                {
                    self.program.kept.insert(name);
                }
                self.program.def(slot, self.unit, exprs.get(k).copied());
            }
        }
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        // `function a.b:c()` writes a function nobody follows into `a.b.c`.
        let mut names = node.name().names().iter().chain(node.name().method_name());
        let Some(first) = names.next() else { return };
        let slot = names.fold(self.program.name_slot(self.unit, first), |slot, n| format!("{slot}.{}", n.token()));
        self.program.def(slot, self.unit, None);
    }

    fn visit_function_call(&mut self, node: &ast::FunctionCall) {
        // `setglobal("Name", v)` and `rawset(_G, "Name", v)` write a global by a fixed name.
        let (Prefix::Name(f), Some(Suffix::Call(Call::AnonymousCall(args)))) = (node.prefix(), node.suffixes().next()) else { return };
        let args = arguments(args);
        let (name, value) = match (f.token().to_string().as_str(), args.as_slice()) {
            ("setglobal", [Expression::String(k), v, ..]) => (literal(k), v),
            ("rawset", [Expression::Var(Var::Name(g)), Expression::String(k), v, ..]) if g.token().to_string() == "_G" => (literal(k), v),
            _ => return,
        };
        if let Some(name) = name {
            self.program.def(format!("G:{name}"), self.unit, Some(value));
        }
    }
}

/// Second pass: the calls, lookups and writes the rules look at.
struct Reads<'p, 'u> {
    program: &'p Program<'u>,
    report: &'p mut Report,
    unit: usize,
}

impl Visitor for Reads<'_, '_> {
    fn visit_function_call(&mut self, node: &ast::FunctionCall) {
        self.program.chain(self.unit, node.prefix(), node.suffixes(), 0, Some(self.report));
    }

    fn visit_var_expression(&mut self, node: &ast::VarExpression) {
        self.program.chain(self.unit, node.prefix(), node.suffixes(), 0, Some(self.report));
    }

    fn visit_assignment(&mut self, node: &ast::Assignment) {
        // `_G.Name = f` or `g["Name"] = f` with `g` holding _G.
        for var in node.variables().iter() {
            let Var::Expression(ve) = var else { continue };
            let suffixes: Vec<&Suffix> = ve.suffixes().collect();
            let Some((Suffix::Index(last), before)) = suffixes.split_last() else { continue };
            let base = self.program.chain(self.unit, ve.prefix(), before.iter().copied(), 0, None);
            if !base.contains(&Val::Env) {
                continue;
            }
            let key = match last {
                Index::Dot { name, .. } => Some(name.token().to_string()),
                Index::Brackets { expression, .. } => only_str(&self.program.eval(self.unit, expression, 0)).map(String::from),
                _ => continue,
            };
            let line = last.start_position().map_or(0, |p| p.line());
            self.program.replaced(self.report, self.unit, line, key.as_deref());
        }
    }
}

/// Runs the rules over `units`, which share one global table, and returns per unit whether it
/// looks up a global by a name the scanner cannot follow.
pub fn check(units: &[Unit], report: &mut Report) -> Vec<Facts> {
    let mut program = Program {
        units,
        defs: HashMap::new(),
        kept: HashSet::new(),
        player_chat: units.iter().any(|u| u.lines.iter().any(|l| PLAYER_CHAT.iter().any(|e| l.contains(e)))),
        newindex: units.iter().any(|u| u.lines.iter().any(|l| mentions_newindex(l))),
        typed: HashSet::new(),
        handlers: HashSet::new(),
        typed_at: vec![HashSet::new(); units.len()],
        memo: RefCell::default(),
        busy: RefCell::default(),
        computed: RefCell::new(vec![false; units.len()]),
    };
    // Slash command handlers first: given as a function, then the ones given by name.
    for (unit, u) in units.iter().enumerate() {
        Handlers { program: &mut program, unit, by_name: false }.visit_ast(&u.ast);
    }
    for (unit, u) in units.iter().enumerate() {
        Handlers { program: &mut program, unit, by_name: true }.visit_ast(&u.ast);
    }
    for (unit, u) in units.iter().enumerate() {
        Writes { program: &mut program, unit }.visit_ast(&u.ast);
    }
    for (unit, u) in units.iter().enumerate() {
        Reads { program: &program, report, unit }.visit_ast(&u.ast);
    }
    let typed_at = std::mem::take(&mut program.typed_at);
    program.computed.into_inner().into_iter().zip(typed_at).map(|(computed, typed)| Facts { computed, typed }).collect()
}

/// Finds the functions that handle slash commands: `SlashCmdList.X = function(msg)`, a function
/// given there by name, and Ace2's `self:RegisterChatCommand({...}, function(text) ... end)`.
/// Their first parameter holds what the player typed.
struct Handlers<'p, 'u> {
    program: &'p mut Program<'u>,
    unit: usize,
    /// The second pass: functions declared under a name given to `SlashCmdList`.
    by_name: bool,
}

impl Handlers<'_, '_> {
    fn mark(&mut self, body: &ast::FunctionBody) {
        if let Some(Parameter::Name(name)) = body.parameters().iter().next() {
            let slot = self.program.name_slot(self.unit, name);
            self.program.typed.insert(slot);
            self.program.typed_at[self.unit].insert(start(name));
        }
    }

    fn handled_by(&mut self, value: &Expression) {
        match value {
            Expression::Function(f) => self.mark(f.body()),
            Expression::Var(Var::Name(t)) => {
                let slot = self.program.name_slot(self.unit, t);
                self.program.handlers.insert(slot);
            }
            _ => {}
        }
    }
}

impl Visitor for Handlers<'_, '_> {
    fn visit_assignment(&mut self, node: &ast::Assignment) {
        if self.by_name {
            return;
        }
        for (var, value) in node.variables().iter().zip(node.expressions().iter()) {
            if self.program.var_slot(self.unit, var).is_some_and(|s| s.starts_with("G:SlashCmdList.")) {
                self.handled_by(value);
            }
        }
    }

    fn visit_function_call(&mut self, node: &ast::FunctionCall) {
        if self.by_name {
            return;
        }
        for suffix in node.suffixes() {
            if let Suffix::Call(Call::MethodCall(m)) = suffix
                && m.name().token().to_string() == "RegisterChatCommand"
            {
                for arg in arguments(m.args()) {
                    self.handled_by(&arg);
                }
            }
        }
    }

    fn visit_function_declaration(&mut self, node: &ast::FunctionDeclaration) {
        let names: Vec<&TokenReference> = node.name().names().iter().collect();
        if self.by_name && names.len() == 1 && node.name().method_name().is_none() {
            let slot = self.program.name_slot(self.unit, names[0]);
            if self.program.handlers.contains(&slot) {
                self.mark(node.body());
            }
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        if self.by_name && self.program.handlers.contains(&self.program.name_slot(self.unit, node.name())) {
            self.mark(node.body());
        }
    }

    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        if !self.by_name {
            return;
        }
        for (name, value) in node.names().iter().zip(node.expressions().iter()) {
            if let Expression::Function(f) = value
                && self.program.handlers.contains(&self.program.name_slot(self.unit, name))
            {
                self.mark(f.body());
            }
        }
    }
}
