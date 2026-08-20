//! Events produced by the preprocessor state machine.
//!
//! Each call to [`crate::Preprocessor::step`] advances the machine by
//! one transition and returns one [`Event`] describing what happened.
//! When the event leaves the machine awaiting a response, the caller
//! invokes the matching response method before calling `step` again.
//! [`crate::Preprocessor::status`] reports which response (if any) the
//! machine is currently awaiting.

use std::sync::Arc;

use crate::directive::Directive;
use crate::error::PreprocessError;
use crate::origin::Origin;
use crate::preprocessed_token::PreprocessedToken;
use crate::source::SourceSpan;
use crate::source_string::SourceString;

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
    /// not streamed as [`Event::Token`].
    ///
    /// State effects that the preprocessor owns internally
    /// (`-define` / `-undef` updates to the macro table) are applied
    /// **before** the event is emitted, so a caller matching
    /// `Event::Directive` observes the post-update macro table via
    /// [`crate::Preprocessor::macros`]. Effects that require a
    /// response from the caller (include resolution, conditional
    /// selection, diagnostic emission) still return through their own
    /// dedicated events in later work.
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

    /// The preprocessor is awaiting a caller-driven macro expansion.
    ///
    /// Fires for every `?NAME` (or `?NAME(...)`) that is neither
    /// `?FILE` / `?LINE` nor present in the current macro table. The
    /// caller inspects the request and responds via
    /// [`crate::Preprocessor::resume_macro_expansion`] with a
    /// [`crate::Source`] whose token stream is spliced in as the
    /// expansion result. An empty [`crate::Source`] effectively
    /// skips the call.
    AwaitingMacroExpansion(MacroExpansionRequest),

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
    /// See [`PreprocessError`] for the concrete failure kinds.
    PreprocessError(PreprocessError),

    /// The whole input has been processed.
    ///
    /// Subsequent `step` calls return `Complete` without side effects.
    Complete,
}

/// Distinguishes `-include` from `-include_lib` in an
/// [`IncludeRequest`] and in [`Origin::Include`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncludeKind {
    /// `-include("path").`
    Include,
    /// `-include_lib("app/include/hdr.hrl").`
    IncludeLib,
}

/// Data of an [`Event::AwaitingInclude`].
///
/// Describes the include the caller must resolve. The preprocessor
/// does no path lookup, no environment expansion, and no filesystem
/// access — the caller uses `path`, `kind`, and (via `SourceStore`)
/// the source referenced by `directive_span.source_id` to resolve the
/// include, then hands the resulting [`crate::Source`] back through
/// [`crate::Preprocessor::resume_include`].
#[derive(Debug, Clone)]
pub struct IncludeRequest {
    /// Whether this is `-include` or `-include_lib`.
    pub kind: IncludeKind,
    /// Decoded, concatenated contents of the include's string
    /// literals (matches `IncludeDirective::path` /
    /// `IncludeLibDirective::path`).
    pub path: SourceString,
    /// Span of the whole directive from the leading `-` through the
    /// terminating `.`. The include's parent source is
    /// `directive_span.source_id`.
    pub directive_span: SourceSpan,
    /// Origin of the directive itself. Becomes the `parent` of the
    /// [`Origin::Include`] attached to every token emitted from the
    /// resolved source.
    pub parent_origin: Arc<Origin>,
}

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

/// Data of an [`Event::AwaitingMacroExpansion`].
///
/// Describes the macro call the caller must resolve. `arity` is
/// `None` for a bare `?NAME` and `Some(n)` for `?NAME(a1, ..., an)`;
/// when `arity` is `Some(n)`, `arguments` holds the `n` argument
/// token streams (each may include hidden tokens like whitespace and
/// comments). An arity-0 call `?NAME()` is `arity: Some(0)` with an
/// empty `arguments`, distinct from a constant-like `?NAME` where
/// `arity` is `None` and `arguments` is also empty.
#[derive(Debug, Clone)]
pub struct MacroExpansionRequest {
    /// Decoded name of the macro (the token following `?`).
    pub name: SourceString,
    /// Arity of the call: `None` for constant-like `?NAME`, `Some(n)`
    /// for a function-like `?NAME(a1, ..., an)`.
    pub arity: Option<usize>,
    /// Span covering the whole call from the leading `?` through the
    /// closing `)` (or through the name token for constant-like
    /// calls).
    pub call_site: SourceSpan,
    /// Per-argument token streams. Empty when `arity` is `None` or
    /// `Some(0)`; otherwise has exactly `n` entries.
    pub arguments: Vec<Vec<PreprocessedToken>>,
}
