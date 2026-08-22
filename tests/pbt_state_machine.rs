//! Property-based tests for the Sans-I/O `erl_pp::Preprocessor` state machine.
//!
//! Complements the macro-body-focused PBT in `tests/pbt_macro_body.rs`
//! by exercising the wider state machine: include push/pop,
//! conditional then/else/fork, macro table updates, diagnostics,
//! branch-boundary events, Complete stability, and protocol errors.

#[expect(dead_code)]
mod pbt_harness;

use std::assert_matches;

use noprop::{Ratio, Runner, TestCaseContext, TestResult};

use pbt_harness::{
    CASES, Counter, LabelSet, MAX_ITEMS, MAX_STEPS, SEED_ENV, build_source, step_expect_ok,
};

const ATOMS: [&str; 6] = ["foo", "bar", "baz", "qux", "spam", "eggs"];
const MACROS: [&str; 4] = ["MYFOO", "MYBAR", "MYBAZ", "MYQUX"];

// ============================================================
// Simple program generator (no directives that request a response)
// ============================================================

fn sample_simple_program(ctx: &mut TestCaseContext) -> String {
    let count =
        noprop::sample_with_boundaries(ctx, &[0usize, 1, MAX_ITEMS], Ratio::one_nth(5), |ctx| {
            noprop::sample_usize_in(ctx, 0..=MAX_ITEMS)
        });
    let mut buf = String::new();
    let mut defined: Vec<&'static str> = Vec::new();
    for _ in 0..count {
        match noprop::sample_weighted_index(ctx, &[3, 3, 1, 2]) {
            0 => {
                let atom = noprop::sample_choice(ctx, &ATOMS);
                buf.push_str(atom);
                buf.push_str(".\n");
            }
            1 => {
                let name = noprop::sample_choice(ctx, &MACROS);
                let value = noprop::sample_choice(ctx, &ATOMS);
                buf.push_str(&format!("-define({name}, {value}).\n"));
                if !defined.contains(&name) {
                    defined.push(name);
                }
            }
            2 => {
                if !defined.is_empty() {
                    let name = defined.remove(defined.len() - 1);
                    buf.push_str(&format!("-undef({name}).\n"));
                }
            }
            _ => {
                if defined.is_empty() {
                    let atom = noprop::sample_choice(ctx, &ATOMS);
                    buf.push_str(atom);
                    buf.push_str(".\n");
                } else {
                    let idx = noprop::sample_usize_in(ctx, 0..defined.len());
                    let name = defined[idx];
                    buf.push_str(&format!("?{name}.\n"));
                }
            }
        }
    }
    buf
}

fn run_simple(source: erl_pp::Source) -> Vec<erl_pp::Event> {
    let mut pp = erl_pp::Preprocessor::new([source]);
    let mut events = Vec::new();
    for _ in 0..MAX_STEPS {
        let ev = step_expect_ok(&mut pp);
        let done = matches!(ev, erl_pp::Event::Complete);
        assert!(
            !matches!(
                ev,
                erl_pp::Event::AwaitingInclude(_)
                    | erl_pp::Event::AwaitingConditional(_)
                    | erl_pp::Event::AwaitingMacroExpansion(_)
            ),
            "simple program generator produced an Awaiting event: {ev:?}"
        );
        events.push(ev);
        if done {
            return events;
        }
    }
    panic!("simple program did not reach Complete within {MAX_STEPS} steps");
}

