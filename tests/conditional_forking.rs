//! Integration tests for `-ifdef` / `-ifndef` / `-else` / `-endif`
//! and the Sans-I/O fork / resume_conditional protocol.

use std::assert_matches;

fn build_source(name: &str, text: &str) -> erl_pp::Source {
    erl_pp::Source::new(
        name,
        text,
        erl_tokenize::scan_tokens(text).expect("test input scans without lex errors"),
    )
}

fn make(text: &str) -> erl_pp::Preprocessor {
    erl_pp::Preprocessor::new([build_source("m.erl", text)])
}

fn step(pp: &mut erl_pp::Preprocessor) -> erl_pp::Event {
    pp.step().expect("no protocol error")
}

fn boundary_tag(b: &erl_pp::BranchBoundary) -> &'static str {
    match b {
        erl_pp::BranchBoundary::Else { .. } => "else",
        erl_pp::BranchBoundary::Endif { .. } => "endif",
    }
}

fn lexical_texts(pp: &mut erl_pp::Preprocessor) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match step(pp) {
            erl_pp::Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                out.push(ppt.text().to_owned());
            }
            erl_pp::Event::Token(_)
            | erl_pp::Event::MacroDefined(_)
            | erl_pp::Event::MacroUndefined(_)
            | erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::Complete => return out,
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 1. -ifdef with the macro defined: payload is Ifdef,
//    defined=true, recommended=Then.
#[test]
fn ifdef_defined_recommends_then() {
    let mut pp = make("-define(FOO, 1).\n-ifdef(FOO).\nthen_side.\n-endif.\n");
    // consume -define
    assert_matches!(step(&mut pp), erl_pp::Event::MacroDefined(_));
    let conditional = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected AwaitingConditional, got {other:?}"),
    };
    let erl_pp::Conditional::Ifdef(d) = conditional else {
        panic!("expected Ifdef, got {conditional:?}");
    };
    assert_eq!(d.name.as_str(), "FOO");
    assert!(d.defined);
    assert_eq!(d.recommended, erl_pp::Branch::Then);
}

// ---------------------------------------------------------------------
// 2. -ifndef with the macro undefined: recommended=Then.
#[test]
fn ifndef_undefined_recommends_then() {
    let mut pp = make("-ifndef(NOPE).\nthen_side.\n-endif.\n");
    let conditional = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected AwaitingConditional, got {other:?}"),
    };
    let erl_pp::Conditional::Ifndef(d) = conditional else {
        panic!("expected Ifndef, got {conditional:?}");
    };
    assert!(!d.defined);
    assert_eq!(d.recommended, erl_pp::Branch::Then);
}

// ---------------------------------------------------------------------
// 3. `-ifdef` folds into AwaitingConditional (like
//    AwaitingMacroExpansion).
#[test]
fn ifdef_does_not_emit_event_directive() {
    let mut pp = make("-ifdef(FOO).\n-endif.\n");
    let first = step(&mut pp);
    assert_matches!(first, erl_pp::Event::AwaitingConditional(_));
}

// ---------------------------------------------------------------------
// 4. Choosing Then in an -ifdef/-endif with content on the Then side:
//    scan of Then tokens, then BranchBoundary(Endif) at end.
#[test]
fn resume_conditional_then_scans_then_side() {
    let mut pp = make("-ifdef(FOO).\nthen_side.\n-endif.\n");
    let _ = step(&mut pp); // AwaitingConditional
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ok");
    let mut saw_then = false;
    let mut saw_endif_boundary = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "then_side" => saw_then = true,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::BranchBoundary(b) => {
                assert_matches!(b, erl_pp::BranchBoundary::Endif { .. });
                saw_endif_boundary = true;
            }
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_then, "Then side content was not emitted");
    assert!(saw_endif_boundary, "endif boundary event was not emitted");
}

// ---------------------------------------------------------------------
// 5. Choosing Else on an ifdef with content on both sides: Then is
//    skipped, Else content is emitted, boundaries fire for else and
//    endif.
#[test]
fn resume_conditional_else_skips_then_and_scans_else_side() {
    let mut pp = make("-ifdef(FOO).\nthen_side.\n-else.\nelse_side.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    let mut saw_else = false;
    let mut saw_then = false;
    let mut boundaries = Vec::new();
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "then_side" => saw_then = true,
            erl_pp::Event::Token(t) if t.text() == "else_side" => saw_else = true,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::BranchBoundary(b) => boundaries.push(boundary_tag(&b)),
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_else);
    assert!(!saw_then, "Then side should have been skipped");
    assert_eq!(boundaries, ["else", "endif"]);
}

