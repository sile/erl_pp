//! Erlang source code preprocessor.
//!
//! This crate is being rebuilt around a Sans-I/O state machine. The
//! public API in this release exposes the shared data model that later
//! work (state machine, directive parser, macro expansion, include
//! handling, and so on) builds on: [`Source`], [`SourceStore`],
//! [`SourceId`], [`SourceSpan`], [`Preprocessed`], and [`Origin`].
//!
//! Runnable examples of the full preprocessor loop will follow in
//! later releases.
//!
//! # References
//!
//! - [Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html)
#![warn(missing_docs)]

pub use crate::directive::{
    DefineDirective, Directive, ElseDirective, EndifDirective, ErrorDirective, IfdefDirective,
    IfndefDirective, IncludeDirective, IncludeLibDirective, Param, UndefDirective,
    WarningDirective,
};
pub use crate::origin::Origin;
pub use crate::preprocessed::Preprocessed;
pub use crate::source::{Source, SourceId, SourceSpan, SourceStore};

mod cursor;
mod directive;
mod error;
mod origin;
mod preprocessed;
mod source;
