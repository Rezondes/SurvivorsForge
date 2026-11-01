//! Which names in a Lua file are locals: declarations, parameters, loop variables, and every read
//! or write of a name inside the scope of such a declaration. Everything else is a global.

use full_moon::ast::{self, Ast, Parameter};
use full_moon::node::Node;
use full_moon::tokenizer::TokenReference;
use full_moon::visitors::Visitor;
use std::collections::HashMap;

/// Byte offsets into the source.
type Range = (usize, usize);

/// Numbers the declarations of a file, in the order they appear.
pub type Binding = usize;

#[derive(Default)]
pub struct Locals {
    /// Tokens that declare a local.
    declarations: HashMap<usize, Binding>,
    /// Per name, sorted by position: from there on, the declaration a use of the name reads.
    visible: HashMap<String, Vec<(usize, Option<Binding>)>>,
}

impl Locals {
    /// The name at byte `at` is a local there, or declares one.
    pub fn is_local(&self, name: &str, at: usize) -> bool {
        self.binding(name, at).is_some()
    }

    /// The declaration the name at byte `at` reads or writes; the innermost, latest one wins.
    pub fn binding(&self, name: &str, at: usize) -> Option<Binding> {
        if let Some(&id) = self.declarations.get(&at) {
            return Some(id);
        }
        let segments = self.visible.get(name)?;
        let n = segments.partition_point(|&(from, _)| from <= at);
        n.checked_sub(1).and_then(|i| segments[i].1)
    }
}

/// The locals of `ast`. A local is visible from the end of its statement (`local x = x` reads the
/// global `x`) to the end of the innermost block around it; `repeat` blocks reach to the end of
/// their `until` condition.
pub fn locals(ast: &Ast) -> Locals {
    let mut walk = Walk { blocks: vec![(0, usize::MAX)], ..Walk::default() };
    walk.visit_ast(ast);
    // One sweep in source order. Blocks nest, so the innermost block open where a local becomes
    // visible is the top of the stack; at one position, blocks end first, then open, then locals
    // start.
    let mut events: Vec<(usize, u8, usize)> = Vec::new();
    for (i, &(s, e)) in walk.blocks.iter().enumerate() {
        events.push((s, 1, i));
        events.push((e, 0, i));
    }
    for (id, &(_, from, _)) in walk.bindings.iter().enumerate() {
        events.push((from, 2, id));
    }
    events.sort_unstable();
    let mut open: Vec<usize> = Vec::new();
    let mut ranges: HashMap<&str, Vec<(Range, Binding)>> = HashMap::new();
    for (_, kind, i) in events {
        match kind {
            0 => {
                if let Some(k) = open.iter().rposition(|&b| b == i) {
                    open.remove(k);
                }
            }
            1 => open.push(i),
            _ => {
                let (name, from, _) = &walk.bindings[i];
                let to = open.last().map_or(usize::MAX, |&b| walk.blocks[b].1);
                ranges.entry(name.as_str()).or_default().push(((*from, to), i));
            }
        }
    }
    let mut locals = Locals::default();
    for (id, (_, _, token)) in walk.bindings.iter().enumerate() {
        locals.declarations.insert(*token, id);
    }
    for (name, list) in ranges {
        locals.visible.insert(name.to_string(), segments(list));
    }
    locals
}

/// The ranges of one name, nested or apart, as segments: from each position on, the innermost,
/// latest declaration that covers it.
fn segments(ranges: Vec<(Range, Binding)>) -> Vec<(usize, Option<Binding>)> {
    // At one position, ends come before starts: a range does not cover its end. Ranges that end
    // together close from the top of the stack down, the latest first.
    let mut events: Vec<(usize, u8, Binding)> = Vec::new();
    for ((from, to), id) in ranges {
        events.push((from, 1, id));
        events.push((to, 0, id));
    }
    events.sort_unstable_by_key(|&(at, kind, id)| (at, kind, if kind == 0 { usize::MAX - id } else { id }));
    let mut stack: Vec<Binding> = Vec::new();
    let mut out: Vec<(usize, Option<Binding>)> = Vec::new();
    for (at, kind, id) in events {
        if kind == 0 {
            if let Some(k) = stack.iter().rposition(|&b| b == id) {
                stack.remove(k);
            }
        } else {
            stack.push(id);
        }
        let now = stack.last().copied();
        match out.last_mut() {
            Some(last) if last.0 == at => last.1 = now,
            _ => out.push((at, now)),
        }
    }
    out
}

#[derive(Default)]
struct Walk {
    blocks: Vec<Range>,
    /// Name, where it becomes visible, and the declaring token.
    bindings: Vec<(String, usize, usize)>,
}

fn start(token: &TokenReference) -> usize {
    token.token().start_position().bytes()
}

fn end(node: &impl Node) -> usize {
    node.end_position().map_or(usize::MAX, |p| p.bytes())
}

impl Walk {
    fn bind(&mut self, name: &TokenReference, from: usize) {
        self.bindings.push((name.token().to_string(), from, start(name)));
    }
}

impl Visitor for Walk {
    fn visit_local_assignment(&mut self, node: &ast::LocalAssignment) {
        let from = end(node);
        for name in node.names() {
            self.bind(name, from);
        }
    }

    fn visit_local_function(&mut self, node: &ast::LocalFunction) {
        // Visible in its own body, for recursion.
        self.bind(node.name(), start(node.name()));
    }

    fn visit_function_body(&mut self, node: &ast::FunctionBody) {
        let (open, close) = (start(node.parameters_parentheses().tokens().0), start(node.end_token()));
        self.blocks.push((open, close));
        for parameter in node.parameters() {
            if let Parameter::Name(name) = parameter {
                self.bind(name, open);
            }
        }
    }

    fn visit_numeric_for(&mut self, node: &ast::NumericFor) {
        let body = (start(node.do_token()), start(node.end_token()));
        self.blocks.push(body);
        self.bind(node.index_variable(), body.0);
    }

    fn visit_generic_for(&mut self, node: &ast::GenericFor) {
        let body = (start(node.do_token()), start(node.end_token()));
        self.blocks.push(body);
        for name in node.names() {
            self.bind(name, body.0);
        }
    }

    fn visit_do(&mut self, node: &ast::Do) {
        self.blocks.push((start(node.do_token()), start(node.end_token())));
    }

    fn visit_while(&mut self, node: &ast::While) {
        self.blocks.push((start(node.do_token()), start(node.end_token())));
    }

    fn visit_repeat(&mut self, node: &ast::Repeat) {
        self.blocks.push((start(node.repeat_token()), end(node.until())));
    }

    fn visit_if(&mut self, node: &ast::If) {
        // Each branch runs from its `then` or `else` to the keyword that starts the next one.
        let mut opens = vec![start(node.then_token())];
        let mut closes = Vec::new();
        for branch in node.else_if().into_iter().flatten() {
            closes.push(start(branch.else_if_token()));
            opens.push(start(branch.then_token()));
        }
        if let Some(token) = node.else_token() {
            closes.push(start(token));
            opens.push(start(token));
        }
        closes.push(start(node.end_token()));
        self.blocks.extend(opens.into_iter().zip(closes));
    }
}