// ------------------------------------------------------------
// Property: deterministic replay
// ------------------------------------------------------------
#[test]
fn deterministic_replay_of_simple_program() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_multi_event = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let text = sample_simple_program(ctx);
        let a = run_simple(build_source("a.erl", &text));
        let b = run_simple(build_source("a.erl", &text));
        assert_eq!(a.len(), b.len(), "event count differs (text={text:?})");
        for (i, (ea, eb)) in a.iter().zip(b.iter()).enumerate() {
            match (ea, eb) {
                (erl_pp::Event::Token(ta), erl_pp::Event::Token(tb)) => {
                    assert_eq!(
                        ta.text(),
                        tb.text(),
                        "token[{i}] text differs (text={text:?})"
                    );
                }
                (erl_pp::Event::Complete, erl_pp::Event::Complete) => {}
                (erl_pp::Event::MacroDefined(_), erl_pp::Event::MacroDefined(_))
                | (erl_pp::Event::MacroUndefined(_), erl_pp::Event::MacroUndefined(_)) => {}
                _ => panic!("event[{i}] kinds differ: {ea:?} vs {eb:?}"),
            }
        }
        if a.len() > 3 {
            saw_multi_event.hit();
        }
        Ok(())
    })?;
    assert!(
        saw_multi_event.get() > 0,
        "no case exercised a program that produced more than 3 events\n{runner}"
    );
    Ok(())
}

// ------------------------------------------------------------
// Property: Complete is stable and does not touch macro state
// ------------------------------------------------------------
#[test]
fn complete_after_step_is_stable() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let text = sample_simple_program(ctx);
        let mut pp = erl_pp::Preprocessor::new([build_source("stable.erl", &text)]);
        loop {
            let ev = step_expect_ok(&mut pp);
            if matches!(ev, erl_pp::Event::Complete) {
                break;
            }
        }
        let len_before = pp.macros().len();
        for _ in 0..4 {
            let ev = step_expect_ok(&mut pp);
            assert!(
                matches!(ev, erl_pp::Event::Complete),
                "step after Complete returned {ev:?}"
            );
        }
        assert_eq!(pp.macros().len(), len_before);
        Ok(())
    })?;
    Ok(())
}

// ------------------------------------------------------------
// Property: Event::Token is lexical only
// ------------------------------------------------------------
#[test]
fn event_stream_omits_hidden_tokens() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_input_hidden = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        // Generate a program that always contains some whitespace and
        // possibly a comment, then check that none of it is re-emitted.
        let atom = noprop::sample_choice(ctx, &ATOMS);
        let with_comment = noprop::sample_bool(ctx);
        let text = if with_comment {
            format!("{atom}. % note\n{atom}.\n")
        } else {
            format!("  {atom} ,  {atom}.\n")
        };
        let source = build_source("h.erl", &text);
        let input_hidden = source
            .tokens()
            .iter()
            .filter(|t| !t.kind().is_lexical())
            .count();
        let events = run_simple(source);
        let seen_hidden = events
            .iter()
            .filter(|e| matches!(e, erl_pp::Event::Token(t) if !t.token().kind().is_lexical()))
            .count();
        assert_eq!(
            seen_hidden, 0,
            "hidden tokens leaked into Event::Token (text={text:?})"
        );
        if input_hidden > 0 {
            saw_input_hidden.hit();
        }
        Ok(())
    })?;
    assert!(
        saw_input_hidden.get() > 0,
        "no case exercised a hidden-token bearing program\n{runner}"
    );
    Ok(())
}

// ------------------------------------------------------------
// Property: bounded progress — every simple program reaches Complete
// within MAX_STEPS.
// ------------------------------------------------------------
#[test]
fn simple_program_reaches_complete_within_bounded_steps() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_nonempty = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let text = sample_simple_program(ctx);
        let events = run_simple(build_source("b.erl", &text));
        assert!(events.len() < MAX_STEPS);
        assert_matches!(events.last(), Some(erl_pp::Event::Complete));
        if events.len() > 1 {
            saw_nonempty.hit();
        }
        Ok(())
    })?;
    assert!(
        saw_nonempty.get() > 0,
        "every generated program was empty; generator support is too narrow\n{runner}"
    );
    Ok(())
}

// ============================================================
// Conditional / branch-boundary property
// ============================================================