// ---------------------------------------------------------------------
// 6. Choosing Then still fires Else boundary when the conditional has
//    an else, then skips the Else side.
#[test]
fn resume_conditional_then_fires_else_boundary_and_skips_else_side() {
    let mut pp = make("-ifdef(FOO).\nthen_side.\n-else.\nelse_side.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ok");
    let mut saw_then = false;
    let mut saw_else = false;
    let mut boundaries = Vec::new();
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "then_side" => saw_then = true,
            erl_pp::Event::Token(t) if t.text() == "else_side" => saw_else = true,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::BranchBoundary(b) => boundaries.push(boundary_tag(&b)),
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_then);
    assert!(!saw_else);
    assert_eq!(boundaries, ["else", "endif"]);
}

// ---------------------------------------------------------------------
// 7. No -else: Choosing Else means the whole span from ifdef to endif
//    is empty content (skip Then, no else side to visit), single
//    Endif boundary.
#[test]
fn resume_conditional_else_without_else_is_empty_branch() {
    let mut pp = make("-ifdef(FOO).\nthen_side.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    let mut saw_then = false;
    let mut boundaries = Vec::new();
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "then_side" => saw_then = true,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::BranchBoundary(b) => boundaries.push(boundary_tag(&b)),
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(!saw_then);
    assert_eq!(boundaries, vec!["endif"]);
}

// ---------------------------------------------------------------------
// 8. Inactive branch suppresses -define side effects.
#[test]
fn inactive_branch_does_not_apply_define() {
    let mut pp = make("-ifdef(FOO).\n-define(BAR, 1).\n-endif.\n?BAR.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    // BAR was defined inside the inactive Then side, so ?BAR should
    // trigger AwaitingMacroExpansion (unknown macro).
    loop {
        match step(&mut pp) {
            erl_pp::Event::BranchBoundary(_) | erl_pp::Event::Token(_) => {}
            erl_pp::Event::AwaitingMacroExpansion(req) => {
                assert_eq!(req.name.as_str(), "BAR");
                return;
            }
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 9. Inactive branch suppresses -include.
#[test]
fn inactive_branch_does_not_fire_awaiting_include() {
    let mut pp = make(
        r#"-ifdef(FOO).
-include("skipped.hrl").
-endif.
"#,
    );
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    // Drain to completion — no AwaitingInclude should appear.
    loop {
        match step(&mut pp) {
            erl_pp::Event::BranchBoundary(_) | erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => return,
            erl_pp::Event::AwaitingInclude(_) => {
                panic!("AwaitingInclude fired inside an inactive branch");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 10. Inactive branch skips ?MACRO calls (no AwaitingMacroExpansion).
#[test]
fn inactive_branch_does_not_recognize_macro_calls() {
    let mut pp = make("-ifdef(FOO).\n?UNKNOWN.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    loop {
        match step(&mut pp) {
            erl_pp::Event::BranchBoundary(_) | erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => return,
            erl_pp::Event::AwaitingMacroExpansion(_) => {
                panic!("macro expansion fired inside an inactive branch");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 11. Nested conditional inside inactive branch: no
//     AwaitingConditional, no boundary events for the nested one.
//     Only the outer endif fires a boundary.
#[test]
fn nested_conditional_inside_inactive_is_silent() {
    let mut pp = make(
        "-ifdef(OUTER).\n\
         -ifdef(INNER).\n\
         inner_then.\n\
         -else.\n\
         inner_else.\n\
         -endif.\n\
         -endif.\n",
    );
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("resume ok");
    let mut awaits = 0;
    let mut boundaries = Vec::new();
    loop {
        match step(&mut pp) {
            erl_pp::Event::AwaitingConditional(_) => awaits += 1,
            erl_pp::Event::BranchBoundary(b) => boundaries.push(boundary_tag(&b)),
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(awaits, 0, "no nested AwaitingConditional should have fired");
    assert_eq!(
        boundaries,
        vec!["endif"],
        "only the outer endif boundary should have fired"
    );
}

// ---------------------------------------------------------------------
// 12. Clone at the awaiting event and drive Then / Else independently
//     with independent macro state.
#[test]
fn clone_then_and_else_share_source_but_diverge_state() {
    let mut base =
        make("-ifdef(FOO).\n-define(FROM_THEN, 1).\n-else.\n-define(FROM_ELSE, 2).\n-endif.\n");
    let _ = step(&mut base); // AwaitingConditional
    let mut then_pp = base.clone();
    let mut else_pp = base;
    then_pp
        .resume_conditional(erl_pp::Branch::Then)
        .expect("then");
    else_pp
        .resume_conditional(erl_pp::Branch::Else)
        .expect("else");
    let _ = lexical_texts(&mut then_pp);
    let _ = lexical_texts(&mut else_pp);
    // then_pp saw -define(FROM_THEN, 1). only.
    assert!(then_pp.macros().is_defined("FROM_THEN"));
    assert!(!then_pp.macros().is_defined("FROM_ELSE"));
    assert!(!else_pp.macros().is_defined("FROM_THEN"));
    assert!(else_pp.macros().is_defined("FROM_ELSE"));
}

// ---------------------------------------------------------------------
// 13. Stray -endif at top level is a erl_pp::PreprocessError::StrayEndif.
#[test]
fn stray_endif_is_conditional_error() {
    let mut pp = make("-endif.\n");
    let event = step(&mut pp);
    match event {
        erl_pp::Event::PreprocessError(err @ erl_pp::PreprocessError::StrayEndif { span }) => {
            assert_eq!(err.span(), span);
        }
        other => panic!("expected StrayEndif error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 14. Stray -else at top level is a erl_pp::PreprocessError::StrayElse.
#[test]
fn stray_else_is_conditional_error() {
    let mut pp = make("-else.\n");
    let event = step(&mut pp);
    match event {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::StrayElse { .. }) => {}
        other => panic!("expected StrayElse error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------
// 15. Double -else in the same conditional is a
//     erl_pp::PreprocessError::DoubleElse.
#[test]
fn double_else_is_conditional_error() {
    let mut pp = make("-ifdef(FOO).\n-else.\n-else.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ok");
    // Drain until we hit the second -else.
    loop {
        match step(&mut pp) {
            erl_pp::Event::BranchBoundary(_) | erl_pp::Event::Token(_) => {}
            erl_pp::Event::PreprocessError(erl_pp::PreprocessError::DoubleElse { .. }) => {
                return;
            }
            other => panic!("expected DoubleElse error, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 16. Unclosed conditional at EOF fires
//     erl_pp::PreprocessError::UnclosedConditional pointing at
//     the opening directive.
#[test]
fn unclosed_conditional_at_eof_is_error() {
    let mut pp = make("-ifdef(FOO).\nsomething.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ok");
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::PreprocessError(erl_pp::PreprocessError::UnclosedConditional {
                ..
            }) => {
                return;
            }
            other => panic!("expected UnclosedConditional error, got {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 17. Protocol error: resume_conditional while scanning.
#[test]
fn resume_conditional_in_scanning_is_protocol_error() {
    let mut pp = make("foo.");
    let err = pp
        .resume_conditional(erl_pp::Branch::Then)
        .expect_err("should fail");
    assert_eq!(err, erl_pp::ProtocolError);
    pp.step()
        .expect("wrong resume_conditional must not leave Scanning");
}

// ---------------------------------------------------------------------
// 18. Protocol error: resume_conditional while awaiting an include.
#[test]
fn resume_conditional_while_awaiting_include_is_protocol_error() {
    let mut pp = make(r#"-include("h.hrl")."#);
    let event = step(&mut pp);
    assert_matches!(event, erl_pp::Event::AwaitingInclude(_));
    let err = pp
        .resume_conditional(erl_pp::Branch::Then)
        .expect_err("should fail");
    assert_eq!(err, erl_pp::ProtocolError);
    assert_eq!(
        pp.step().expect_err("still awaiting include"),
        erl_pp::ProtocolError
    );
}

// ---------------------------------------------------------------------
// 19. Sanity: an active-branch parse error IS surfaced normally.
//     (Ensures the inactive-suppression logic did not accidentally
//     silence errors that should always fire.)
#[test]
fn active_branch_parse_error_still_fires() {
    // Malformed directive parses fail in the active branch.
    let mut pp = make("-include.\n");
    let event = step(&mut pp);
    assert_matches!(event, erl_pp::Event::PreprocessError(_));
}

// ---------------------------------------------------------------------
// 20. Include-in-active-branch still works normally (guards the
//     "not is_in_inactive_branch" path).
#[test]
fn include_in_active_branch_still_fires_awaiting_include() {
    let mut pp = make(
        r#"-ifdef(FOO).
-include("h.hrl").
-endif.
"#,
    );
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ok");
    loop {
        match step(&mut pp) {
            erl_pp::Event::AwaitingInclude(req) => {
                assert_eq!(req.kind, erl_pp::IncludeKind::Include);
                assert_eq!(req.path.as_str(), "h.hrl");
                return;
            }
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// -if / -elif
// ---------------------------------------------------------------------

fn lexical_from_condition(tokens: &[erl_pp::SourceToken]) -> Vec<String> {
    tokens
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .map(|t| t.text().to_owned())
        .collect()
}

#[test]
fn if_then_scans_body() {
    let mut pp = make("-if(true).\nthen_side.\n-endif.\n");
    let req = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected AwaitingConditional, got {other:?}"),
    };
    let erl_pp::Conditional::If(expr) = req else {
        panic!("expected If, got {req:?}");
    };
    assert_eq!(lexical_from_condition(&expr.condition_tokens), ["true"]);
    pp.resume_conditional(erl_pp::Branch::Then).expect("resume");
    let texts = lexical_texts(&mut pp);
    assert_eq!(texts, ["then_side", "."]);
}

#[test]
fn if_else_skips_body() {
    let mut pp = make("-if(false).\nthen_side.\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else).expect("resume");
    let texts = lexical_texts(&mut pp);
    assert!(texts.is_empty(), "Then body must be skipped: {texts:?}");
}

#[test]
fn if_elif_chain_first_active_skips_later_elif() {
    let mut pp = make(
        "-if(true).\n\
         a.\n\
         -elif(true).\n\
         b.\n\
         -else.\n\
         c.\n\
         -endif.\n",
    );
    let req = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected opening -if, got {other:?}"),
    };
    assert_matches!(req, erl_pp::Conditional::If(_));
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume if");

    let mut saw_a = false;
    let mut saw_b = false;
    let mut saw_c = false;
    let mut saw_elif_await = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "a" => saw_a = true,
            erl_pp::Event::Token(t) if t.text() == "b" => saw_b = true,
            erl_pp::Event::Token(t) if t.text() == "c" => saw_c = true,
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::AwaitingConditional(erl_pp::Conditional::Elif(_)) => {
                saw_elif_await = true;
            }
            erl_pp::Event::Complete => break,
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(saw_a);
    assert!(!saw_b, "later elif body must not emit");
    assert!(!saw_c, "else body must not emit after taken if");
    assert!(!saw_elif_await, "taken chain must not await elif");
}

#[test]
fn if_elif_chain_first_inactive_awaits_elif() {
    let mut pp = make(
        "-define(V, 1).\n\
         -if(false).\n\
         a.\n\
         -elif(?V).\n\
         b.\n\
         -endif.\n",
    );
    assert_matches!(step(&mut pp), erl_pp::Event::MacroDefined(_));
    let req = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected -if, got {other:?}"),
    };
    assert_matches!(req, erl_pp::Conditional::If(_));
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("skip if");

    let mut saw_a = false;
    let elif_req = loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "a" => saw_a = true,
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::AwaitingConditional(erl_pp::Conditional::Elif(r)) => break r,
            erl_pp::Event::Complete => panic!("never saw elif await"),
            other => panic!("unexpected: {other:?}"),
        }
    };
    assert!(!saw_a);
    assert_eq!(lexical_from_condition(&elif_req.condition_tokens), ["1"]);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("take elif");
    let texts = lexical_texts(&mut pp);
    assert_eq!(texts, ["b", "."]);
}

#[test]
fn if_elif_else_each_branch() {
    // Else path of the chain.
    let mut pp = make(
        "-if(false).\n\
         a.\n\
         -elif(false).\n\
         b.\n\
         -else.\n\
         c.\n\
         -endif.\n",
    );
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("skip if");
    loop {
        match step(&mut pp) {
            erl_pp::Event::AwaitingConditional(erl_pp::Conditional::Elif(_)) => break,
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            other => panic!("unexpected before elif: {other:?}"),
        }
    }
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("skip elif");
    let texts = lexical_texts(&mut pp);
    assert_eq!(texts, ["c", "."]);
}

#[test]
fn stray_elif_errors() {
    let mut pp = make("-elif(true).\n");
    match step(&mut pp) {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::StrayElif { .. }) => {}
        other => panic!("expected StrayElif, got {other:?}"),
    }
}

#[test]
fn elif_on_ifdef_is_stray() {
    let mut pp = make("-ifdef(FOO).\n-elif(true).\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then)
        .expect("resume ifdef");
    match step(&mut pp) {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::StrayElif { .. }) => {}
        other => panic!("expected StrayElif on ifdef, got {other:?}"),
    }
}

#[test]
fn elif_after_else_errors() {
    let mut pp = make("-if(false).\n-else.\n-elif(true).\n-endif.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("skip if");
    // Cross -else boundary.
    loop {
        match step(&mut pp) {
            erl_pp::Event::BranchBoundary(erl_pp::BranchBoundary::Else { .. }) => break,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::PreprocessError(erl_pp::PreprocessError::ElifAfterElse { .. }) => return,
            other => panic!("unexpected before else/elif: {other:?}"),
        }
    }
    match step(&mut pp) {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::ElifAfterElse { .. }) => {}
        other => panic!("expected ElifAfterElse, got {other:?}"),
    }
}

#[test]
fn nested_if_in_inactive_outer_is_silent() {
    let mut pp = make(
        "-ifdef(FOO).\n\
         -if(true).\n\
         inner.\n\
         -elif(true).\n\
         x.\n\
         -endif.\n\
         -endif.\n",
    );
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Else)
        .expect("skip outer");
    let texts = lexical_texts(&mut pp);
    assert!(
        texts.is_empty(),
        "nested if chain in inactive outer must stay silent: {texts:?}"
    );
}

#[test]
fn if_fork_independent_branches() {
    let mut pp = make("-if(true).\nthen_side.\n-else.\nelse_side.\n-endif.\n");
    let _ = step(&mut pp);
    let mut then_pp = pp.clone();
    let mut else_pp = pp;
    then_pp
        .resume_conditional(erl_pp::Branch::Then)
        .expect("then");
    else_pp
        .resume_conditional(erl_pp::Branch::Else)
        .expect("else");
    assert_eq!(lexical_texts(&mut then_pp), ["then_side", "."]);
    assert_eq!(lexical_texts(&mut else_pp), ["else_side", "."]);
}

#[test]
fn if_condition_expands_defined_macro() {
    let mut pp = make("-define(V, 27).\n-if(?V >= 27).\nok.\n-endif.\n");
    assert_matches!(step(&mut pp), erl_pp::Event::MacroDefined(_));
    let req = match step(&mut pp) {
        erl_pp::Event::AwaitingConditional(r) => r,
        other => panic!("expected -if, got {other:?}"),
    };
    let erl_pp::Conditional::If(expr) = req else {
        panic!("expected If, got {req:?}");
    };
    assert_eq!(
        lexical_from_condition(&expr.condition_tokens),
        ["27", ">=", "27"]
    );
    pp.resume_conditional(erl_pp::Branch::Then).expect("resume");
    assert_eq!(lexical_texts(&mut pp), ["ok", "."]);
}

#[test]
fn if_condition_caller_driven_macro_empty_response() {
    let mut pp = make("-if(?UNKNOWN >= 27).\nok.\n-endif.\n");
    // First event may be AwaitingMacroExpansion for ?UNKNOWN inside
    // the condition, then AwaitingConditional with empty/partial tokens.
    let mut saw_macro = false;
    let req = loop {
        match step(&mut pp) {
            erl_pp::Event::AwaitingMacroExpansion(r) => {
                assert_eq!(r.name.as_str(), "UNKNOWN");
                saw_macro = true;
                let empty = build_source("<caller-driven>", "");
                pp.resume_macro_expansion(empty).expect("empty expand");
            }
            erl_pp::Event::AwaitingConditional(r) => break r,
            other => panic!("unexpected before conditional: {other:?}"),
        }
    };
    assert!(saw_macro);
    let erl_pp::Conditional::If(expr) = req else {
        panic!("expected If, got {req:?}");
    };
    assert_eq!(lexical_from_condition(&expr.condition_tokens), [">=", "27"]);
}

#[test]
fn if_unclosed_at_eof() {
    let mut pp = make("-if(true).\nbody.\n");
    let _ = step(&mut pp);
    pp.resume_conditional(erl_pp::Branch::Then).expect("resume");
    let mut saw_unclosed = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(_) | erl_pp::Event::BranchBoundary(_) => {}
            erl_pp::Event::PreprocessError(erl_pp::PreprocessError::UnclosedConditional {
                ..
            }) => {
                saw_unclosed = true;
            }
            erl_pp::Event::Complete => break,
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(saw_unclosed);
}
