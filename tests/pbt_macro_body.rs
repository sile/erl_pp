//! Property-based tests for `-define` body collection.
//!
//! The parser used to terminate the `-define` body at the first
//! `)` it saw, regardless of nesting. The fix collects tokens until
//! the outer `)` is immediately followed by `.` (2-token lookahead),
//! with no delimiter tracking. These properties fuzz macro bodies
//! that would trip the old behavior and assert the new parser
//! accepts them without a preprocess error.
//!
//! Each property renders a randomly generated body as source text,
//! wraps it as `-define(FOO, <body>).\n`, feeds it into
//! `erl_pp::Preprocessor::step`, and inspects the emitted event.

use std::cell::Cell;
// ---------------------------------------------------------------------------
// AST + renderer

#[derive(Debug, Clone)]
enum Node {
    /// A lexical atom-like leaf that never contains `)` or `.`.
    Leaf(&'static str),
    /// `( children )`.
    Parens(Vec<Node>),
    /// `[ children ]`.
    Brackets(Vec<Node>),
    /// `{ children }`.
    Braces(Vec<Node>),
    /// `<< children >>`.
    Binary(Vec<Node>),
    /// `begin children end`.
    BeginEnd(Vec<Node>),
    /// `if children end`.
    IfEnd(Vec<Node>),
    /// A bare `)` embedded inside a container. Used by the fuzz to
    /// place a `)` deep inside a delimiter that the old parser would
    /// have misinterpreted as the outer directive close.
    CloseParen,
    /// A comma separator.
    Comma,
}

const LEAVES: &[&str] = &["a", "b", "foo", "X", "Y", "42", "ok"];

impl Node {
    fn render(&self, out: &mut String) {
        match self {
            Node::Leaf(s) => out.push_str(s),
            Node::Parens(children) => wrap_children(out, "(", ")", children),
            Node::Brackets(children) => wrap_children(out, "[", "]", children),
            Node::Braces(children) => wrap_children(out, "{", "}", children),
            Node::Binary(children) => wrap_children(out, "<<", ">>", children),
            Node::BeginEnd(children) => wrap_children(out, "begin ", " end", children),
            Node::IfEnd(children) => wrap_children(out, "if ", " end", children),
            Node::CloseParen => out.push(')'),
            Node::Comma => out.push_str(", "),
        }
    }

    /// Returns `true` if this subtree contains a bare `)` inside a
    /// delimiter that is not the outer directive.
    fn has_injected_close_paren(&self) -> bool {
        match self {
            Node::CloseParen => true,
            Node::Leaf(_) | Node::Comma => false,
            Node::Parens(cs)
            | Node::Brackets(cs)
            | Node::Braces(cs)
            | Node::Binary(cs)
            | Node::BeginEnd(cs)
            | Node::IfEnd(cs) => cs.iter().any(Node::has_injected_close_paren),
        }
    }
}

fn wrap_children(out: &mut String, open: &str, close: &str, children: &[Node]) {
    out.push_str(open);
    for (i, child) in children.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        child.render(out);
    }
    out.push_str(close);
}

// ---------------------------------------------------------------------------
// Generators

const MAX_DEPTH: usize = 4;
const MAX_CHILDREN: usize = 4;

fn sample_leaf(ctx: &mut noprop::TestCaseContext) -> Node {
    let idx = noprop::sample_weighted_index(ctx, &[1; LEAVES.len()]);
    Node::Leaf(LEAVES[idx])
}

/// Draws a container node (never `Leaf`, `Comma`, or `CloseParen`).
fn sample_container(ctx: &mut noprop::TestCaseContext, depth_budget: usize) -> Node {
    // 6 container kinds with equal weight.
    let children = sample_children(ctx, depth_budget);
    match noprop::sample_weighted_index(ctx, &[1, 1, 1, 1, 1, 1]) {
        0 => Node::Parens(children),
        1 => Node::Brackets(children),
        2 => Node::Braces(children),
        3 => Node::Binary(children),
        4 => Node::BeginEnd(children),
        _ => Node::IfEnd(children),
    }
}

fn sample_node(ctx: &mut noprop::TestCaseContext, depth_budget: usize) -> Node {
    if depth_budget == 0 {
        return sample_leaf(ctx);
    }
    // Bias toward containers so we exercise delimiter handling, but
    // keep a leaf branch to terminate recursion cases early.
    match noprop::sample_weighted_index(ctx, &[3, 1]) {
        0 => sample_container(ctx, depth_budget - 1),
        _ => sample_leaf(ctx),
    }
}

fn sample_children(ctx: &mut noprop::TestCaseContext, depth_budget: usize) -> Vec<Node> {
    let count = noprop::sample_with_boundaries(
        ctx,
        &[0usize, 1, MAX_CHILDREN],
        noprop::Ratio::one_nth(5),
        |ctx| {
            // sample_usize_in for the interior distribution.
            noprop::sample_usize_in(ctx, 0..=MAX_CHILDREN)
        },
    );
    let mut children = Vec::new();
    for i in 0..count {
        if i > 0 {
            children.push(Node::Comma);
        }
        children.push(sample_node(ctx, depth_budget));
    }
    children
}

