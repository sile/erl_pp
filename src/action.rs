//! Actions produced by the preprocessor state machine.
//!
//! [`crate::Preprocessor::next_action`] returns one [`Action`] at a
//! time. Callers consume the action and — when the action leaves the
//! machine awaiting a response — invoke the matching response method
//! before calling `next_action` again. [`crate::Preprocessor::status`]
//! reports which response (if any) the machine is currently awaiting.

use crate::directive::Directive;
use crate::error::PreprocessError;
use crate::preprocessed_token::PreprocessedToken;

/// One-shot output of [`crate::Preprocessor::next_action`].
///
/// The state machine advances by exactly one action per call. Some
/// variants leave the machine in an awaiting state; while the machine
/// is awaiting a response, the caller must invoke the corresponding
/// response method before `next_action` will return another action.
/// Inspect [`crate::Preprocessor::status`] to see which response is
/// expected.
#[derive(Debug, Clone)]
pub enum Action {
    /// A token was scanned from the input.
    ///
    /// The bundled [`PreprocessedToken`] carries the token itself, the
    /// [`crate::Source`] it came from, its [`crate::SourceSpan`], and
    /// its [`crate::Origin`]. Callers that need the whole stream keep
    /// their own accumulator; the preprocessor does not retain scanned
    /// tokens.
    Token(PreprocessedToken),

    /// A preprocessor directive was observed.
    ///
    /// The directive tokens are consumed from the source; they are
    /// not streamed as [`Action::Token`]. Downstream effects (macro
    /// table updates, include resolution, conditional selection,
    /// diagnostic emission) are the caller's or later work's
    /// responsibility.
    Directive(Directive),

    /// The preprocessor needs the caller to resolve an include.
    ///
    /// Payload is filled in by later work on include resolution.
    IncludeRequest(IncludeRequest),

    /// The preprocessor needs the caller to select a conditional
    /// branch.
    ///
    /// Payload is filled in by later work on conditional branching.
    ConditionalRequest(ConditionalRequest),

    /// The preprocessor is crossing a conditional branch boundary
    /// (`-else` / `-endif`).
    ///
    /// Payload is filled in by later work on conditional branching.
    BranchBoundary(BranchBoundary),

    /// A `-error` / `-warning` diagnostic reached the caller.
    ///
    /// Payload is filled in by later work on diagnostic directives.
    Diagnostic(Diagnostic),

    /// An input-derived error surfaced.
    ///
    /// See [`PreprocessError`] for the concrete failure kinds. If the
    /// failure was a lexical error, the caller may respond with
    /// [`crate::Preprocessor::resume_lexical`] to continue scanning at
    /// the `resume_position` carried on the error.
    PreprocessError(PreprocessError),

    /// The whole input has been processed.
    ///
    /// Subsequent `next_action` calls return `Complete` without side
    /// effects.
    Complete,
}

/// Data of an [`Action::IncludeRequest`].
///
/// Payload details (directive kind, decoded path, span, origin, etc.)
/// are filled in by later work on include resolution.
#[derive(Debug, Clone)]
pub struct IncludeRequest {}

/// Data of an [`Action::ConditionalRequest`].
///
/// Payload details are filled in by later work on conditional
/// branching.
#[derive(Debug, Clone)]
pub struct ConditionalRequest {}

/// Data of an [`Action::BranchBoundary`].
///
/// Payload details are filled in by later work on conditional
/// branching.
#[derive(Debug, Clone)]
pub struct BranchBoundary {}

/// Data of an [`Action::Diagnostic`].
///
/// Payload details are filled in by later work on diagnostic
/// directives.
#[derive(Debug, Clone)]
pub struct Diagnostic {}
