//! Erlang source code preprocessor.
//!
//! The crate is built around a Sans-I/O state machine. The caller feeds
//! a [`Source`] into [`Preprocessor`] and drives it by calling
//! [`Preprocessor::step`] in a loop; each call returns one [`Event`]
//! describing the next transition (a scanned token, a directive, a
//! caller-driven include / conditional / macro-expansion response, a
//! diagnostic, an error, or completion).
//!
//! # Minimal event loop
//!
//! Trivial input with no directives, includes, conditionals, macros, or
//! `-error` / `-warning`. All `Awaiting*` / `Directive` / `Diagnostic` /
//! `PreprocessError` branches are unreachable and fall into the
//! `unreachable!` arm.
//!
//! ```
//! use erl_pp::{Event, Preprocessor, Source};
//! use erl_tokenize::{Position, Token, scan_token};
//!
//! let text = "atom, foo, bar.";
//! let mut tokens = Vec::new();
//! let mut position = Position::new();
//! while let Some(t) = scan_token(text, position).unwrap() {
//!     position = t.end();
//!     tokens.push(t);
//! }
//! let source = Source::new("example.erl", text.to_string(), tokens);
//! let mut pp = Preprocessor::new(source);
//!
//! let mut lexical = Vec::<String>::new();
//! loop {
//!     match pp.step().expect("no protocol error on trivial input") {
//!         Event::Token(t) if t.token().kind().is_lexical() => {
//!             lexical.push(t.text().to_owned());
//!         }
//!         Event::Token(_) => {} // hidden (whitespace / comments)
//!         Event::Complete => break,
//!         other => unreachable!("unexpected event: {other:?}"),
//!     }
//! }
//! assert_eq!(lexical, ["atom", ",", "foo", ",", "bar", "."]);
//! ```
//!
//! # References
//!
//! - [Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html)
#![warn(missing_docs)]
#![forbid(unsafe_code)]

pub use crate::directive::{
    DefineDirective, Directive, ElseDirective, EndifDirective, ErrorDirective, IfdefDirective,
    IfndefDirective, IncludeDirective, IncludeLibDirective, Param, UndefDirective,
    WarningDirective,
};
pub use crate::error::{
    ConditionalErrorKind, MacroCallErrorKind, MacroDefinitionErrorKind, PreprocessError,
    PreprocessParseFailure, ProtocolError,
};
pub use crate::event::{
    Branch, BranchBoundary, BranchBoundaryKind, ConditionalKind, ConditionalRequest, Diagnostic,
    Event, IncludeKind, IncludeRequest, MacroExpansionRequest, Severity,
};
pub use crate::include_path::{OpenIncludeError, OpenedInclude, open_include};
pub use crate::macros::{MacroDefinition, MacroKey, MacroTable};
pub use crate::origin::{Origin, SourceInfoMacroKind};
pub use crate::preprocessed_token::PreprocessedToken;
pub use crate::preprocessor::{Preprocessor, Status};
pub use crate::source::{Source, SourceId, SourceSpan, SourceStore};
pub use crate::source_string::SourceString;

pub mod docs;

mod cursor;
mod directive;
mod error;
mod event;
mod include_path;
mod macros;
mod origin;
mod preprocessed_token;
mod preprocessor;
mod source;
mod source_string;
