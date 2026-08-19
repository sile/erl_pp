//! Erlang source code preprocessor.
//!
//! This crate is being rebuilt around a Sans-I/O state machine. The
//! public API in this release exposes the shared data model that later
//! work (state machine, directive parser, macro expansion, include
//! handling, and so on) builds on: [`Source`], [`SourceStore`],
//! [`SourceId`], [`SourceSpan`], [`PreprocessedToken`], and [`Origin`].
//!
//! Runnable examples of the full preprocessor loop will follow in
//! later releases.
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
    MacroDefinitionErrorKind, PreprocessError, PreprocessParseFailure, ProtocolError,
};
pub use crate::event::{BranchBoundary, ConditionalRequest, Diagnostic, Event, IncludeRequest};
pub use crate::macros::{MacroDefinition, MacroKey, MacroTable};
pub use crate::origin::Origin;
pub use crate::preprocessed_token::PreprocessedToken;
pub use crate::preprocessor::{Preprocessor, Status};
pub use crate::source::{Source, SourceId, SourceSpan, SourceStore};

mod cursor;
mod directive;
mod error;
mod event;
mod macros;
mod origin;
mod preprocessed_token;
mod preprocessor;
mod source;