// ------------------------------------------------------------
// Property: -ifdef Then / Else selection propagates the right macro
// definitions to Complete.
// ------------------------------------------------------------
#[test]
fn ifdef_then_and_else_select_effective_branch() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_then_selected = Counter::default();
    let saw_else_selected = Counter::default();
    let saw_else_boundary = Counter::default();
    let saw_endif_boundary = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let cond = noprop::sample_choice(ctx, &MACROS);
        let then_def = noprop::sample_choice(ctx, &MACROS);
        let else_def = noprop::sample_choice(ctx, &MACROS);
        let choose_then = noprop::sample_bool(ctx);
        let branch = if choose_then {
            erl_pp::Branch::Then
        } else {
            erl_pp::Branch::Else
        };
        let text = format!(
            "-ifdef({cond}).\n\
             -define({then_def}, ok).\n\
             -else.\n\
             -define({else_def}, ok).\n\
             -endif.\n"
        );
        let mut pp = erl_pp::Preprocessor::new([build_source("c.erl", &text)]);
        let mut resumed = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingConditional(req) => {
                    assert!(!resumed, "double AwaitingConditional");
                    let erl_pp::Conditional::Ifdef(d) = req else {
                        panic!("expected Ifdef, got {req:?}");
                    };
                    assert_eq!(d.name.as_str(), cond);
                    pp.resume_conditional(branch).expect("resume");
                    resumed = true;
                }
                erl_pp::Event::BranchBoundary(erl_pp::BranchBoundary::Else { .. }) => {
                    saw_else_boundary.hit()
                }
                erl_pp::Event::BranchBoundary(erl_pp::BranchBoundary::Endif { .. }) => {
                    saw_endif_boundary.hit()
                }
                erl_pp::Event::Complete => break,
                erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_) => {}
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(resumed, "AwaitingConditional never appeared");
        let table = pp.macros();
        if choose_then {
            assert!(
                table.is_defined(then_def) && !table.is_defined(else_def) || then_def == else_def,
                "then branch left macros in unexpected state"
            );
            saw_then_selected.hit();
        } else {
            assert!(
                table.is_defined(else_def) && !table.is_defined(then_def) || then_def == else_def,
                "else branch left macros in unexpected state"
            );
            saw_else_selected.hit();
        }
        Ok(())
    })?;
    assert!(saw_then_selected.get() > 0, "Then never selected\n{runner}");
    assert!(saw_else_selected.get() > 0, "Else never selected\n{runner}");
    assert!(
        saw_else_boundary.get() > 0,
        "no Else BranchBoundary observed\n{runner}"
    );
    assert!(
        saw_endif_boundary.get() > 0,
        "no Endif BranchBoundary observed\n{runner}"
    );
    Ok(())
}

// ------------------------------------------------------------
// Property: fork at AwaitingConditional produces two independent
// runs; the branches' macro tables reflect their own selection.
// ------------------------------------------------------------
#[test]
fn conditional_fork_yields_independent_macro_tables() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_fork = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let cond = noprop::sample_choice(ctx, &MACROS);
        let then_def = noprop::sample_choice(ctx, &MACROS);
        let else_def = noprop::sample_choice(ctx, &MACROS);
        if then_def == else_def {
            // Symmetric macros make the property vacuous; skip.
            return Ok(());
        }
        let text = format!(
            "-ifdef({cond}).\n\
             -define({then_def}, ok).\n\
             -else.\n\
             -define({else_def}, ok).\n\
             -endif.\n"
        );
        let mut pp = erl_pp::Preprocessor::new([build_source("fork.erl", &text)]);
        loop {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingConditional(_) => break,
                erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_)
                | erl_pp::Event::BranchBoundary(_) => {}
                other => panic!("unexpected event before fork: {other:?}"),
            }
        }
        let mut pp_else = pp.clone();
        pp.resume_conditional(erl_pp::Branch::Then).expect("then");
        pp_else
            .resume_conditional(erl_pp::Branch::Else)
            .expect("else");
        let drain = |mut p: erl_pp::Preprocessor| -> erl_pp::Preprocessor {
            for _ in 0..MAX_STEPS {
                if let erl_pp::Event::Complete = step_expect_ok(&mut p) {
                    return p;
                }
            }
            panic!("fork side did not complete");
        };
        let pp_then_done = drain(pp);
        let pp_else_done = drain(pp_else);
        assert!(pp_then_done.macros().is_defined(then_def));
        assert!(!pp_then_done.macros().is_defined(else_def));
        assert!(pp_else_done.macros().is_defined(else_def));
        assert!(!pp_else_done.macros().is_defined(then_def));
        saw_fork.hit();
        Ok(())
    })?;
    assert!(saw_fork.get() > 0, "no fork exercised\n{runner}");
    Ok(())
}

