//! Preprocessor error types.
//!
//! The crate exposes two public error types:
//!
//! - [`PreprocessError`] wraps every input-derived failure the
//!   preprocessor can surface as an event (parse and later
//!   macro/include/conditional variants). It is the payload of
//!   `Event::PreprocessError`.
//! - [`ProtocolError`] describes caller mistakes when driving the
//!   state machine (calling `step` while the machine is awaiting a
//!   response, calling the wrong response method, and so on). It is
//!   returned as `Err` from `step` and from the response methods.
//!
//! Tokenization is the caller's responsibility; lexical failures are
//! surfaced by [`erl_tokenize::scan_token`] at the point the caller
//! scans the source and never reach this module.
//!
//! The internal [`ParseError`] emitted by the directive parser stays
//! `pub(crate)`; it is turned into [`PreprocessError`] by a `From`
//! conversion when it crosses the public API boundary.

use erl_tokenize::TokenKind;

use crate::source::SourceSpan;
use crate::source_string::SourceString;

// ---------------------------------------------------------------------------
// crate-internal parse error (produced by the directive parser)

/// Error emitted by the directive parser after it has committed to a
/// known directive but its structure does not match.
///
/// Carries the span of the directive's opening `-` so the outer
/// preprocessor can point at the malformed directive as a whole, a
/// short description of what was expected, and how parsing actually
/// failed (an unexpected token or an unexpected end of source).
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
        /// The repeated parameter (name span points at the later
        /// occurrence, not the original).
        name: SourceString,
    },
}

/// The concrete failure the parser hit. Used as the `actual` field
/// of [`PreprocessError::Parse`].
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

impl From<ParseError> for PreprocessError {
    fn from(e: ParseError) -> Self {
        let actual = match e.actual {
            ParseFailure::UnexpectedToken { span, kind } => {
                PreprocessParseFailure::UnexpectedToken { span, kind }
            }
            ParseFailure::UnexpectedEof => PreprocessParseFailure::UnexpectedEof,
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
    /// machine is awaiting.
    WrongResponseKind,
    /// `step` was called while the machine is awaiting a response.
    StepWhilePending,
}
