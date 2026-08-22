//! Events produced by the preprocessor state machine.
//!
//! Each call to [`Preprocessor::step`](crate::Preprocessor::step) advances the machine by
//! one transition and returns one [`Event`] describing what happened.
//! When the event leaves the machine awaiting a response, the caller
//! invokes the matching response method before calling `step` again.

use std::sync::Arc;

use crate::error::PreprocessError;
use crate::macros::MacroDefinition;
use crate::origin::{IncludeKind, Origin};
use crate::source::SourceSpan;
use crate::source_string::SourceString;
use crate::source_token::SourceToken;

/// One-shot output of [`Preprocessor::step`](crate::Preprocessor::step).
///
/// Each `step` call advances the state machine and returns exactly one
/// event. Some variants leave the machine in an awaiting state; while
/// the machine is awaiting a response, the caller must invoke the
/// corresponding response method before `step` will return another
/// event. The event itself names which response is expected.
#[derive(Debug, Clone)]
pub enum Event {
    /// A lexical token was scanned from the input.
    ///
    /// Whitespace and comments are not emitted; they remain on the
    /// [`Source`](crate::Source) the caller already supplied. The bundled
    /// [`SourceToken`] carries the token itself, the [`Source`](crate::Source)
    /// it indexes, its [`SourceSpan`](crate::SourceSpan), and its
    /// [`Origin`](crate::Origin). Callers that need the whole stream keep
    /// their own accumulator.
    Token(SourceToken),

    /// A `-define(...)` was applied to the macro table.
    ///
    /// The table update is visible through
    /// [`Preprocessor::macros`](crate::Preprocessor::macros) before this event is
    /// returned. The payload is the definition that was inserted.
    MacroDefined(MacroDefinition),

    /// A `-undef(...)` was applied to the macro table.
    ///
    /// Every entry matching the name is already gone when this
    /// event is returned, including the case where the name was
    /// not defined. See [`UndefinedMacro`].
    MacroUndefined(UndefinedMacro),

    /// The preprocessor is awaiting an include resolution from the
    /// caller.
    ///
    /// Payload is [`IncludeDirective`]. Resume with
    /// [`Preprocessor::resume_include`](crate::Preprocessor::resume_include).
    AwaitingInclude(IncludeDirective),

    /// The preprocessor is awaiting a conditional-branch decision
    /// from the caller.
    ///
    /// `-ifdef` / `-ifndef` and `-if` / `-elif` share this event
    /// and [`Preprocessor::resume_conditional`](crate::Preprocessor::resume_conditional), but their
    /// payloads differ: see [`Conditional`].
    AwaitingConditional(Conditional),

    /// The preprocessor is awaiting a caller-driven macro expansion.
    ///
    /// Fires for every `?NAME` (or `?NAME(...)`) that is neither
    /// `?FILE` / `?LINE` nor present in the current macro table. The
    /// caller inspects the [`MacroCall`] and resumes via
    /// [`Preprocessor::resume_macro_expansion`](crate::Preprocessor::resume_macro_expansion) with a
    /// [`Source`](crate::Source) whose token stream is spliced in as the
    /// expansion result.
    ///
    /// An empty [`Source`](crate::Source) deletes the call from the
    /// stream. That is not OTP epp's undef error, and it is not a
    /// [`PreprocessError`]. If the caller uses emptiness for an
    /// unknown macro, a downstream parser may see a hole and report a
    /// grammar error. The diagnostic belongs to the caller. See
    /// [`crate::docs::otp_differences`].
    AwaitingMacroExpansion(MacroCall),

    /// The preprocessor is crossing a conditional branch boundary
    /// (`-else` / `-endif`).
    ///
    /// Observation only; the machine does not wait. See
    /// [`BranchBoundary`].
    BranchBoundary(BranchBoundary),

    /// A `-error` / `-warning` diagnostic reached the caller.
    ///
    /// The machine does not wait. Abort, record, or ignore. See
    /// [`Diagnostic`].
    Diagnostic(Diagnostic),

