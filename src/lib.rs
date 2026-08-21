//! Erlang source code preprocessor.
//!
//! The crate is built around a Sans-I/O state machine. The caller feeds
//! a sequence of [`Source`]s into [`Preprocessor`] and drives it by calling
//! [`Preprocessor::step`] in a loop; each call returns one [`Event`]
//! describing the next transition (a scanned token, a macro
//! definition or undef, a caller-driven include / conditional /
//! macro-expansion response, a diagnostic, an error, or completion).
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
//! let mut tokens = Vec::new();
//! let mut position = erl_tokenize::Position::new();
//! while let Some(t) = erl_tokenize::scan_token(text, position)? {
//!     position = t.end();
//!     tokens.push(t);
//! }
//! let source = erl_pp::Source::new("example.erl", text.to_string(), tokens);
//! let mut pp = erl_pp::Preprocessor::new([source]);
//!
//! let mut lexical = Vec::<String>::new();
//! loop {
//!     match pp.step().expect("no protocol error on trivial input") {
//!         erl_pp::Event::Token(t) if t.token().kind().is_lexical() => {
//!             lexical.push(t.text().to_owned());
//!         }
//!         erl_pp::Event::Token(_) => {} // hidden (whitespace / comments)
//!         erl_pp::Event::Complete => break,
//!         other => unreachable!("unexpected event: {other:?}"),
//!     }
//! }
//! assert_eq!(lexical, ["atom", ",", "foo", ",", "bar", "."]);
//! # Ok(())
//! # }
//! ```
//!
//! # References
//!
//! - [Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html)
#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub use crate::directive::Param;
pub use crate::error::{PreprocessError, ProtocolError};
pub use crate::event::{
    Branch, BranchBoundary, Conditional, DefinedConditional, Diagnostic, Event,
    ExpressionConditional, IncludeDirective, MacroCall, Severity, UndefinedMacro,
};
pub use crate::include_path::{OpenIncludeError, open_include};
pub use crate::macros::{MacroDefinition, MacroTable};
pub use crate::origin::{IncludeKind, Origin, SourceInfoMacroKind};
pub use crate::preprocessor::{Preprocessor, Status};
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
