//! Integration tests for `-error` / `-warning` surfacing as
//! `Event::Diagnostic`.

use std::sync::Arc;

use erl_pp::{
    Branch, Diagnostic, Event, Origin, PreprocessError, Preprocessor, Severity, Source, Status,
};
use erl_tokenize::{Position, TokenKind, scan_token};

fn build_source(name: &str, text: &str) -> Source {
    let mut tokens = Vec::new();
    let mut position = Position::new();
    while let Some(t) = scan_token(text, position).expect("test input scans without lex errors") {
        position = t.end();
        tokens.push(t);
    }
    Source::new(name, text.to_string(), tokens)
}

fn make(text: &str) -> Preprocessor {
    Preprocessor::new(build_source("m.erl", text))
}

fn step(pp: &mut Preprocessor) -> Event {
    pp.step().expect("no protocol error")
}

fn diagnostic_or_panic(event: Event) -> Diagnostic {
    match event {
        Event::Diagnostic(d) => d,
        other => panic!("expected Event::Diagnostic, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 1. -error("msg"). fires Event::Diagnostic with severity=Error and a
//    single string-literal argument (lexical filter).
#[test]
fn error_directive_fires_diagnostic_with_error_severity() {
    let mut pp = make(r#"-error("msg")."#);
    let diag = diagnostic_or_panic(step(&mut pp));
    assert_eq!(diag.severity, Severity::Error);
    let lex: Vec<_> = diag
        .arguments
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .collect();
    assert_eq!(lex.len(), 1);
    assert_eq!(lex[0].token().kind(), TokenKind::String);
}

// ---------------------------------------------------------------------
// 2. -warning(atom_msg). fires Event::Diagnostic with severity=Warning.
#[test]
fn warning_directive_fires_diagnostic_with_warning_severity() {
    let mut pp = make("-warning(atom_msg).");
    let diag = diagnostic_or_panic(step(&mut pp));
    assert_eq!(diag.severity, Severity::Warning);
    let lex: Vec<_> = diag
        .arguments
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .collect();
    assert_eq!(lex.len(), 1);
    assert_eq!(lex[0].token().kind(), TokenKind::Atom);
}

// ---------------------------------------------------------------------
// 3. Multi-token argument keeps every lexical token.
#[test]
fn multi_token_argument_is_preserved() {
    let mut pp = make("-error({tuple, msg}).");
    let diag = diagnostic_or_panic(step(&mut pp));
    let lex_texts: Vec<_> = diag
        .arguments
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .map(|t| t.text().to_owned())
        .collect();
    assert_eq!(lex_texts, vec!["{", "tuple", ",", "msg", "}"]);
}

// ---------------------------------------------------------------------
// 4. Hidden tokens (whitespace) inside the argument are preserved as
//    part of `arguments` (matches MacroExpansionRequest convention).
#[test]
fn argument_preserves_hidden_tokens() {
    let mut pp = make(r#"-error( "hi" )."#);
    let diag = diagnostic_or_panic(step(&mut pp));
    // At least one hidden token (the leading whitespace) should be
    // present in `arguments`.
    let hidden_count = diag
        .arguments
        .iter()
        .filter(|t| !t.token().kind().is_lexical())
        .count();
    assert!(
        hidden_count > 0,
        "expected hidden tokens in arguments, got {:?}",
        diag.arguments
            .iter()
            .map(|t| (t.text(), t.token().kind()))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------
// 5. Event::Diagnostic is not held pending — the next step returns
//    the following token.
#[test]
fn diagnostic_does_not_pend_the_state_machine() {
    let mut pp = make("-warning(hi).\nafter.");
    let _ = step(&mut pp); // Diagnostic
    assert!(matches!(pp.status(), Status::Scanning));
    let mut saw_after = false;
    loop {
        match step(&mut pp) {
            Event::Token(t) if t.text() == "after" => {
                saw_after = true;
                break;
            }
            Event::Token(_) => {}
            Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_after);
}

// ---------------------------------------------------------------------
// 6. `-error` / `-warning` fold into Diagnostic (same shape as
//    AwaitingInclude / AwaitingConditional).
#[test]
fn error_directive_does_not_emit_macro_events() {
    let mut pp = make("-error(oops).");
    let event = step(&mut pp);
    assert!(matches!(event, Event::Diagnostic(_)));
    loop {
        match step(&mut pp) {
            Event::Complete => break,
            Event::MacroDefined(d) => panic!("unexpected MacroDefined: {d:?}"),
            Event::MacroUndefined(u) => panic!("unexpected MacroUndefined: {u:?}"),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// 7. Argument macros are NOT expanded — `?FOO` surfaces as raw tokens.
#[test]
fn argument_macros_are_not_expanded() {
    let mut pp = make("-error(?FOO).");
    let diag = diagnostic_or_panic(step(&mut pp));
    let lex_texts: Vec<_> = diag
        .arguments
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .map(|t| t.text().to_owned())
        .collect();
    assert_eq!(lex_texts, vec!["?", "FOO"]);
    // And the state machine must not have transitioned into
    // AwaitingMacroExpansion while producing the diagnostic.
    assert!(matches!(pp.status(), Status::Scanning));
}

// ---------------------------------------------------------------------
// 8. `-error` inside an inactive branch is silenced (no
//    Event::Diagnostic).
#[test]
fn inactive_branch_suppresses_diagnostic() {
    let mut pp = make(
        "-ifdef(FOO).\n\
         -error(skipped).\n\
         -endif.\n",
    );
    let _ = step(&mut pp); // AwaitingConditional
    pp.resume_conditional(Branch::Else).expect("resume ok");
    loop {
        match step(&mut pp) {
            Event::Diagnostic(d) => panic!("unexpected diagnostic inside inactive branch: {d:?}"),
            Event::BranchBoundary(_) | Event::Token(_) => {}
            Event::Complete => return,
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 9. `-error` inside an active branch still fires normally.
#[test]
fn active_branch_still_fires_diagnostic() {
    let mut pp = make(
        "-ifdef(FOO).\n\
         -error(active).\n\
         -endif.\n",
    );
    let _ = step(&mut pp); // AwaitingConditional
    pp.resume_conditional(Branch::Then).expect("resume ok");
    let mut saw_diag = false;
    loop {
        match step(&mut pp) {
            Event::Diagnostic(d) => {
                assert_eq!(d.severity, Severity::Error);
                saw_diag = true;
            }
            Event::BranchBoundary(_) | Event::Token(_) => {}
            Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_diag);
}

// ---------------------------------------------------------------------
// 10. `parent_origin` and `directive_span` reflect the current source.
#[test]
fn diagnostic_parent_origin_matches_current_source() {
    let mut pp = make(r#"-error("x")."#);
    let diag = diagnostic_or_panic(step(&mut pp));
    // Top-level source: parent_origin is Origin::Source.
    assert!(matches!(*diag.parent_origin, Origin::Source));
    // directive_span sits on the top-level source; every argument
    // token also shares that SourceId.
    let src_id = diag.directive_span.source_id;
    for t in &diag.arguments {
        assert_eq!(t.source_span().source_id, src_id);
    }
}

// ---------------------------------------------------------------------
// 11. `-error` inside an include source: parent_origin carries the
//     Origin::Include chain.
#[test]
fn diagnostic_inside_include_carries_include_origin() {
    let mut pp = make(r#"-include("h.hrl")."#);
    let _ = step(&mut pp); // AwaitingInclude
    pp.resume_include(build_source("h.hrl", "-error(inner)."))
        .expect("resume ok");
    let diag = diagnostic_or_panic(step(&mut pp));
    match &*diag.parent_origin {
        Origin::Include { parent, .. } => {
            assert!(matches!(**parent, Origin::Source));
        }
        other => panic!("expected Origin::Include, got {other:?}"),
    }
    // Every argument token also gets the same include origin.
    for t in &diag.arguments {
        assert!(matches!(t.origin(), Origin::Include { .. }));
    }
    // Sanity: Arc-share between diag.parent_origin and the token's origin.
    if let Some(first_arg) = diag.arguments.first() {
        let arg_origin = first_arg.origin();
        // Both should be the same Origin::Include structurally (Arc
        // sharing is optional but structural equality is required).
        match (arg_origin, &*diag.parent_origin) {
            (
                Origin::Include {
                    kind: k1,
                    include_site: s1,
                    ..
                },
                Origin::Include {
                    kind: k2,
                    include_site: s2,
                    ..
                },
            ) => {
                assert_eq!(k1, k2);
                assert_eq!(s1, s2);
            }
            _ => panic!("both should be Origin::Include"),
        }
    }
    let _ = Arc::clone(&diag.parent_origin); // exercise Arc API
}

// ---------------------------------------------------------------------
// 12. Malformed `-error(` surfaces as `Event::PreprocessError`
//     with a parse failure, NOT as `Event::Diagnostic`.
#[test]
fn malformed_diagnostic_directive_is_parse_error() {
    let mut pp = make("-error(unterminated");
    loop {
        match step(&mut pp) {
            Event::PreprocessError(PreprocessError::ParseUnexpectedToken { .. })
            | Event::PreprocessError(PreprocessError::ParseUnexpectedEof { .. }) => return,
            Event::Diagnostic(d) => panic!("expected Parse error, got Diagnostic: {d:?}"),
            Event::Complete => panic!("expected Parse error, got Complete"),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------
// 13. `directive_span` covers `-` through `.` of the directive.
#[test]
fn directive_span_covers_the_whole_directive() {
    let text = "-error(msg).";
    let mut pp = make(text);
    let diag = diagnostic_or_panic(step(&mut pp));
    let start = diag.directive_span.start.offset();
    let end = diag.directive_span.end.offset();
    // The directive occupies the full source in this test.
    assert_eq!(start, 0);
    assert_eq!(end, text.len());
}
