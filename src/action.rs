//! Actions produced by the preprocessor state machine.
//!
//! `next_action` on the preprocessor returns one [`Action`] at a
//! time. Callers consume the action, look up any related token in the
//! [`crate::Preprocessed`] container, and — for actions that carry a
//! [`RequestId`] — respond through one of the preprocessor's response
//! methods before calling `next_action` again.
//!
//! Variants that require future work carry only their [`RequestId`]
//! for now. Payload details are filled in by later work on include
//! resolution, conditional branching, and diagnostic directives.

use std::num::NonZeroU32;

use crate::directive::Directive;
use crate::error::PreprocessError;

/// One-shot output of [`crate::Preprocessor::next_action`].
///
/// The state machine advances by exactly one action per call. Some
/// variants carry a [`RequestId`] that identifies a pending request;
/// while such a request is pending, the caller must invoke the
/// corresponding response method before `next_action` will return
/// another action.
#[derive(Debug, Clone)]
pub enum Action {
    /// A token was appended to the output container.
    ///
    /// The token itself, its span, and its origin are available via
    /// [`crate::Preprocessed`] indexed by `index`.
    Token {
        /// Index into [`crate::Preprocessed`].
        index: usize,
    },

    /// A preprocessor directive was observed.
    ///
    /// The directive tokens are consumed from the source; they are
    /// not appended to [`crate::Preprocessed`]. Downstream effects
    /// (macro table updates, include resolution, conditional
    /// selection, diagnostic emission) are the caller's or later
    /// work's responsibility.
    Directive(Directive),

    /// The preprocessor needs the caller to resolve an include.
    ///
    /// Payload is filled in by later work on include resolution; this
    /// variant currently carries only the [`RequestId`] needed for the
    /// response protocol.
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
    /// failure was a lexical error, the caller may inspect
    /// [`crate::Preprocessor::pending_request`] and respond with
    /// [`crate::Preprocessor::resume_lexical`] to continue scanning.
    PreprocessError(PreprocessError),

    /// The whole input has been processed.
    ///
    /// Subsequent `next_action` calls return `Complete` without side
    /// effects.
    Complete,
}

/// Identifier for a pending request that the state machine has raised.
///
/// Values are only meaningful inside the preprocessor that issued
/// them; do not compare identifiers from different preprocessors.
//
// Internally represented as `NonZeroU32` so that `Option<RequestId>`
// fits in four bytes and 0 is unavailable as a valid handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(NonZeroU32);

impl RequestId {
    #[allow(dead_code, reason = "constructed by Preprocessor internals")]
    pub(crate) fn from_index(index: u32) -> Self {
        let one_based = index
            .checked_add(1)
            .and_then(NonZeroU32::new)
            .expect("RequestId counter overflowed");
        Self(one_based)
    }
}

/// Data of an [`Action::IncludeRequest`].
///
/// Only the [`RequestId`] is populated in this release; the rest of
/// the payload (directive kind, decoded path, span, origin, etc.) is
/// added by later work on include resolution.
#[derive(Debug, Clone)]
pub struct IncludeRequest {
    /// Identifier the caller must echo back when responding.
    pub request_id: RequestId,
}

/// Data of an [`Action::ConditionalRequest`].
///
/// Payload details are added by later work on conditional branching.
#[derive(Debug, Clone)]
pub struct ConditionalRequest {
    /// Identifier the caller must echo back when responding.
    pub request_id: RequestId,
}

/// Data of an [`Action::BranchBoundary`].
///
/// Payload details are added by later work on conditional branching.
#[derive(Debug, Clone)]
pub struct BranchBoundary {}

/// Data of an [`Action::Diagnostic`].
///
/// Payload details are added by later work on diagnostic directives.
#[derive(Debug, Clone)]
pub struct Diagnostic {}