// ============================================================
// Include property
// ============================================================

// ------------------------------------------------------------
// Property: -include response drives events from the include source
// first, then resumes the parent source with the correct token.
// ------------------------------------------------------------
#[test]
fn include_response_streams_include_tokens_before_parent_resume() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_include = Counter::default();
    let seen_origin_kinds = LabelSet::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let kind = if noprop::sample_bool(ctx) {
            erl_pp::IncludeKind::Include
        } else {
            erl_pp::IncludeKind::IncludeLib
        };
        let inner_atom = noprop::sample_choice(ctx, &ATOMS);
        let outer_atom = noprop::sample_choice(ctx, &ATOMS);
        let include_directive = match kind {
            erl_pp::IncludeKind::Include => r#"-include("hdr.hrl")."#.to_owned(),
            erl_pp::IncludeKind::IncludeLib => r#"-include_lib("app/include/hdr.hrl")."#.to_owned(),
        };
        let text = format!("{include_directive}\n{outer_atom}.\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("main.erl", &text)]);
        let mut inner_first_atom: Option<String> = None;
        let mut outer_atom_after: Option<String> = None;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingInclude(req) => {
                    assert_eq!(req.kind, kind);
                    let inner = build_source("hdr.hrl", &format!("{inner_atom}.\n"));
                    pp.resume_include(inner).expect("resume include");
                    saw_include.hit();
                }
                erl_pp::Event::Token(t) if t.token().kind().is_lexical() => match t.origin() {
                    erl_pp::Origin::Include { .. } => {
                        seen_origin_kinds.hit("include");
                        if inner_first_atom.is_none() && t.text() == inner_atom {
                            inner_first_atom = Some(t.text().to_owned());
                        }
                    }
                    erl_pp::Origin::Source => {
                        seen_origin_kinds.hit("source");
                        if inner_first_atom.is_some()
                            && outer_atom_after.is_none()
                            && t.text() == outer_atom
                        {
                            outer_atom_after = Some(t.text().to_owned());
                        }
                    }
                    _ => {}
                },
                erl_pp::Event::Token(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(
            inner_first_atom.is_some(),
            "include source tokens did not surface"
        );
        assert!(
            outer_atom_after.is_some(),
            "parent source did not resume after include"
        );
        Ok(())
    })?;
    assert!(saw_include.get() > 0, "no include exercised\n{runner}");
    assert!(
        seen_origin_kinds.contains("include") && seen_origin_kinds.contains("source"),
        "did not observe both erl_pp::Origin::Include and erl_pp::Origin::Source token flows\n{runner}"
    );
    Ok(())
}

// ------------------------------------------------------------
// Property: rejecting an include (empty erl_pp::Source) surfaces the parent
// source's next token immediately.
// ------------------------------------------------------------
#[test]
fn include_reject_returns_directly_to_parent() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_reject = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let outer_atom = noprop::sample_choice(ctx, &ATOMS);
        let text = format!("-include(\"hdr.hrl\").\n{outer_atom}.\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("main.erl", &text)]);
        let mut got_outer = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingInclude(_) => {
                    let empty = build_source("hdr.hrl", "");
                    pp.resume_include(empty).expect("empty include");
                    saw_reject.hit();
                }
                erl_pp::Event::Token(t)
                    if t.token().kind().is_lexical() && t.text() == outer_atom =>
                {
                    // Must be from the parent source (erl_pp::Origin::Source).
                    assert!(
                        matches!(t.origin(), erl_pp::Origin::Source),
                        "outer atom appeared with unexpected origin: {:?}",
                        t.origin()
                    );
                    got_outer = true;
                }
                erl_pp::Event::Token(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(got_outer, "parent-source atom never surfaced after reject");
        Ok(())
    })?;
    assert!(saw_reject.get() > 0, "no reject exercised\n{runner}");
    Ok(())
}

