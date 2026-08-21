//! Shared helpers for `pbt_state_machine.rs`.
//!
//! Cargo compiles every `tests/*.rs` as its own integration-test binary,
//! so each binary only uses a subset of these helpers. Silence
//! `dead_code` on the `mod pbt_harness;` declaration in the consuming
//! file, not here.

use std::cell::Cell;
use std::collections::BTreeSet;

/// Environment variable used by `noprop::seed_from_env_or_time` to
/// reproduce a failing case.
pub const SEED_ENV: &str = "ERL_PP_SEED";

/// Number of cases to run per property.
pub const CASES: usize = 256;

/// Upper bound on the number of top-level items (`-define`, `-ifdef`,
/// atom-comma expressions, …) a single generated program contains.
pub const MAX_ITEMS: usize = 6;

/// Upper bound on the number of steps we drive the preprocessor for
/// (per case, per side of a fork).
pub const MAX_STEPS: usize = 512;

// ============================================================
// Coverage counters
// ============================================================

/// `Cell<usize>` newtype for coverage counting inside a noprop closure
/// (`Fn` requires interior mutability). Matches the pattern used in
/// `tests/pbt_macro_body.rs`.
#[derive(Default)]
pub struct Counter(Cell<usize>);

impl Counter {
    pub fn hit(&self) {
        self.0.set(self.0.get() + 1);
    }

    pub fn get(&self) -> usize {
        self.0.get()
    }
}

/// Collect the labels seen this run.
#[derive(Default)]
pub struct LabelSet(std::cell::RefCell<BTreeSet<String>>);

impl LabelSet {
    pub fn hit(&self, label: impl Into<String>) {
        self.0.borrow_mut().insert(label.into());
    }

    pub fn contains(&self, label: &str) -> bool {
        self.0.borrow().contains(label)
    }
}

// ============================================================
// erl_pp::Source construction
// ============================================================

/// Tokenize `text` with `erl_tokenize::scan_token`. Panics on
/// tokenization failure — property generators produce well-formed
/// programs, so lexical failures indicate a generator bug.
pub fn scan_all(text: &str) -> Vec<erl_tokenize::Token> {
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(t) =
        erl_tokenize::scan_token(text, position).expect("generated source scans cleanly")
    {
        position = t.end();
        tokens.push(t);
    }
    tokens
}

/// Build an in-memory [`erl_pp::Source`] with the given display name and text.
pub fn build_source(name: &str, text: &str) -> erl_pp::Source {
    let tokens = scan_all(text);
    erl_pp::Source::new(name, text.to_string(), tokens)
}

// ============================================================
// erl_pp::Preprocessor driving helpers
// ============================================================

/// Drive the preprocessor until it either produces `erl_pp::Event::Complete`
/// or steps `MAX_STEPS` times. Every non-terminal event is appended to
/// the returned `Vec`. If the machine hits an `Awaiting*` state, the
/// caller is expected to be running with an input that never triggers
/// external responses (used by tests that only exercise `erl_pp::Event::Token`
/// / `erl_pp::Event::Complete` paths).
///
/// Returns `Err(erl_pp::ProtocolError)` if `step` returns one; the panic-free
/// contract of the preprocessor lets us surface protocol errors as
/// test values rather than aborting the case.
pub fn drive_to_complete(
    pp: &mut erl_pp::Preprocessor,
) -> Result<Vec<erl_pp::Event>, erl_pp::ProtocolError> {
    let mut events = Vec::new();
    for _ in 0..MAX_STEPS {
        let ev = pp.step()?;
        let done = matches!(ev, erl_pp::Event::Complete);
        events.push(ev);
        if done {
            return Ok(events);
        }
    }
    // Loop budget exhausted — treat as a case failure rather than a
    // hang. The property should assert on `events.len() < MAX_STEPS`
    // when it needs bounded progress.
    Ok(events)
}

/// Step the preprocessor once. Convenience wrapper that panics on
/// `erl_pp::ProtocolError` — useful in properties where the current state was
/// generated to be `Scanning`.
pub fn step_expect_ok(pp: &mut erl_pp::Preprocessor) -> erl_pp::Event {
    pp.step()
        .expect("preprocessor was in Scanning; step must not protocol-error")
}
