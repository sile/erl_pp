//! Erlang source code preprocessor.
//!
//! A Sans-I/O state machine for language tools. The caller tokenizes
//! ([`erl_tokenize::scan_token`]), performs I/O, searches for includes,
//! evaluates `-if` / `-elif`, and decides what unknown macros mean.
//! This crate advances directives, the macro table, and the condition
//! stack.
//!
//! Feed a sequence of [`Source`]s into [`Preprocessor`] and call
//! [`Preprocessor::step`] in a loop. [`Event::Token`] is lexical only;
//! whitespace and comments stay on [`Source`].
//!
//! Include search, environment expansion, cycle / depth limits, and
//! encoding are outside [`Preprocessor`]. [`IncludeDirective`] carries
//! the decoded path and the directive's span / origin.
//! [`open_include`] is optional OTP 29.0 path resolution and returns
//! the opened path, not a file handle. Differences from OTP `epp` are
//! in [`docs::otp_differences`].
//!
//! [`Preprocessor`] implements [`Clone`]: the clone shares
//! [`SourceStore`]; cursor, macro table, and branch stack are
//! independent. There is no API that merges two forked machines.
//! [`Clone`] is isolation, not undo after an error.
//!
//! # Minimal event loop
//!
//! Trivial input with no directives, includes, conditionals, macros, or
//! `-error` / `-warning`. All `Awaiting*` / `MacroDefined` /
//! `MacroUndefined` / `Diagnostic` / `PreprocessError` branches are
//! unreachable and fall into the `unreachable!` arm.
//!
//! ```
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let text = "atom, foo, bar.";
//! let source = erl_pp::Source::from_text("example.erl", text)?;
//! let mut pp = erl_pp::Preprocessor::new([source]);
//!
//! let mut lexical = Vec::<String>::new();
//! loop {
//!     match pp.step()? {
//!         erl_pp::Event::Token(t) => {
//!             lexical.push(t.text().to_owned());
//!         }
//!         erl_pp::Event::Complete => break,
//!         other => unreachable!("unexpected event: {other:?}"),
//!     }
//! }
//! assert_eq!(lexical, ["atom", ",", "foo", ",", "bar", "."]);
//! # Ok(())
//! # }
//! ```
//!
//! # Driving `step`
//!
//! Each [`Event`] variant documents its own contract. The skeleton
//! below is a driver: resume the three waits, decide whether a
//! diagnostic or input error is fatal, and treat the rest as
//! observation. There is no skip-to-next-form or rewind API; recover
//! by continuing `step` or dropping [`Preprocessor`].
//!
//! [`ProtocolError`] from `step` is a driver bug (`step` while awaiting,
//! or the wrong `resume_*`). State is unchanged; the last [`Event`]
//! names the wait.
//!
//! ```rust,ignore
//! loop {
//!     match pp.step() {
//!         Ok(erl_pp::Event::Token(t)) => { /* accumulate; lexical only */ }
//!         Ok(erl_pp::Event::AwaitingInclude(_)) => {
//!             pp.resume_include(source)?; // empty Source skips
//!         }
//!         Ok(erl_pp::Event::AwaitingConditional(_)) => {
//!             pp.resume_conditional(branch)?;
//!         }
//!         Ok(erl_pp::Event::AwaitingMacroExpansion(_)) => {
//!             pp.resume_macro_expansion(empty)?; // empty Source skips
//!         }
//!         Ok(erl_pp::Event::Diagnostic(_) | erl_pp::Event::PreprocessError(_)) => {
//!             // Record and step, or break and drop `pp`.
//!         }
//!         Ok(erl_pp::Event::Complete) => break,
//!         Err(erl_pp::ProtocolError) => { /* driver bug; state unchanged */ }
//!         Ok(_) => { /* MacroDefined, MacroUndefined, BranchBoundary */ }
//!     }
//! }
//! ```
//!
//! Typical policies:
//!
//! | | Compiler-like | Formatter / linter |
//! | --- | --- | --- |
//! | Include | Filesystem ([`open_include`] or equivalent) | In-memory or empty [`Source`] |
//! | `-ifdef` / `-ifndef` | `recommended` | `recommended`, or [`Clone`] both sides |
//! | `-if` / `-elif` | Evaluate | Evaluate, or pick a [`Branch`] |
//! | Unknown macro | Error, or implement | Empty expansion |
//! | [`Event::Diagnostic`] Error | Fail the file | Record and continue |
//!
//! An empty resume succeeds and emits nothing; it is not OTP epp's undef
//! error. Drop without `resume_*` if abandoning a wait; do not
//! [`Preprocessor::step`] while awaiting.
//!
//! Copy-paste driver recipes (seed macros, and more as they are added)
//! live in [`docs::recipes`].
//!
//! Lexical errors never reach [`Preprocessor::step`]. Inactive-branch
//! `-define`, includes, diagnostics, unknown macros, and parse failures
//! do not surface. Include failure is not a preprocessor error. A
//! source sequence continues after an error in an earlier source.
//!
//! # References
//!
//! - [Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html)
#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub use crate::error::{PreprocessError, ProtocolError};
pub use crate::event::{
    Branch, BranchBoundary, Conditional, DefinedConditional, Diagnostic, Event,
    ExpressionConditional, IncludeDirective, MacroCall, Severity, UndefinedMacro,
};
pub use crate::include_path::{OpenIncludeError, open_include};
pub use crate::macros::{MacroDefinition, MacroTable};
pub use crate::origin::{IncludeKind, Origin, SourceInfoMacroKind};
pub use crate::preprocessor::Preprocessor;
pub use crate::source::{Source, SourceId, SourceSpan, SourceStore};
pub use crate::source_string::SourceString;
pub use crate::source_token::SourceToken;

pub mod docs;

mod cursor;
mod directive;
mod error;
mod event;
mod include_path;
mod macros;
mod origin;
mod preprocessor;
mod source;
mod source_string;
mod source_token;