// ------------------------------------------------------------
// Property: -define inside an include source is visible in the
// parent (macros table is a single instance across include push/pop).
// ------------------------------------------------------------
#[test]
fn include_scoped_define_reaches_parent() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_inherit = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let name = noprop::sample_choice(ctx, &MACROS);
        let atom = noprop::sample_choice(ctx, &ATOMS);
        let text = format!("-include(\"hdr.hrl\").\n?{name}.\n");
        let inner_text = format!("-define({name}, {atom}).\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("main.erl", &text)]);
        let mut expanded = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingInclude(_) => {
                    pp.resume_include(build_source("hdr.hrl", &inner_text))
                        .expect("resume");
                }
                erl_pp::Event::Token(t) if t.token().kind().is_lexical() && t.text() == atom => {
                    // Must reach the expanded macro value.
                    if let erl_pp::Origin::Source
                    | erl_pp::Origin::Include { .. }
                    | erl_pp::Origin::MacroBody { .. } = t.origin()
                    {
                        expanded = true;
                    }
                }
                erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(expanded, "include-defined macro did not expand in parent");
        assert!(pp.macros().is_defined(name));
        saw_inherit.hit();
        Ok(())
    })?;
    assert!(
        saw_inherit.get() > 0,
        "no include-macro inheritance exercised\n{runner}"
    );
    Ok(())
}

// ------------------------------------------------------------
// Property: nested include produces an erl_pp::Origin::Include chain whose
// parent is another erl_pp::Origin::Include (not erl_pp::Origin::Source).
// ------------------------------------------------------------
#[test]
fn nested_include_forms_origin_chain() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_nested = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let atom = noprop::sample_choice(ctx, &ATOMS);
        let outer = format!(r#"-include("mid.hrl").{atom}."#);
        let mid = format!(r#"-include("inner.hrl").{atom}."#);
        let inner = format!("{atom}.");
        let mut pp = erl_pp::Preprocessor::new([build_source("outer.erl", &outer)]);
        let mut awaiting_stack = vec![mid.clone(), inner.clone()];
        let mut deepest_depth = 0usize;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingInclude(_) => {
                    let next = awaiting_stack.remove(0);
                    let name = if awaiting_stack.is_empty() {
                        "inner.hrl"
                    } else {
                        "mid.hrl"
                    };
                    pp.resume_include(build_source(name, &next))
                        .expect("resume");
                }
                erl_pp::Event::Token(t) if t.token().kind().is_lexical() => {
                    let depth = origin_include_depth(t.origin());
                    if depth > deepest_depth {
                        deepest_depth = depth;
                    }
                }
                erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(
            deepest_depth >= 2,
            "expected erl_pp::Origin::Include chain depth >= 2, got {deepest_depth}"
        );
        saw_nested.hit();
        Ok(())
    })?;
    assert!(
        saw_nested.get() > 0,
        "no nested include exercised\n{runner}"
    );
    Ok(())
}

fn origin_include_depth(origin: &erl_pp::Origin) -> usize {
    let mut depth = 0;
    let mut cur = origin;
    while let erl_pp::Origin::Include { parent, .. } = cur {
        depth += 1;
        cur = parent;
    }
    depth
}

