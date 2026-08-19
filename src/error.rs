//! Preprocessor error types.
//!
//! The crate exposes two public error types:
//!
//! - [`PreprocessError`] wraps every input-derived failure the
//!   preprocessor can surface as an event (lexical, parse, and later
//!   macro/include/conditional variants). It is the payload of
//!   `Event::PreprocessError`.
//! - [`ProtocolError`] describes caller mistakes when driving the
//!   state machine (calling `step` while the machine is awaiting a
//!   response, calling the wrong response method, and so on). It is
//!   returned as `Err` from `step` and from the response methods.
//!
//! The internal [`LexicalError`] emitted by the source cursor and the
//! internal [`ParseError`] emitted by the directive parser stay
//! `pub(crate)`; they are turned into [`PreprocessError`] by `From`
//! conversions when they cross the public API boundary.

use erl_tokenize::{ErrorKind, Position, TokenKind};

use crate::source::SourceSpan;

// ---------------------------------------------------------------------------
// crate-internal errors (produced by the cursor and the directive parser)

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

// ---------------------------------------------------------------------------
// public errors

/// An input-derived error that the preprocessor surfaced through the
/// event stream.
///
/// Every variant carries the source span at which the failure was
/// located, alongside variant-specific details.
///
/// Future work adds more variants (macro expansion, include reject,
/// conditional syntax) as the corresponding parts of the preprocessor
/// come online.
#[derive(Debug, Clone)]
pub enum PreprocessError {
    /// The tokenizer failed to scan a token.
    Lexical {
        /// Span at which the failing scan started.
        span: SourceSpan,
        /// Underlying tokenizer error kind.
        error_kind: ErrorKind,
        /// Position the cursor can be resumed at.
        resume_position: Position,
    },
    /// The directive parser committed to a known directive but its
    /// structure did not match.
    Parse {
        /// Span covering the directive's opening `-`.
        directive_start: SourceSpan,
        /// Human-readable description of what was expected.
        expected: String,
        /// What the parser actually saw at the point of failure.
        actual: PreprocessParseFailure,
    },
    /// A `-define(...)` directive's structure is rejected by the macro
    /// table (duplicate parameter name, etc.).
    MacroDefinition {
        /// Span covering the whole directive.
        span: SourceSpan,
        /// What made the definition invalid.
        kind: MacroDefinitionErrorKind,
    },
}

/// Reasons a `-define(...)` directive is rejected by the macro table.
#[derive(Debug, Clone)]
pub enum MacroDefinitionErrorKind {
    /// The parameter list repeats the same name.
    DuplicateParameter {
        /// The repeated parameter name.
        name: String,
        /// Span of the offending later occurrence.
        span: SourceSpan,
    },
}

/// The concrete failure the parser hit. Used as the `actual` field
/// of [`PreprocessError::Parse`].
///
/// Lexical failures that surface inside directive parsing are routed
/// to [`PreprocessError::Lexical`] by the state machine, so this enum
/// only covers structural mismatches.
#[derive(Debug, Clone)]
pub enum PreprocessParseFailure {
    /// An unexpected token was found.
    UnexpectedToken {
        /// Span of the offending token.
        span: SourceSpan,
        /// Kind of the offending token.
        kind: TokenKind,
    },
    /// The source ended before the directive was complete.
    UnexpectedEof,
}

impl From<LexicalError> for PreprocessError {
    fn from(e: LexicalError) -> Self {
        PreprocessError::Lexical {
            span: e.span,
            error_kind: e.kind,
            resume_position: e.resume_position,
        }
    }
}

impl From<ParseError> for PreprocessError {
    fn from(e: ParseError) -> Self {
        let actual = match e.actual {
            ParseFailure::UnexpectedToken { span, kind } => {
                PreprocessParseFailure::UnexpectedToken { span, kind }
            }
            ParseFailure::UnexpectedEof => PreprocessParseFailure::UnexpectedEof,
            ParseFailure::Lexical(_) => unreachable!(
                "lexical failures inside directive parsing are routed to \
                 PreprocessError::Lexical by the state machine before conversion",
            ),
        };
        PreprocessError::Parse {
            directive_start: e.directive_start,
            expected: e.expected,
            actual,
        }
    }
}

/// Caller-driven-mistake error returned from response methods and from
/// `step` when the caller uses the state machine's protocol
/// incorrectly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    /// A response method was called while the machine was not
    /// awaiting any response.
    UnexpectedResponse,
    /// A response method was called that does not match what the
    /// machine is awaiting (e.g. `resume_lexical` while awaiting an
    /// include resolution).
    WrongResponseKind,
    /// `step` was called while the machine is awaiting a response.
    StepWhilePending,
}