    /// An input-derived error surfaced.
    ///
    /// The machine stays scanning; continue [`Preprocessor::step`](crate::Preprocessor::step)
    /// or drop the preprocessor. See [`PreprocessError`] for the
    /// concrete failure kinds.
    PreprocessError(PreprocessError),

    /// The whole input has been processed.
    ///
    /// Subsequent `step` calls return `Complete` without side effects.
    Complete,
}

/// Data of an [`Event::MacroUndefined`].
///
/// Names the macro that `-undef` removed. The preprocessor has
/// already dropped every arity of `name` from the table; this
/// event is the observation of that directive, including when
/// `name` was not defined.
#[derive(Debug, Clone)]
pub struct UndefinedMacro {
    /// Decoded name passed to `-undef`.
    pub name: SourceString,
    /// Span of the whole `-undef(...)` directive.
    pub directive_span: SourceSpan,
    /// Origin at the directive's site.
    pub parent_origin: Arc<Origin>,
}

/// Data of an [`Event::AwaitingInclude`].
///
/// Describes the include the caller must resolve. The preprocessor
/// does no path lookup, no environment expansion, and no filesystem
/// access — the caller uses `path`, `kind`, and (via `SourceStore`)
/// the source referenced by `directive_span.source_id` to resolve the
/// include, then hands the resulting [`Source`](crate::Source) back through
/// [`Preprocessor::resume_include`](crate::Preprocessor::resume_include).
#[derive(Debug, Clone)]
pub struct IncludeDirective {
    /// Whether this is `-include` or `-include_lib`.
    pub kind: IncludeKind,
    /// Decoded, concatenated contents of the include's string
    /// literals. Environment-variable expansion (`$FOO`),
    /// relative-path resolution, and filesystem lookup are the
    /// caller's job.
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

/// Which side of a conditional the caller wants to process. Passed to
/// [`Preprocessor::resume_conditional`](crate::Preprocessor::resume_conditional).
///
/// For `-ifdef` / `-ifndef`, `Then` is the tokens between the opening
/// directive and `-else` (or `-endif` when there is no `-else`), and
/// `Else` is the tokens between `-else` and `-endif`.
///
/// For `-if` / `-elif`, `Then` means take this branch of the chain and
/// `Else` means skip it and wait for a later `-elif` / `-else` /
/// `-endif`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Branch {
    /// Take the current branch.
    Then,
    /// Skip the current branch.
    Else,
}

/// Data of an [`Event::AwaitingConditional`].
///
/// `-ifdef` / `-ifndef` and `-if` / `-elif` both wait for
/// [`Preprocessor::resume_conditional`](crate::Preprocessor::resume_conditional), but the information
/// the caller needs is different, so the variants carry distinct
/// payloads instead of sharing optional fields.
#[derive(Debug, Clone)]
pub enum Conditional {
    /// `-ifdef(NAME).`
    Ifdef(DefinedConditional),
    /// `-ifndef(NAME).`
    Ifndef(DefinedConditional),
    /// `-if(Expression).` Expression evaluation is the caller's
    /// responsibility.
    If(ExpressionConditional),
    /// `-elif(Expression).` Expression evaluation is the caller's
    /// responsibility.
    Elif(ExpressionConditional),
}

/// Payload of [`Conditional::Ifdef`] and
/// [`Conditional::Ifndef`].
#[derive(Debug, Clone)]
pub struct DefinedConditional {
    /// Decoded name of the target macro.
    pub name: SourceString,
    /// [`MacroTable::is_defined`](crate::MacroTable::is_defined) at the point of the directive.
    pub defined: bool,
    /// The branch OTP `epp` would take given the directive and
    /// `defined`. A defined `-ifdef` prefers [`Branch::Then`];
    /// `-ifndef` prefers the opposite. Cached so callers do not have
    /// to reproduce the mapping. The caller may still pick either
    /// side.
    pub recommended: Branch,
    /// Span of the whole directive.
    pub directive_span: SourceSpan,
    /// Origin at the directive's site.
    pub parent_origin: Arc<Origin>,
}