// ------------------------------------------------------------
// Property: nested conditional — inner `-ifdef` inside a chosen
// outer branch works and produces the expected macro definitions.
// ------------------------------------------------------------
#[test]
fn nested_conditional_selects_inner_branch_correctly() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_nested = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let outer = noprop::sample_choice(ctx, &MACROS);
        let inner = noprop::sample_choice(ctx, &MACROS);
        let mark = noprop::sample_choice(ctx, &MACROS);
        if outer == inner || outer == mark || inner == mark {
            return Ok(());
        }
        let text = format!(
            "-ifdef({outer}).\n\
             -ifdef({inner}).\n\
             -define({mark}, ok).\n\
             -endif.\n\
             -endif.\n"
        );
        let mut pp = erl_pp::Preprocessor::new([build_source("n.erl", &text)]);
        let mut await_count = 0usize;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingConditional(_) => {
                    await_count += 1;
                    // Always take Then so both -ifdef enter their bodies.
                    pp.resume_conditional(erl_pp::Branch::Then).expect("resume");
                }
                erl_pp::Event::BranchBoundary(_)
                | erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(
            await_count, 2,
            "expected 2 AwaitingConditional events (outer + inner), got {await_count}"
        );
        assert!(pp.macros().is_defined(mark));
        saw_nested.hit();
        Ok(())
    })?;
    assert!(
        saw_nested.get() > 0,
        "no nested conditional exercised\n{runner}"
    );
    Ok(())
}

// ============================================================
// Macro expansion property
// ============================================================

// ------------------------------------------------------------
// Property: caller-driven macro expansion — unknown ?NAME triggers
// AwaitingMacroExpansion; the substitute source replaces the call.
// ------------------------------------------------------------
#[test]
fn caller_macro_expansion_substitutes_response_source() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_expansion = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let name = noprop::sample_choice(ctx, &MACROS);
        let substitute = noprop::sample_choice(ctx, &ATOMS);
        // Undefined macro forces AwaitingMacroExpansion.
        let text = format!("?{name}.\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("m.erl", &text)]);
        let mut saw_substitute = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingMacroExpansion(req) => {
                    assert_eq!(req.name.as_str(), name);
                    let sub = build_source("<expansion>", substitute);
                    pp.resume_macro_expansion(sub).expect("expand");
                    saw_expansion.hit();
                }
                erl_pp::Event::Token(t)
                    if t.token().kind().is_lexical() && t.text() == substitute =>
                {
                    assert_matches!(t.origin(), erl_pp::Origin::CallerExpansion { .. });
                    saw_substitute = true;
                }
                erl_pp::Event::Token(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_substitute, "substitute token never surfaced");
        Ok(())
    })?;
    assert!(
        saw_expansion.get() > 0,
        "no AwaitingMacroExpansion exercised\n{runner}"
    );
    Ok(())
}

// ============================================================
// erl_pp::Diagnostic property
// ============================================================

// ------------------------------------------------------------
// Property: -error / -warning surface as erl_pp::Event::Diagnostic with the
// expected severity; state machine keeps advancing.
// ------------------------------------------------------------
#[test]
fn error_and_warning_surface_as_diagnostics() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_error = Counter::default();
    let saw_warning = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let severity = if noprop::sample_bool(ctx) {
            erl_pp::Severity::Error
        } else {
            erl_pp::Severity::Warning
        };
        let directive = match severity {
            erl_pp::Severity::Error => "-error",
            erl_pp::Severity::Warning => "-warning",
        };
        let atom = noprop::sample_choice(ctx, &ATOMS);
        let text = format!("{directive}({atom}).\n{atom}.\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("d.erl", &text)]);
        let mut got_diag = false;
        let mut got_post_diag_atom = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::Diagnostic(d) => {
                    assert_eq!(d.severity, severity);
                    assert!(
                        d.arguments
                            .iter()
                            .any(|t| t.token().kind().is_lexical() && t.text() == atom),
                        "diagnostic did not carry the atom argument"
                    );
                    assert_matches!(*d.parent_origin, erl_pp::Origin::Source);
                    got_diag = true;
                    match severity {
                        erl_pp::Severity::Error => saw_error.hit(),
                        erl_pp::Severity::Warning => saw_warning.hit(),
                    }
                }
                erl_pp::Event::Token(t) if t.token().kind().is_lexical() && t.text() == atom => {
                    if got_diag {
                        got_post_diag_atom = true;
                    }
                }
                erl_pp::Event::Token(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(got_diag, "diagnostic never fired");
        assert!(
            got_post_diag_atom,
            "state machine did not resume after diagnostic"
        );
        Ok(())
    })?;
    assert!(saw_error.get() > 0, "no -error exercised\n{runner}");
    assert!(saw_warning.get() > 0, "no -warning exercised\n{runner}");
    Ok(())
}

