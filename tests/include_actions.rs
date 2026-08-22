//! Integration tests for the `-include` / `-include_lib`
//! Sans-I/O event/response protocol.
//!
//! Covers the polished contract: kind + path payload, no source
//! read before the response, parent-source resume after include EOF,
//! nested include order, `erl_pp::Origin::Include` chain, empty-`erl_pp::Source`
//! skip, and protocol-error paths.

use core::assert_matches;
use std::sync::Arc;

fn build_source(name: &str, text: &str) -> erl_pp::Source {
    erl_pp::Source::from_text(name, text).expect("test input scans without lex errors")
}

fn make(text: &str) -> erl_pp::Preprocessor {
    erl_pp::Preprocessor::new([build_source("m.erl", text)])
}

fn step(pp: &mut erl_pp::Preprocessor) -> erl_pp::Event {
    pp.step().expect("no protocol error")
}

// ---------------------------------------------------------------------
// 1. `-include("foo.hrl").` fires AwaitingInclude with the right kind
//    and path. The include is not also reported as MacroDefined /
//    MacroUndefined.
#[test]
fn include_fires_awaiting_include_with_kind_and_path() {
    let mut pp = make(r#"-include("foo.hrl")."#);
    let event = step(&mut pp);
    let erl_pp::Event::AwaitingInclude(include) = event else {
        panic!("expected AwaitingInclude, got {event:?}");
    };
    assert_eq!(include.kind, erl_pp::IncludeKind::Include);
    assert_eq!(include.path.as_str(), "foo.hrl");
}

// ---------------------------------------------------------------------
// 2. `-include_lib("kernel/include/file.hrl").` fires AwaitingInclude
//    with kind = IncludeLib.
#[test]
fn include_lib_fires_awaiting_include_with_lib_kind() {
    let mut pp = make(r#"-include_lib("kernel/include/file.hrl")."#);
    let event = step(&mut pp);
    let erl_pp::Event::AwaitingInclude(include) = event else {
        panic!("expected AwaitingInclude, got {event:?}");
    };
    assert_eq!(include.kind, erl_pp::IncludeKind::IncludeLib);
    assert_eq!(include.path.as_str(), "kernel/include/file.hrl");
}

// ---------------------------------------------------------------------
// 3. `directive_span.source_id` differs from the include source's
//    SourceId — the parent id is stable and does not follow the
//    include's newly-issued id.
#[test]
fn directive_span_points_at_parent_not_include() {
    let mut pp = make(
        r#"-include("x.hrl").
"#,
    );
    let erl_pp::Event::AwaitingInclude(include) = step(&mut pp) else {
        panic!("expected AwaitingInclude");
    };
    let parent_id = include.directive_span.source_id;
    pp.resume_include(build_source("x.hrl", "inside."))
        .expect("resume ok");
    let erl_pp::Event::Token(t) = step(&mut pp) else {
        panic!("expected token from include source");
    };
    assert_ne!(t.source_span().source_id, parent_id);
}

// ---------------------------------------------------------------------
// 4. resume_include(source) splices the include tokens before
//    the parent resumes.
#[test]
fn resume_include_splices_include_before_parent_resumes() {
    let mut pp = make(
        r#"-include("h.hrl").
after."#,
    );
    let event = step(&mut pp);
    assert_matches!(event, erl_pp::Event::AwaitingInclude(_));
    pp.resume_include(build_source("h.hrl", "inside."))
        .expect("resume ok");
    // Include source: `inside.`
    let e = step(&mut pp);
    let erl_pp::Event::Token(t) = e else {
        panic!("expected erl_tokenize::Token, got {e:?}");
    };
    assert_eq!(t.text(), "inside");
    let e = step(&mut pp);
    let erl_pp::Event::Token(t) = e else {
        panic!("expected erl_tokenize::Token, got {e:?}");
    };
    assert_eq!(t.text(), ".");
    // Parent source resumes at the next lexical token `after`.
    let mut found_after = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "after" => {
                found_after = true;
                break;
            }
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(found_after, "parent source did not resume with `after`");
}

// ---------------------------------------------------------------------
// 5. An empty include erl_pp::Source skips content and jumps straight to the
//    parent's next token (same idiom as `resume_macro_expansion` with
//    a token-free erl_pp::Source).
#[test]
fn resume_include_with_empty_source_skips_content() {
    let mut pp = make(
        r#"-include("h.hrl").
after."#,
    );
    let event = step(&mut pp);
    assert_matches!(event, erl_pp::Event::AwaitingInclude(_));
    pp.resume_include(build_source("h.hrl", ""))
        .expect("resume ok");
    // Parent source next lexical token should be `after`.
    let mut found_after = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "after" => {
                found_after = true;
                break;
            }
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(found_after);
}

// ---------------------------------------------------------------------
// 6. Include source's tokens carry `erl_pp::Origin::Include { include_site, kind }`
//    chained under the parent origin.
#[test]
fn include_source_tokens_carry_origin_include_chain() {
    let mut pp = make(
        r#"-include("h.hrl").
"#,
    );
    let erl_pp::Event::AwaitingInclude(include) = step(&mut pp) else {
        panic!("expected AwaitingInclude");
    };
    let directive_span = include.directive_span;
    let expected_kind = include.kind;
    pp.resume_include(build_source("h.hrl", "inside."))
        .expect("resume ok");
    let erl_pp::Event::Token(ppt) = step(&mut pp) else {
        panic!("expected erl_tokenize::Token from include");
    };
    let erl_pp::Origin::Include {
        parent,
        include_site,
        kind,
    } = ppt.origin()
    else {
        panic!("expected erl_pp::Origin::Include, got {:?}", ppt.origin());
    };
    assert_eq!(*include_site, directive_span);
    assert_eq!(*kind, expected_kind);
    assert_matches!(**parent, erl_pp::Origin::Source);
}

// ---------------------------------------------------------------------
// 7. A macro defined inside the include source is visible after the
//    parent source resumes.
#[test]
fn macro_defined_in_include_visible_in_parent() {
    let mut pp = make(
        r#"-include("h.hrl").
?FOO."#,
    );
    let _ = step(&mut pp); // AwaitingInclude
    let include_src = build_source("h.hrl", "-define(FOO, 42).\n");
    pp.resume_include(include_src).expect("resume ok");
    // Drain include source; then parent's ?FOO should expand to 42.
    let mut saw_42 = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "42" => {
                saw_42 = true;
                break;
            }
            erl_pp::Event::Token(_)
            | erl_pp::Event::MacroDefined(_)
            | erl_pp::Event::MacroUndefined(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(saw_42, "?FOO defined in include did not expand in parent");
}

// ---------------------------------------------------------------------
// 8. Nested include: parent → child → grandchild → child → parent.
#[test]
fn nested_include_and_return_order() {
    let mut pp = make(
        r#"-include("a.hrl").
parent_after."#,
    );
    let erl_pp::Event::AwaitingInclude(req1) = step(&mut pp) else {
        panic!("expected AwaitingInclude for a.hrl");
    };
    assert_eq!(req1.path.as_str(), "a.hrl");
    pp.resume_include(build_source(
        "a.hrl",
        r#"-include("b.hrl").
a_after."#,
    ))
    .expect("resume a");
    let erl_pp::Event::AwaitingInclude(req2) = step(&mut pp) else {
        panic!("expected AwaitingInclude for b.hrl");
    };
    assert_eq!(req2.path.as_str(), "b.hrl");
    pp.resume_include(build_source("b.hrl", "b_inside."))
        .expect("resume b");
    // Now drain: expect b_inside, then a_after, then parent_after.
    let mut order = Vec::new();
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.token().kind().is_lexical() && t.text() != "." => {
                order.push(t.text().to_owned());
            }
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(order, vec!["b_inside", "a_after", "parent_after"]);
}

// ---------------------------------------------------------------------
// 9. Protocol error: resume_include while scanning.
#[test]
fn resume_include_in_scanning_is_protocol_error() {
    let mut pp = make("just_a_token.");
    let err = pp
        .resume_include(build_source("x.hrl", "x."))
        .expect_err("resume_include while scanning should fail");
    assert_eq!(err, erl_pp::ProtocolError);
    pp.step()
        .expect("wrong resume_include must not leave Scanning");
}

// ---------------------------------------------------------------------
// 10. Protocol error: resume_include while awaiting a macro expansion.
#[test]
fn resume_include_while_awaiting_macro_is_protocol_error() {
    let mut pp = make("?UNKNOWN.");
    let event = step(&mut pp);
    assert_matches!(event, erl_pp::Event::AwaitingMacroExpansion(_));
    let err = pp
        .resume_include(build_source("x.hrl", "x."))
        .expect_err("resume_include while awaiting macro should fail");
    assert_eq!(err, erl_pp::ProtocolError);
    assert_eq!(
        pp.step().expect_err("still awaiting macro expansion"),
        erl_pp::ProtocolError
    );
}

// ---------------------------------------------------------------------
// 11. Same include source pulled from two sites keeps distinct
//     `include_site` on each token's `erl_pp::Origin::Include`.
#[test]
fn same_source_from_two_sites_gets_distinct_include_site() {
    let mut pp = make(
        r#"-include("h.hrl").
-include("h.hrl")."#,
    );
    // First include.
    let erl_pp::Event::AwaitingInclude(req1) = step(&mut pp) else {
        panic!("expected first AwaitingInclude");
    };
    let site1 = req1.directive_span;
    pp.resume_include(build_source("h.hrl", "one."))
        .expect("resume 1");
    let erl_pp::Event::Token(t1) = step(&mut pp) else {
        panic!("expected token from first include");
    };
    let erl_pp::Origin::Include {
        include_site: s1, ..
    } = t1.origin()
    else {
        panic!("expected erl_pp::Origin::Include");
    };
    assert_eq!(*s1, site1);
    // Drain first include to EOF, arrive at second directive.
    loop {
        match step(&mut pp) {
            erl_pp::Event::AwaitingInclude(req2) => {
                let site2 = req2.directive_span;
                assert_ne!(site2, site1);
                pp.resume_include(build_source("h.hrl", "two."))
                    .expect("resume 2");
                let erl_pp::Event::Token(t2) = step(&mut pp) else {
                    panic!("expected token from second include");
                };
                let erl_pp::Origin::Include {
                    include_site: s2, ..
                } = t2.origin()
                else {
                    panic!("expected erl_pp::Origin::Include");
                };
                assert_eq!(*s2, site2);
                return;
            }
            erl_pp::Event::Token(_) => {}
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

// ---------------------------------------------------------------------
// 12. Include source EOF does not surface as top-level erl_pp::Event::Complete
//     — only when the parent stack is fully drained.
#[test]
fn include_eof_is_not_top_level_complete() {
    let mut pp = make(
        r#"-include("h.hrl").
parent_tail."#,
    );
    let _ = step(&mut pp);
    pp.resume_include(build_source("h.hrl", "inc."))
        .expect("resume ok");
    let mut seen_parent_tail = false;
    loop {
        match step(&mut pp) {
            erl_pp::Event::Token(t) if t.text() == "parent_tail" => seen_parent_tail = true,
            erl_pp::Event::Token(_) => {}
            erl_pp::Event::Complete => break,
            other => panic!("unexpected event: {other:?}"),
        }
    }
    assert!(seen_parent_tail);
}

// ---------------------------------------------------------------------
// 13. The awaiting event's `parent_origin` is what becomes the parent
//     of the child source's `erl_pp::Origin::Include`.
#[test]
fn include_parent_origin_matches_child_include_parent() {
    let mut pp = make(r#"-include("h.hrl")."#);
    let erl_pp::Event::AwaitingInclude(req) = step(&mut pp) else {
        panic!("expected AwaitingInclude");
    };
    let include_parent = Arc::clone(&req.parent_origin);
    pp.resume_include(build_source("h.hrl", "x."))
        .expect("resume ok");
    let erl_pp::Event::Token(t) = step(&mut pp) else {
        panic!("expected token");
    };
    let erl_pp::Origin::Include { parent, .. } = t.origin() else {
        panic!("expected erl_pp::Origin::Include");
    };
    assert!(Arc::ptr_eq(parent, &include_parent));
}