/// Payload of [`Conditional::If`] and
/// [`Conditional::Elif`].
#[derive(Debug, Clone)]
pub struct ExpressionConditional {
    /// Macro-expanded expression tokens. Evaluating them is the
    /// caller's responsibility.
    pub condition_tokens: Vec<SourceToken>,
    /// Span of the whole directive.
    pub directive_span: SourceSpan,
    /// Origin at the directive's site.
    pub parent_origin: Arc<Origin>,
}

/// Data of an [`Event::BranchBoundary`].
///
/// Emitted when the scanner crosses `-else` or `-endif` of the
/// current conditional, both from the active branch and from an
/// inactive skip. The `directive_span` and the caller's own branch
/// stack identify which conditional this boundary closes.
#[derive(Debug, Clone)]
pub enum BranchBoundary {
    /// The scanner crossed a `-else`.
    Else {
        /// Span of the `-else` directive.
        directive_span: SourceSpan,
        /// Origin at the directive's site.
        parent_origin: Arc<Origin>,
    },
    /// The scanner crossed a `-endif`.
    Endif {
        /// Span of the `-endif` directive.
        directive_span: SourceSpan,
        /// Origin at the directive's site.
        parent_origin: Arc<Origin>,
    },
}

impl BranchBoundary {
    /// Span of the boundary directive.
    pub fn directive_span(&self) -> SourceSpan {
        match self {
            Self::Else { directive_span, .. } | Self::Endif { directive_span, .. } => {
                *directive_span
            }
        }
    }

    /// Origin at the boundary directive's site.
    pub fn parent_origin(&self) -> &Arc<Origin> {
        match self {
            Self::Else { parent_origin, .. } | Self::Endif { parent_origin, .. } => parent_origin,
        }
    }
}

/// Distinguishes `-error` from `-warning` in a [`Diagnostic`] event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    /// `-error(...)`
    Error,
    /// `-warning(...)`
    Warning,
}

/// Data of an [`Event::Diagnostic`].
///
/// Describes an `-error` or `-warning` directive surfaced to the
/// caller. The preprocessor never writes to stdout / stderr / a
/// logger; the caller decides whether to abort, record, or ignore.
/// State machine is not held pending: after this event, `step`
/// returns the next event as usual.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Whether this is `-error` or `-warning`.
    pub severity: Severity,
    /// Argument tokens inside the parentheses, kept as a flat
    /// stream that includes hidden tokens (whitespace / comments)
    /// — same convention as
    /// [`MacroCall::arguments`]'s inner streams. Each
    /// token's `Origin` is the directive site's origin (the
    /// `parent_origin` on this same struct); no macro expansion is
    /// applied inside the arguments.
    pub arguments: Vec<SourceToken>,
    /// Span of the whole directive (`-` through `.`).
    pub directive_span: SourceSpan,
    /// Span of the argument tokens, from the first lexical token's
    /// start to the last lexical token's end (hidden token edges
    /// are not included).
    pub arg_span: SourceSpan,
    /// Origin at the directive's site.
    pub parent_origin: Arc<Origin>,
}

/// Data of an [`Event::AwaitingMacroExpansion`].
///
/// Describes the macro call the caller must resolve. `arity` is
/// `None` for a bare `?NAME` and `Some(n)` for `?NAME(a1, ..., an)`;
/// when `arity` is `Some(n)`, `arguments` holds the `n` argument
/// token streams (each may include hidden tokens like whitespace and
/// comments). An arity-0 call (`?NAME()` or `?NAME(   )`) is
/// `arity: Some(0)` with an empty `arguments`, distinct from a
/// constant-like `?NAME` where `arity` is `None` and `arguments` is
/// also empty.
#[derive(Debug, Clone)]
pub struct MacroCall {
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
    pub arguments: Vec<Vec<SourceToken>>,
}
