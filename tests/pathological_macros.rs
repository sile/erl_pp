//! Regression tests for pathological macro-expansion patterns.
//!
//! These cases were derived by walking OTP's `epp.erl` (`macro_arg`,
//! `expand_macro`, `expand_arg`, `stringify`, `check_uses`,
//! `count_args`) and cross-checking against a Rust Erlang formatter
//! (`efmt`) that already reads real-world macro-heavy source. Each
//! test either locks a currently-working behaviour or documents a
//! known limitation that a follow-up change will lift.

fn build_source(name: &str, text: &str) -> erl_pp::Source {
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(t) =
        erl_tokenize::scan_token(text, position).expect("test input scans without lex errors")
    {
        position = t.end();
        tokens.push(t);
    }
    erl_pp::Source::new(name, text.to_string(), tokens)
}

fn make(text: &str) -> erl_pp::Preprocessor {
    erl_pp::Preprocessor::new([build_source("m.erl", text)])
}

fn collect_lexical_texts(pp: &mut erl_pp::Preprocessor) -> Vec<String> {
    let mut out = Vec::new();
    loop {
        match pp.step().expect("no protocol errors") {
            erl_pp::Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                out.push(ppt.text().to_owned());
            }
            erl_pp::Event::Token(_)
            | erl_pp::Event::MacroDefined(_)
            | erl_pp::Event::MacroUndefined(_) => {}
            erl_pp::Event::Complete => return out,
            other => panic!("unexpected event: {other:?}"),
        }
    }
}

fn drive_until<F, T>(pp: &mut erl_pp::Preprocessor, mut probe: F) -> T
where
    F: FnMut(erl_pp::Event) -> Option<T>,
{
    loop {
        let event = pp.step().expect("no protocol errors");
        if let Some(v) = probe(event) {
            return v;
        }
    }
}