// ============================================================
// ?FILE / ?LINE synth-token property
// ============================================================

// ------------------------------------------------------------
// Property: ?FILE and ?LINE emit synthesized tokens with the
// SourceInfo origin kind.
// ------------------------------------------------------------
#[test]
fn source_info_macros_emit_synthesized_tokens() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_file = Counter::default();
    let saw_line = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let use_file = noprop::sample_bool(ctx);
        let text = if use_file { "?FILE.\n" } else { "?LINE.\n" };
        let mut pp = erl_pp::Preprocessor::new([build_source("info.erl", text)]);
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::Token(t) if t.token().kind().is_lexical() => {
                    if let erl_pp::Origin::SourceInfo { kind, .. } = t.origin() {
                        match kind {
                            erl_pp::SourceInfoMacroKind::File => saw_file.hit(),
                            erl_pp::SourceInfoMacroKind::Line => saw_line.hit(),
                        }
                    }
                }
                erl_pp::Event::Token(_) => {}
                erl_pp::Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        Ok(())
    })?;
    assert!(
        saw_file.get() > 0,
        "?FILE never emitted a synth token\n{runner}"
    );
    assert!(
        saw_line.get() > 0,
        "?LINE never emitted a synth token\n{runner}"
    );
    Ok(())
}

// ============================================================
// Protocol error property
// ============================================================

// ------------------------------------------------------------
// Property: wrong response kind returns erl_pp::ProtocolError but does not
// corrupt the pending state (subsequent correct response works).
// ------------------------------------------------------------
#[test]
fn wrong_response_kind_returns_protocol_error_without_state_damage() -> TestResult {
    let seed = noprop::seed_from_env_or_time(SEED_ENV)?;
    let saw_wrong = Counter::default();
    let mut runner = Runner::new(seed);
    runner.run(CASES, |ctx| {
        let name = noprop::sample_choice(ctx, &MACROS);
        let branch = if noprop::sample_bool(ctx) {
            erl_pp::Branch::Then
        } else {
            erl_pp::Branch::Else
        };
        let text = format!("-ifdef({name}).\nfoo.\n-else.\nbar.\n-endif.\n");
        let mut pp = erl_pp::Preprocessor::new([build_source("t.erl", &text)]);
        let mut awaiting = false;
        for _ in 0..MAX_STEPS {
            match step_expect_ok(&mut pp) {
                erl_pp::Event::AwaitingConditional(_) => {
                    awaiting = true;
                    break;
                }
                erl_pp::Event::Token(_)
                | erl_pp::Event::MacroDefined(_)
                | erl_pp::Event::MacroUndefined(_) => {}
                other => panic!("unexpected event before AwaitingConditional: {other:?}"),
            }
        }
        assert!(awaiting, "AwaitingConditional never surfaced");
        // Feed the wrong response kind: include instead of conditional.
        let wrong = pp.resume_include(build_source("dummy.hrl", ""));
        assert!(wrong.is_err(), "wrong resume_include should error");
        // The wait was not consumed: step still fails, and the
        // matching resume still works.
        assert!(
            pp.step().is_err(),
            "step while still awaiting conditional must protocol-error"
        );
        pp.resume_conditional(branch)
            .expect("recover after protocol error");
        // Drain to Complete.
        for _ in 0..MAX_STEPS {
            if matches!(step_expect_ok(&mut pp), erl_pp::Event::Complete) {
                break;
            }
        }
        saw_wrong.hit();
        Ok(())
    })?;
    assert!(saw_wrong.get() > 0, "no wrong-response exercised\n{runner}");
    Ok(())
}
