//! Preprocessor error types.
//!
//! The crate exposes two public error types:
//!
//! - [`PreprocessError`] is every input-derived failure the
//!   preprocessor can surface as an event. It is the payload of
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
// crate-internal macro-call failure (span is attached at the call site)

/// Failure found while parsing a function-like macro's argument list
/// or expanding `??Param`, before the call-site span is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MacroCallErrorKind {
    /// The argument list of a function-like call was never closed.
    UnclosedArgument,
    /// A leading empty argument appeared (`?NAME(, ...)`).
    LeadingEmptyArgument,
    /// A trailing empty argument appeared (`?NAME(..., )`).
    TrailingEmptyArgument,
    /// The token following `??` is not a parameter name.
    InvalidStringificationTarget {
        /// Span of the offending token following `??`.
        span: SourceSpan,
    },
}

impl MacroCallErrorKind {
    /// Attaches `call_site` and turns this into a public
    /// [`PreprocessError`].
    pub(crate) fn into_preprocess_error(self, call_site: SourceSpan) -> PreprocessError {
        match self {
            Self::UnclosedArgument => PreprocessError::UnclosedArgument { span: call_site },
            Self::LeadingEmptyArgument => PreprocessError::LeadingEmptyArgument { span: call_site },
            Self::TrailingEmptyArgument => {
                PreprocessError::TrailingEmptyArgument { span: call_site }
            }
            Self::InvalidStringificationTarget { span } => {
                PreprocessError::InvalidStringificationTarget {
                    call_site,
                    target: span,
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// public errors

/// An input-derived error that the preprocessor surfaced through the
/// event stream.
///
/// Every variant is one concrete failure. Highlight position is
/// [`PreprocessError::span`].
#[derive(Debug, Clone)]
pub enum PreprocessError {
    /// The directive parser committed to a known directive but saw
    /// an unexpected token.
    ParseUnexpectedToken {
        /// Span covering the directive's opening `-`.
        directive_start: SourceSpan,
        /// Human-readable description of what was expected.
        expected: String,
        /// Span of the offending token.
        span: SourceSpan,
        /// Kind of the offending token.
        kind: TokenKind,
    },
    /// The directive parser committed to a known directive but the
    /// source ended before the directive was complete.
    ParseUnexpectedEof {
        /// Span covering the directive's opening `-`.
        directive_start: SourceSpan,
        /// Human-readable description of what was expected.
        expected: String,
    },
    /// A `-define(...)` parameter list repeats the same name.
    DuplicateParameter {
        /// Span covering the whole directive.
        span: SourceSpan,
        /// The repeated parameter (name span points at the later
        /// occurrence, not the original).
        name: SourceString,
    },
    /// The macro is defined, but not for the called shape. The
    /// `defined_arities` list carries every arity currently defined
    /// for the name (constant-like is `None`, function-like is
    /// `Some(n)`).
    ArityMismatch {
        /// Span of the call site.
        span: SourceSpan,
        /// Called name.
        name: SourceString,
        /// Called arity (`None` for constant-like call, `Some(n)` for
        /// function-like call with `n` arguments).
        called_arity: Option<usize>,
        /// Arities currently defined for `name`.
        defined_arities: Vec<Option<usize>>,
    },
    /// The argument list of a function-like call was never closed
    /// before the end of source.
    UnclosedArgument {
        /// Span of the call site.
        span: SourceSpan,
    },
    /// A leading empty argument appeared (`?NAME(, ...)`). Middle
    /// empty arguments (`?NAME(A, , B)`) are valid per OTP and do not
    /// surface here.
    LeadingEmptyArgument {
        /// Span of the call site.
        span: SourceSpan,
    },
    /// A trailing empty argument appeared (`?NAME(..., )`).
    TrailingEmptyArgument {
        /// Span of the call site.
        span: SourceSpan,
    },
    /// The token following `??` is not a parameter name of the
    /// enclosing macro.
    InvalidStringificationTarget {
        /// Span of the whole macro call.
        call_site: SourceSpan,
        /// Span of the offending token following `??`.
        target: SourceSpan,
    },
    /// A macro call would recurse into itself directly or
    /// transitively.
    CircularExpansion {
        /// Span of the call site.
        span: SourceSpan,
        /// Called name.
        name: String,
        /// Called arity.
        arity: Option<usize>,
        /// The `(name, arity)` chain that closes back on the call, in
        /// call order.
        chain: Vec<(String, Option<usize>)>,
    },
    /// `-else` appeared without a matching opening `-ifdef` /
    /// `-ifndef` / `-if`.
    StrayElse {
        /// Span of the offending directive.
        span: SourceSpan,
    },
    /// `-endif` appeared without a matching opening `-ifdef` /
    /// `-ifndef` / `-if`.
    StrayEndif {
        /// Span of the offending directive.
        span: SourceSpan,
    },
    /// A second `-else` appeared inside the same conditional.
    DoubleElse {
        /// Span of the offending directive.
        span: SourceSpan,
    },
    /// The source ended while a conditional was still open.
    UnclosedConditional {
        /// Span of the opening directive.
        span: SourceSpan,
    },
    /// `-elif` appeared without a matching opening `-if`, or on top
    /// of an `-ifdef` / `-ifndef` frame (erl_pp rejects the latter
    /// even though OTP `epp` would accept it).
    StrayElif {
        /// Span of the offending directive.
        span: SourceSpan,
    },
    /// `-elif` appeared after `-else` in the same conditional.
    ElifAfterElse {
        /// Span of the offending directive.
        span: SourceSpan,
    },
}

impl PreprocessError {
    /// Source span to highlight for this failure.
    ///
    /// For [`PreprocessError::ParseUnexpectedToken`] this is the
    /// offending token. For [`PreprocessError::ParseUnexpectedEof`]
    /// it is the directive's opening `-`. For
    /// [`PreprocessError::InvalidStringificationTarget`] it is the
    /// token following `??`. Every other variant returns its
    /// primary `span`.
    pub fn span(&self) -> SourceSpan {
        match self {
            Self::ParseUnexpectedToken { span, .. } => *span,
            Self::ParseUnexpectedEof {
                directive_start, ..
            } => *directive_start,
            Self::DuplicateParameter { span, .. }
            | Self::ArityMismatch { span, .. }
            | Self::UnclosedArgument { span }
            | Self::LeadingEmptyArgument { span }
            | Self::TrailingEmptyArgument { span }
            | Self::CircularExpansion { span, .. }
            | Self::StrayElse { span }
            | Self::StrayEndif { span }
            | Self::DoubleElse { span }
            | Self::UnclosedConditional { span }
            | Self::StrayElif { span }
            | Self::ElifAfterElse { span } => *span,
            Self::InvalidStringificationTarget { target, .. } => *target,
        }
    }
}

impl From<ParseError> for PreprocessError {
    fn from(e: ParseError) -> Self {
        match e.actual {
            ParseFailure::UnexpectedToken { span, kind } => Self::ParseUnexpectedToken {
                directive_start: e.directive_start,
                expected: e.expected,
                span,
                kind,
            },
            ParseFailure::UnexpectedEof => Self::ParseUnexpectedEof {
                directive_start: e.directive_start,
                expected: e.expected,
            },
        }
    }
}

/// Caller-driven-mistake error returned from response methods and
/// from `step` when the caller uses the state machine's protocol
/// incorrectly.
///
/// The value has no variants: `step` only fails when a response is
/// pending, and `resume_*` only fails when that response is not
/// expected. The last [`Event`](crate::Event) already names which wait (if
/// any) is in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError;

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("preprocessor protocol violation")
    }
}

impl std::error::Error for ProtocolError {}