// ---------------------------------------------------------------------
// 1. Fun type inside args — outer `)` drains the fun_end sentinel.
#[test]
fn fun_type_inside_arguments() {
    let mut pp =
        make("-define(F(A, B), A).\n?F(fun((atom()) -> ok), fun((integer()) -> integer())).");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::AwaitingMacroExpansion(_) => panic!("F is defined, no event expected"),
        erl_pp::Event::PreprocessError(err) => panic!("unexpected preprocess error: {err:?}"),
        erl_pp::Event::Complete => Some(()),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 2. `fun bar/1` followed by top-level comma — FunEnd drains on `,`.
#[test]
fn fun_arity_form_then_comma() {
    let mut pp = make("?F(fun bar/1, X).");
    let call = drive_until(&mut pp, |e| match e {
        erl_pp::Event::AwaitingMacroExpansion(req) => Some(req),
        other => panic!("unexpected event: {other:?}"),
    });
    assert_eq!(call.arity, Some(2));
}

// ---------------------------------------------------------------------
// 3. Nested delimiters + binary + line comment — no cross-argument split.
#[test]
fn nested_delimiters_binary_and_comment_in_arg() {
    let mut pp = make("?F(<<X:8, Y/binary>>, [1, % inner\n                       2, 3]).");
    let call = drive_until(&mut pp, |e| match e {
        erl_pp::Event::AwaitingMacroExpansion(req) => Some(req),
        other => panic!("unexpected event: {other:?}"),
    });
    assert_eq!(call.arity, Some(2));
}

// ---------------------------------------------------------------------
// 4. Comma inside a string literal — string is a single token; the
//    interior comma never surfaces at the argument-parse level.
#[test]
fn comma_inside_string_literal_argument() {
    let mut pp = make(r#"?F("a,b", 42)."#);
    let call = drive_until(&mut pp, |e| match e {
        erl_pp::Event::AwaitingMacroExpansion(req) => Some(req),
        other => panic!("unexpected event: {other:?}"),
    });
    assert_eq!(call.arity, Some(2));
    // The first argument's single lexical token is the whole string.
    let first_arg_lex_count = call.arguments[0]
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .count();
    assert_eq!(first_arg_lex_count, 1);
}

// ---------------------------------------------------------------------
// 5. Chained constant-like rescan — `?A -> ?B -> ?C -> 1`.
#[test]
fn constant_like_chain_rescans_through_three_layers() {
    let mut pp = make("-define(A, ?B).\n-define(B, ?C).\n-define(C, 1).\n?A.");
    let texts = collect_lexical_texts(&mut pp);
    assert!(texts.contains(&"1".to_owned()));
    assert!(!texts.contains(&"?".to_owned()));
}

// ---------------------------------------------------------------------
// 6. Function-like macro inside a constant-like body — rescan path
//    now expands the nested call.
#[test]
fn function_like_in_constant_body_rescans_to_expanded_tokens() {
    let mut pp = make("-define(WRAP(X), <<X>>).\n-define(FOO, ?WRAP(42)).\n?FOO.");
    let texts = collect_lexical_texts(&mut pp);
    assert!(!texts.contains(&"?".to_owned()));
    assert!(!texts.contains(&"WRAP".to_owned()));
    // Body substituted `X` → `42`, so `<<`, `42`, `>>` appear.
    assert!(texts.contains(&"<<".to_owned()));
    assert!(texts.contains(&"42".to_owned()));
    assert!(texts.contains(&">>".to_owned()));
}

// ---------------------------------------------------------------------
// 6b. Cycle through a function-like body — static uses graph still
//     rejects `?A -> ?B(x) -> ?A` even though the second hop only
//     surfaces from the queue.
#[test]
fn function_like_rescan_detects_cycle() {
    let mut pp = make("-define(A, ?B(x)).\n-define(B(X), ?A).\n?A.");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::CircularExpansion { .. }) => {
            Some(())
        }
        erl_pp::Event::Complete => panic!("expected CircularExpansion"),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 6c. Function-like rescan with an undefined nested call surfaces
//     the AwaitingMacroExpansion event so the caller can supply the
//     expansion.
#[test]
fn function_like_rescan_fires_event_on_table_miss() {
    let mut pp = make("-define(FOO, ?UNKNOWN(1, 2)).\n?FOO.");
    let call = drive_until(&mut pp, |e| match e {
        erl_pp::Event::AwaitingMacroExpansion(req) => Some(req),
        erl_pp::Event::Complete => panic!("expected AwaitingMacroExpansion"),
        _ => None,
    });
    assert_eq!(call.name.as_str(), "UNKNOWN");
    assert_eq!(call.arity, Some(2));
}

// ---------------------------------------------------------------------
// 7. Direct constant-like recursion.
#[test]
fn direct_constant_like_recursion_is_circular() {
    let mut pp = make("-define(X, ?X).\n?X.");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::CircularExpansion { .. }) => {
            Some(())
        }
        erl_pp::Event::Complete => panic!("expected CircularExpansion"),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 8. Indirect cycle across arity boundary.
#[test]
fn indirect_cycle_across_arity_boundary_is_circular() {
    let mut pp = make("-define(X, ?Y(1)).\n-define(Y(A), ?X).\n?X.");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::CircularExpansion { .. }) => {
            Some(())
        }
        erl_pp::Event::Complete => panic!("expected CircularExpansion"),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 9. Caller-response direct recursion.
#[test]
fn caller_response_direct_recursion_is_circular() {
    let mut pp = make("?UNKNOWN.");
    let event = pp.step().expect("no protocol error");
    match event {
        erl_pp::Event::AwaitingMacroExpansion(req) => assert_eq!(req.name.as_str(), "UNKNOWN"),
        other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
    }
    let response = build_source("<synth:UNKNOWN>", "?UNKNOWN");
    pp.resume_macro_expansion(response).expect("resume ok");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::CircularExpansion { .. }) => {
            Some(())
        }
        erl_pp::Event::Complete => panic!("expected CircularExpansion"),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 10. Multi-token argument with a run of whitespace — stringification
//     joins lexical tokens with a single space regardless of source
//     spacing.
#[test]
fn stringification_collapses_source_whitespace() {
    let mut pp = make("-define(S(A), ??A).\n?S(x   +   1).");
    let value = drive_until(&mut pp, |e| match e {
        erl_pp::Event::Token(ppt) if ppt.token().kind() == erl_tokenize::TokenKind::String => {
            match ppt.value() {
                erl_tokenize::TokenValue::String(cow) => Some(cow.into_owned()),
                other => panic!("expected String value, got {other:?}"),
            }
        }
        erl_pp::Event::Complete => panic!("expected string token"),
        _ => None,
    });
    assert_eq!(value, "x + 1");
}

// ---------------------------------------------------------------------
// 11. Stringifying a String argument — the source token text is
//     re-wrapped by the escape pass, matching OTP's write_string.
#[test]
fn stringification_of_string_literal_argument() {
    let mut pp = make(
        r#"-define(S(A), ??A).
?S("hi")."#,
    );
    let value = drive_until(&mut pp, |e| match e {
        erl_pp::Event::Token(ppt) if ppt.token().kind() == erl_tokenize::TokenKind::String => {
            match ppt.value() {
                erl_tokenize::TokenValue::String(cow) => Some(cow.into_owned()),
                other => panic!("expected String value, got {other:?}"),
            }
        }
        erl_pp::Event::Complete => panic!("expected string token"),
        _ => None,
    });
    assert_eq!(value, "\"hi\"");
}

// ---------------------------------------------------------------------
// 12. Whitespace-only argument — stringifies to an empty string.
#[test]
fn stringification_of_whitespace_only_argument() {
    let mut pp = make("-define(S(A), ??A).\n?S(  ).");
    let value = drive_until(&mut pp, |e| match e {
        erl_pp::Event::Token(ppt) if ppt.token().kind() == erl_tokenize::TokenKind::String => {
            match ppt.value() {
                erl_tokenize::TokenValue::String(cow) => Some(cow.into_owned()),
                other => panic!("expected String value, got {other:?}"),
            }
        }
        erl_pp::Event::Complete => panic!("expected string token"),
        _ => None,
    });
    assert_eq!(value, "");
}

// ---------------------------------------------------------------------
// 13. `??` on a non-parameter target — error.
#[test]
fn stringification_of_non_parameter_is_invalid() {
    let mut pp = make("-define(S(A), ??Foo).\n?S(x).");
    drive_until(&mut pp, |e| match e {
        erl_pp::Event::PreprocessError(erl_pp::PreprocessError::InvalidStringificationTarget {
            ..
        }) => Some(()),
        erl_pp::Event::Complete => panic!("expected InvalidStringificationTarget"),
        _ => None,
    });
}

// ---------------------------------------------------------------------
// 14. `?LINE` inside a macro body — resolves against the outer call
//     site's line, not the definition line.
#[test]
fn line_inside_macro_body_uses_outer_caller_line() {
    // The `?HERE` call is on line 3 (blank second line included), so
    // `?LINE` should synth as `3` even though the definition itself
    // sits on line 1.
    let mut pp = make("-define(HERE, ?LINE).\n\n?HERE.");
    let ppt = drive_until(&mut pp, |e| match e {
        erl_pp::Event::Token(ppt) if ppt.text() == "3" => Some(ppt),
        erl_pp::Event::Complete => panic!("expected integer synth token `3`"),
        _ => None,
    });
    assert!(matches!(
        ppt.origin(),
        erl_pp::Origin::SourceInfo {
            kind: erl_pp::SourceInfoMacroKind::Line,
            ..
        }
    ));
}

// ---------------------------------------------------------------------
// 15. Macro at form boundary, expanding to nothing lexical —
//     the following `-define` directive must still be parsed.
#[test]
fn macro_at_form_boundary_expanding_to_nothing_leaves_directive_parsable() {
    let mut pp = make("-define(NL, ).\n?NL\n-define(X, 1).\n?X.");
    let texts = collect_lexical_texts(&mut pp);
    // The final expansion should have emitted `1` (from ?X).
    assert!(texts.contains(&"1".to_owned()));
    // Sanity: raw `?` tokens must not leak out.
    assert!(!texts.contains(&"?".to_owned()));
}