/// Injects a bare `)` at a random leaf position inside `node` (only
/// modifies container children; the top-level node is never replaced
/// so the outer shape stays a container).
fn inject_close_paren(ctx: &mut noprop::TestCaseContext, node: &mut Node) {
    let children = match node {
        Node::Parens(c)
        | Node::Brackets(c)
        | Node::Braces(c)
        | Node::Binary(c)
        | Node::BeginEnd(c)
        | Node::IfEnd(c) => c,
        Node::Leaf(_) | Node::Comma | Node::CloseParen => return,
    };
    if children.is_empty() {
        children.push(Node::CloseParen);
        return;
    }
    let idx = noprop::sample_usize_in(ctx, 0..=children.len());
    if idx == children.len() {
        children.push(Node::CloseParen);
    } else {
        // Recurse into an existing child with some probability so we
        // reach a nested container's interior; otherwise replace this
        // slot with `)` directly.
        if matches!(noprop::sample_weighted_index(ctx, &[1, 1]), 0) {
            inject_close_paren(ctx, &mut children[idx]);
        } else {
            children.insert(idx, Node::CloseParen);
        }
    }
}

// ---------------------------------------------------------------------------
// Driver

fn parse_define_body(body: &str) -> Result<usize, DriveError> {
    let text = format!("-define(FOO, {body}).\n");
    let tokens = scan_all(&text).map_err(DriveError::Lexical)?;
    let source = erl_pp::Source::new("prop.erl", text, tokens);
    let mut pp = erl_pp::Preprocessor::new([source]);
    match pp.step().map_err(DriveError::Protocol)? {
        erl_pp::Event::MacroDefined(def) => Ok(def.replacement.len()),
        erl_pp::Event::PreprocessError(err) => Err(DriveError::Preprocess(Box::new(err))),
        other => Err(DriveError::UnexpectedEvent(format!("{other:?}"))),
    }
}

fn scan_all(text: &str) -> Result<Vec<erl_tokenize::Token>, erl_tokenize::Error> {
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(token) = erl_tokenize::scan_token(text, position)? {
        position = token.end();
        tokens.push(token);
    }
    Ok(tokens)
}

#[derive(Debug)]
#[expect(dead_code, reason = "fields surface through Debug in failure messages")]
enum DriveError {
    Preprocess(Box<erl_pp::PreprocessError>),
    Lexical(erl_tokenize::Error),
    Protocol(erl_pp::ProtocolError),
    UnexpectedEvent(String),
}

// ---------------------------------------------------------------------------
// Properties

#[test]
fn round_trip_balanced_body_parses_successfully() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("ERL_PP_SEED")?;
    let mut runner = noprop::Runner::new(seed);
    let saw_container = Cell::new(0usize);
    let saw_leaf_only = Cell::new(0usize);

    runner.run(256, |ctx| {
        let depth = noprop::sample_usize_in(ctx, 0..=MAX_DEPTH);
        let node = if depth == 0 {
            sample_leaf(ctx)
        } else {
            sample_container(ctx, depth)
        };
        let mut body = String::new();
        node.render(&mut body);

        let n_tokens = match parse_define_body(&body) {
            Ok(n) => n,
            Err(e) => panic!("valid body `{body}` failed to parse: {e:?}"),
        };

        assert!(n_tokens > 0, "empty replacement for body `{body}`");
        if depth == 0 {
            saw_leaf_only.set(saw_leaf_only.get() + 1);
        } else {
            saw_container.set(saw_container.get() + 1);
        }
        Ok(())
    })?;

    assert!(
        saw_container.get() > 0,
        "no case exercised a container body\n{runner}"
    );
    assert!(
        saw_leaf_only.get() > 0,
        "no case exercised a leaf-only body\n{runner}"
    );
    Ok(())
}

#[test]
fn injected_close_paren_still_parses() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("ERL_PP_SEED")?;
    let mut runner = noprop::Runner::new(seed);
    let saw_injected = Cell::new(0usize);

    runner.run(256, |ctx| {
        // Always start from a container so `)` can be injected at a
        // meaningful position.
        let mut node = sample_container(ctx, MAX_DEPTH);
        inject_close_paren(ctx, &mut node);
        let mut body = String::new();
        node.render(&mut body);

        if !node.has_injected_close_paren() {
            // Injection may bottom out at a Leaf sub-node; skip when
            // it did not actually place a `)`.
            return Ok(());
        }

        if let Err(e) = parse_define_body(&body) {
            panic!("body with injected `)` failed to parse `{body}`: {e:?}");
        }
        saw_injected.set(saw_injected.get() + 1);
        Ok(())
    })?;

    assert!(
        saw_injected.get() >= 64,
        "expected at least 64 cases with an injected `)`, saw {}\n{runner}",
        saw_injected.get()
    );
    Ok(())
}

#[test]
fn adversarial_tail_only_outer_close_paren_dot_terminates() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("ERL_PP_SEED")?;
    let mut runner = noprop::Runner::new(seed);
    let saw_tail = Cell::new(0usize);

    runner.run(256, |ctx| {
        // Body = a simple leaf, followed by random tail padding of
        // `)`, whitespace, and `%comment\n` fragments. The outer `).`
        // is appended after the body, so the terminator is always the
        // final two characters of the wrapped source text.
        let mut body = String::from("foo");
        let tail_len = noprop::sample_usize_in(ctx, 0..=6);
        for _ in 0..tail_len {
            match noprop::sample_weighted_index(ctx, &[3, 2, 1, 1]) {
                0 => body.push(')'),
                1 => body.push(' '),
                2 => {
                    // Trailing comment ends at newline, so keep it
                    // followed by whitespace to stay inside the body.
                    body.push_str("% ");
                    body.push(noprop::sample_ascii_printable_char(ctx));
                    body.push('\n');
                }
                _ => body.push('\t'),
            }
        }

        if let Err(e) = parse_define_body(&body) {
            panic!("adversarial tail body `{body}` failed to parse: {e:?}");
        }
        saw_tail.set(saw_tail.get() + 1);
        Ok(())
    })?;

    assert!(
        saw_tail.get() > 0,
        "no case exercised an adversarial tail\n{runner}"
    );
    Ok(())
}
