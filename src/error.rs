//! Placeholder for preprocessor error types.
//!
//! The full preprocessor error model is added by later work alongside
//! the state machine. This module currently defines the internal
//! errors emitted by the source cursor and the directive parser.

use erl_tokenize::{ErrorKind, Position, TokenKind};

use crate::source::SourceSpan;

/// Lexical error emitted by the source cursor when
/// `erl_tokenize::scan_token` fails.
///
/// Carries the source span at which the failing scan started, the
/// error kind, and the resume position suggested by
/// [`erl_tokenize::Error`]. After emitting this error the cursor is
/// kept in a pending-resume state; the outer state machine chooses to
/// stop or to resume the cursor at the suggested (or any later)
/// position.
#[allow(dead_code, reason = "consumed by preprocessor internals to be added")]
#[derive(Debug, Clone)]
pub(crate) struct LexicalError {
    /// Span at which the failing scan started. `span.end` matches
    /// [`resume_position`](Self::resume_position).
    pub span: SourceSpan,
    /// Kind of the tokenizer error.
    pub kind: ErrorKind,
    /// Position the cursor can be resumed at without looping. This is
    /// `erl_tokenize::Error`'s `resume_position` unchanged; the
    /// tokenizer guarantees it is strictly after the failing scan.
    pub resume_position: Position,
}

/// Error emitted by the directive parser after it has committed to a
/// known directive but its structure does not match.
///
/// Carries the span of the directive's opening `-` so the outer
/// preprocessor can point at the malformed directive as a whole, a
/// short description of what was expected, and how parsing actually
/// failed (an unexpected token, an unexpected end of source, or a
/// lexical error surfaced by the cursor).
#[allow(dead_code, reason = "consumed by preprocessor internals to be added")]
#[derive(Debug, Clone)]
pub(crate) struct ParseError {
    /// Span covering the directive's opening `-`.
    pub directive_start: SourceSpan,
    /// Human-readable description of what the parser was expecting.
    pub expected: String,
    /// What the parser actually saw at the point of failure.
    pub actual: ParseFailure,
}

/// The concrete failure that caused a [`ParseError`].
#[allow(dead_code, reason = "consumed by preprocessor internals to be added")]
#[derive(Debug, Clone)]
pub(crate) enum ParseFailure {
    /// An unexpected token was found.
    UnexpectedToken {
        /// Span of the offending token.
        span: SourceSpan,
        /// Kind of the offending token.
        kind: TokenKind,
    },
    /// The source ended before the directive was complete.
    UnexpectedEof,
    /// The cursor surfaced a lexical error while parsing the directive.
    ///
    /// Boxed to keep the enclosing [`ParseError`] small.
    Lexical(Box<LexicalError>),
}
