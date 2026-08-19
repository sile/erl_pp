//! Events produced by the preprocessor state machine.
//!
//! Each call to [`crate::Preprocessor::step`] advances the machine by
//! one transition and returns one [`Event`] describing what happened.
//! When the event leaves the machine awaiting a response, the caller
//! invokes the matching response method before calling `step` again.
//! [`crate::Preprocessor::status`] reports which response (if any) the
//! machine is currently awaiting.

use crate::directive::Directive;
use crate::error::PreprocessError;
use crate::preprocessed_token::PreprocessedToken;

/// One-shot output of [`crate::Preprocessor::step`].
///
/// Each `step` call advances the state machine and returns exactly one
/// event. Some variants leave the machine in an awaiting state; while
/// the machine is awaiting a response, the caller must invoke the
/// corresponding response method before `step` will return another
/// event. Inspect [`crate::Preprocessor::status`] to see which
/// response is expected.
#[derive(Debug, Clone)]
pub enum Event {
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
    /// not streamed as [`Event::Token`]. Downstream effects (macro
    /// table updates, include resolution, conditional selection,
    /// diagnostic emission) are the caller's or later work's
    /// responsibility.
    Directive(Directive),

    /// The preprocessor is awaiting an include resolution from the
    /// caller.
    ///
    /// Payload struct name is preserved for now; details are filled
    /// in by later work on include resolution.
    AwaitingInclude(IncludeRequest),

    /// The preprocessor is awaiting a conditional-branch decision
    /// from the caller.
    ///
    /// Payload struct name is preserved for now; details are filled
    /// in by later work on conditional branching.
    AwaitingConditional(ConditionalRequest),

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
    /// Subsequent `step` calls return `Complete` without side effects.
    Complete,
}

/// Data of an [`Event::AwaitingInclude`].
///
/// Payload details (directive kind, decoded path, span, origin, etc.)
/// are filled in by later work on include resolution.
#[derive(Debug, Clone)]
pub struct IncludeRequest {}

/// Data of an [`Event::AwaitingConditional`].
///
/// Payload details are filled in by later work on conditional
/// branching.
#[derive(Debug, Clone)]
pub struct ConditionalRequest {}

/// Data of an [`Event::BranchBoundary`].
///
/// Payload details are filled in by later work on conditional
/// branching.
#[derive(Debug, Clone)]
pub struct BranchBoundary {}

/// Data of an [`Event::Diagnostic`].
///
/// Payload details are filled in by later work on diagnostic
/// directives.
#[derive(Debug, Clone)]
pub struct Diagnostic {}
