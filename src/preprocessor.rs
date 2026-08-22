//! Sans-I/O preprocessor state machine.
//!
//! [`Preprocessor`] owns a [`Cursor`], a shared [`SourceStore`], and a
//! small state variable that tracks whether the machine is currently
//! awaiting a response. Callers drive the machine one step at a time
//! with [`Preprocessor::step`] and, when the returned event leaves the
//! machine awaiting a response, respond through one of the response
//! methods before calling `step` again.
//!
//! The preprocessor consumes pre-scanned [`Source`] token streams;
//! tokenization is the caller's responsibility (scan with
//! [`erl_tokenize::scan_token`] and hand the resulting tokens to
//! [`Source::new`]). Lexical errors surface only when the caller
//! scans, never through [`Preprocessor::step`].
//!
//! [`Event::Token`](crate::Event::Token) is lexical only. Whitespace
//! and comments stay in the caller's [`Source`]; they are walked for
//! recognition but never re-emitted.
//!
//! This module intentionally does no I/O and holds no runtime, path,
//! or logging dependency.
#![expect(
    clippy::result_large_err,
    reason = "PreprocessError deliberately carries structured spans; \
              boxing every Result would add allocation overhead on every define"
)]

use crate::cursor::Cursor;
use crate::directive::{Directive, parse_directive};
use crate::error::{MacroCallErrorKind, PreprocessError, ProtocolError};
use crate::event::{
    Branch, BranchBoundary, Conditional, DefinedConditional, Diagnostic, Event,
    ExpressionConditional, IncludeDirective, MacroCall, Severity, UndefinedMacro,
};
use crate::macros::{MacroDefinition, MacroKey, MacroTable};
use crate::origin::{IncludeKind, Origin, SourceInfoMacroKind};
use crate::source::{Source, SourceId, SourceSpan, SourceStore};
use crate::source_string::SourceString;
use crate::source_token::SourceToken;
use std::collections::VecDeque;
use std::sync::Arc;

/// Sans-I/O preprocessor state machine.
///
/// # Overview
///
/// 1. Create with [`Preprocessor::new`] and a sequence of [`Source`]s.
/// 2. Call [`step`](Self::step) repeatedly; every call advances the
///    machine by one transition and returns exactly one [`Event`].
/// 3. When the returned event leaves the machine awaiting a response,
///    invoke the matching response method before calling `step` again.
///    The event names which response is expected.
/// 4. When [`Event::Complete`] is returned, later `step` calls keep
///    returning `Event::Complete`.
///
/// [`Event::Token`] is lexical only. Whitespace and comments remain
/// in the caller's [`Source`] and are not re-emitted.
///
/// [`Clone`] shares the [`SourceStore`]. Cursor, macro table, and
/// branch stack are independent. There is no API that merges two
/// forked machines.
pub struct Preprocessor {
    sources: Arc<SourceStore>,
    /// Cursor for the source currently being scanned. `None` when
    /// [`Preprocessor::new`] received an empty sequence (the next
    /// `step` completes) or after the last top-level source has
    /// been left; in the latter case `state` is already
    /// [`State::Completed`].
    cursor: Option<Cursor>,
    /// Remaining top-level sources, in scan order. Distinct from
    /// `include_stack`, which nests `-include` inside the current
    /// top-level source.
    source_queue: VecDeque<SourceId>,
    /// Origin attached to every token emitted from the current cursor
    /// (`Origin::Source` at the top level, `Origin::Include { ... }`
    /// while an include source is active). Swapped in and out
    /// together with `cursor` on include push / pop.
    current_origin: Arc<Origin>,
    /// Parent (cursor, current_origin) pairs saved when an include is
    /// pushed; restored in the same order on include EOF.
    include_stack: Vec<(Cursor, Arc<Origin>)>,
    /// Currently open conditional frames. Pushed on
    /// `-ifdef` / `-ifndef` (via [`Preprocessor::resume_conditional`]),
    /// flipped on `-else`, popped on `-endif`. The top of the stack
    /// determines whether the current scan is active or an inactive
    /// skip.
    branch_stack: Vec<BranchFrame>,
    /// Macro table updated on `-define` / `-undef`.
    macros: MacroTable,
    /// State-machine state.
    state: State,
    /// `true` when the cursor stands at a form boundary and the next
    /// scan step should attempt directive recognition.
    ///
    /// Initialised to `true` (start of source). Flipped to `false` as
    /// soon as any lexical token is bumped, or as soon as
    /// `parse_directive` returns `Ok(None)`. Flipped back to `true`
    /// after a successful directive parse (the parser consumed the
    /// terminating `.`) or after a lexical `.` symbol is bumped.
    at_form_boundary: bool,
    /// Tokens queued for emission as the result of a macro expansion.
    ///
    /// Drained by [`Preprocessor::step`] before consulting the cursor,
    /// so a `?FOO` call finished during this or an earlier step
    /// surfaces its replacement before scanning continues.
    expansion_queue: VecDeque<SourceToken>,
    /// When `Some`, the scanner is expanding an `-if` / `-elif`
    /// condition expression: queue tokens are collected into this
    /// buffer instead of being emitted as [`Event::Token`], and the
    /// inactive-branch skip guard is bypassed so macros inside a
    /// skipped `-elif` condition still expand.
    condition_collect: Option<ConditionCollect>,
}

/// State-machine state.
#[derive(Debug, Clone)]
enum State {
    /// Default state: `step` runs the scan loop.
    Scanning,
    /// The scan loop emitted [`Event::AwaitingInclude`] and is
    /// waiting for a [`Preprocessor::resume_include`] call. The
    /// payload retains what the resume path needs to build the new
    /// `Origin::Include` when a `Source` is supplied.
    AwaitingIncludeResolution(PendingInclude),
    /// The scan loop emitted [`Event::AwaitingConditional`] and is
    /// waiting for a [`Preprocessor::resume_conditional`] call. The
    /// payload retains what the resume path needs to push a matching
    /// `BranchFrame` onto the branch stack.
    AwaitingConditionalDecision(PendingConditional),
    /// The scan loop emitted [`Event::AwaitingMacroExpansion`] and is
    /// waiting for a [`Preprocessor::resume_macro_expansion`] call.
    /// The payload retains what the resume path needs to attach the
    /// caller-supplied tokens to the right call site.
    AwaitingMacroExpansion(PendingExpansion),
    /// The input has been fully processed.
    Completed,
}

/// Bookkeeping saved while an [`Event::AwaitingInclude`] is pending,
/// used to build the correct [`Origin::Include`] when a `Source` is
/// supplied via [`Preprocessor::resume_include`].
#[derive(Debug, Clone)]
struct PendingInclude {
    /// Origin at the include directive's call site; becomes the
    /// `parent` of the new `Origin::Include`.
    parent_origin: Arc<Origin>,
    /// Span of the whole directive at the call site; becomes the
    /// `include_site` of the new `Origin::Include`.
    directive_span: SourceSpan,
    /// Whether the directive was `-include` or `-include_lib`;
    /// becomes the `kind` of the new `Origin::Include`.
    kind: IncludeKind,
}

/// Bookkeeping saved while an [`Event::AwaitingConditional`] is
/// pending. Distinguishes an opening directive (push a new frame on
/// resume) from an `-elif` continuation (update the existing frame).
#[derive(Debug, Clone)]
enum PendingConditional {
    /// `-if` / `-ifdef` / `-ifndef`: push a new [`BranchFrame`] on
    /// resume. `is_if_chain` is `true` only for `-if`.
    OpenNew {
        directive_span: SourceSpan,
        is_if_chain: bool,
    },
    /// `-elif`: update the top frame's `active` / `chain_active_seen`
    /// without pushing.
    ContinueElif,
}

/// A conditional currently open on the branch stack. Pushed when an
/// opening `-if` / `-ifdef` / `-ifndef` is resolved; the `active` flag
/// flips on `-else` (with an exception for taken `-if` chains); the
/// frame is popped on `-endif`.
#[derive(Debug, Clone)]
struct BranchFrame {
    /// Whether the current side of the frame is being executed
    /// (`true`) or skipped (`false`). Flips on `-else` for
    /// `-ifdef` / `-ifndef`; for an `-if` chain that already took a
    /// branch, stays `false` through later `-else`.
    active: bool,
    /// `true` once `-else` has been observed for this frame.
    else_seen: bool,
    /// `true` when the frame was pushed while an outer frame was
    /// already inactive. Silent frames do not fire
    /// `Event::BranchBoundary` on `-else` / `-endif`, matching the
    /// polished contract that nested conditionals inside an
    /// inactive skip stay invisible to the caller.
    silent_close: bool,
    /// Span of the opening `-if` / `-ifdef` / `-ifndef`. Reported by
    /// `UnclosedConditional` when the source ends with the frame
    /// still on the stack.
    open_span: SourceSpan,
    /// `true` once any branch of this `-if` / `-elif` chain has been
    /// taken. Unused (`false`) for `-ifdef` / `-ifndef` frames.
    chain_active_seen: bool,
    /// `true` when this frame was opened by `-if` (so `-elif` is
    /// legal). `false` for `-ifdef` / `-ifndef`.
    is_if_chain: bool,
}

/// Distinguishes `-ifdef` from `-ifndef` when assembling a
/// [`Conditional`]. That split lives on
/// [`Conditional::Ifdef`] / [`Conditional::Ifndef`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefinedKind {
    Ifdef,
    Ifndef,
}

/// Distinguishes `-if` from `-elif` while a condition expression is
/// being expanded. That split lives on
/// [`Conditional::If`] / [`Conditional::Elif`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExprKind {
    If,
    Elif,
}

/// In-flight expansion of an `-if` / `-elif` condition expression.
#[derive(Debug, Clone)]
struct ConditionCollect {
    kind: ExprKind,
    /// Span of the `-if` / `-elif` directive itself (event payload).
    directive_span: SourceSpan,
    pending: PendingConditional,
    collected: Vec<SourceToken>,
}

/// Bookkeeping saved while an [`Event::AwaitingMacroExpansion`] is
/// pending, used to build the correct [`Origin::CallerExpansion`] for
/// the response tokens.
#[derive(Debug, Clone)]
struct PendingExpansion {
    /// Name of the requested macro, echoed on every response token's
    /// origin.
    name: SourceString,
    /// Span of the whole `?NAME(...)` call at the call site.
    call_site: SourceSpan,
    /// Origin at the call site itself; response tokens hang under this
    /// as their parent origin.
    parent_origin: Arc<Origin>,
}

impl Preprocessor {
    /// Creates a preprocessor that scans `sources` from front to back.
    ///
    /// Each item is appended to a freshly created shared
    /// [`SourceStore`] in iterator order. Scanning starts at the
    /// first source; when that source's top-level cursor hits EOF
    /// (and no include is open), the next source is activated with
    /// [`Origin::Source`] and a fresh form boundary. An empty
    /// sequence completes on the first [`step`](Self::step).
    ///
    /// The constructor does not parse or insert macros. A leading
    /// `-define(...)` source is scanned like any other and surfaces
    /// as [`Event::MacroDefined`]. To seed environment macros such as
    /// `?MACHINE` / `?OTP_RELEASE` before the main file, prepend that
    /// source to the iterator; see [`docs::recipes`](crate::docs::recipes).
    pub fn new<I>(sources: I) -> Self
    where
        I: IntoIterator<Item = Source>,
    {
        let store = Arc::new(SourceStore::new());
        let mut source_queue = VecDeque::new();
        for source in sources {
            source_queue.push_back(store.append(source));
        }
        let cursor = source_queue.pop_front().map(|id| {
            let arc_source = store.get(id);
            Cursor::new(id, arc_source)
        });
        Self {
            sources: store,
            cursor,
            source_queue,
            current_origin: Arc::new(Origin::Source),
            include_stack: Vec::new(),
            branch_stack: Vec::new(),
            macros: MacroTable::new(),
            state: State::Scanning,
            at_form_boundary: true,
            expansion_queue: VecDeque::new(),
            condition_collect: None,
        }
    }

    fn cursor(&self) -> &Cursor {
        self.cursor
            .as_ref()
            .expect("scan requires an active source")
    }

    fn cursor_mut(&mut self) -> &mut Cursor {
        self.cursor
            .as_mut()
            .expect("scan requires an active source")
    }

    /// Returns a shared handle to the underlying source store.
    pub fn sources(&self) -> &Arc<SourceStore> {
        &self.sources
    }

    /// Returns a read-only view of the macro table.
    ///
    /// Entries are added by `-define(...)` directives and removed by
    /// `-undef(...)` directives observed while scanning. Once a
    /// directive is applied the table update is visible from this
    /// method before the caller receives the matching
    /// [`Event::MacroDefined`] or [`Event::MacroUndefined`] — the
    /// state-then-event ordering is fixed (see [`step`](Self::step)).
    pub fn macros(&self) -> &MacroTable {
        &self.macros
    }

    /// Advances the state machine and returns one [`Event`].
    ///
    /// Returns `Err(ProtocolError)` when the machine is awaiting a
    /// response; the caller must respond before calling this method
    /// again. The last [`Event`] names which response is expected.
    pub fn step(&mut self) -> Result<Event, ProtocolError> {
        match &self.state {
            State::AwaitingIncludeResolution(_)
            | State::AwaitingConditionalDecision(_)
            | State::AwaitingMacroExpansion(_) => Err(ProtocolError),
            State::Completed => Ok(Event::Complete),
            State::Scanning => Ok(self.step_scan()),
        }
    }

    /// Resumes the scan loop after an
    /// [`Event::AwaitingMacroExpansion`] event.
    ///
    /// `source` is the caller-supplied expansion result whose tokens
    /// are spliced into the token stream **in front of** any tokens
    /// already queued (for example the rest of a `-define` body that
    /// still contains nested `?NAME` calls). Appending them would
    /// emit those remaining body tokens first and leave holes such as
    /// `{, ,}` followed by the replacements. Pass a token-free
    /// [`Source`] to skip the call without emitting any expansion
    /// tokens; the caller is responsible for surfacing any error
    /// diagnostic in its own error stream.
    ///
    /// Returns `Err(ProtocolError)` when no macro-expansion response
    /// is expected (the machine is scanning, completed, or awaiting a
    /// different response). The last [`Event`] names which wait, if
    /// any, is in force.
    pub fn resume_macro_expansion(&mut self, source: Source) -> Result<(), ProtocolError> {
        match &self.state {
            State::AwaitingMacroExpansion(_) => {}
            State::AwaitingIncludeResolution(_)
            | State::AwaitingConditionalDecision(_)
            | State::Scanning
            | State::Completed => return Err(ProtocolError),
        }
        let State::AwaitingMacroExpansion(pending) =
            std::mem::replace(&mut self.state, State::Scanning)
        else {
            unreachable!("state was checked immediately above");
        };
        let source_id = self.sources.append(source);
        let source_arc = self.sources.get(source_id);
        let mut response = VecDeque::new();
        for token in source_arc.tokens() {
            let origin = Origin::CallerExpansion {
                parent: Arc::clone(&pending.parent_origin),
                call_site: pending.call_site,
                name: pending.name.clone(),
            };
            response.push_back(SourceToken::new(
                *token,
                Arc::clone(&source_arc),
                source_id,
                origin,
            ));
        }
        self.prepend_to_queue(response);
        Ok(())
    }

    /// Resumes the scan loop after an [`Event::AwaitingInclude`]
    /// event.
    ///
    /// `source` is the caller-supplied include content. It is
    /// appended to the shared [`SourceStore`] and becomes the active
    /// cursor; the parent cursor and origin are pushed onto the
    /// include stack and restored on include EOF. Pass a token-free
    /// [`Source`] to skip the include without processing any content
    /// (same shape as [`Preprocessor::resume_macro_expansion`]); the
    /// caller is responsible for surfacing any diagnostic in its own
    /// error stream.
    ///
    /// Returns `Err(ProtocolError)` when no include response is
    /// expected (the machine is scanning, completed, or awaiting a
    /// different response). The last [`Event`] names which wait, if
    /// any, is in force.
    pub fn resume_include(&mut self, source: Source) -> Result<(), ProtocolError> {
        match &self.state {
            State::AwaitingIncludeResolution(_) => {}
            State::AwaitingMacroExpansion(_)
            | State::AwaitingConditionalDecision(_)
            | State::Scanning
            | State::Completed => return Err(ProtocolError),
        }
        let State::AwaitingIncludeResolution(pending) =
            std::mem::replace(&mut self.state, State::Scanning)
        else {
            unreachable!("state was checked immediately above");
        };
        // Push parent cursor + origin, swap in the include cursor. A
        // token-free source hits EOF on the next step_scan iteration
        // and the parent is restored right away — no special-cased
        // skip path.
        let source_id = self.sources.append(source);
        let source_arc = self.sources.get(source_id);
        let child_cursor = Cursor::new(source_id, source_arc);
        let child_origin = Arc::new(Origin::Include {
            parent: Arc::clone(&pending.parent_origin),
            include_site: pending.directive_span,
            kind: pending.kind,
        });
        let parent_cursor = self
            .cursor
            .replace(child_cursor)
            .expect("include resume requires an active source");
        let parent_origin = std::mem::replace(&mut self.current_origin, child_origin);
        self.include_stack.push((parent_cursor, parent_origin));
        // Fresh source starts at a form boundary.
        self.at_form_boundary = true;
        Ok(())
    }

    /// Resumes the scan loop after an [`Event::AwaitingConditional`]
    /// event.
    ///
    /// `branch` is the side of the conditional the caller wants to
    /// scan (`Branch::Then` for tokens up to `-else`, `Branch::Else`
    /// for tokens after `-else`). Both sides may be processed by
    /// [`Preprocessor::clone`]-ing the machine at the awaiting event
    /// and calling this method on the two clones with different
    /// arguments.
    ///
    /// Returns `Err(ProtocolError)` when no conditional response is
    /// expected (the machine is scanning, completed, or awaiting a
    /// different response). The last [`Event`] names which wait, if
    /// any, is in force.
    pub fn resume_conditional(&mut self, branch: Branch) -> Result<(), ProtocolError> {
        match &self.state {
            State::AwaitingConditionalDecision(_) => {}
            State::AwaitingIncludeResolution(_)
            | State::AwaitingMacroExpansion(_)
            | State::Scanning
            | State::Completed => return Err(ProtocolError),
        }
        let State::AwaitingConditionalDecision(pending) =
            std::mem::replace(&mut self.state, State::Scanning)
        else {
            unreachable!("state was checked immediately above");
        };
        let active = matches!(branch, Branch::Then);
        match pending {
            PendingConditional::OpenNew {
                directive_span,
                is_if_chain,
            } => {
                // AwaitingConditional for an opening directive only
                // fires from an active state, so this frame is never
                // nested inside an inactive one.
                self.branch_stack.push(BranchFrame {
                    active,
                    else_seen: false,
                    silent_close: false,
                    open_span: directive_span,
                    chain_active_seen: active,
                    is_if_chain,
                });
            }
            PendingConditional::ContinueElif => {
                let frame = self
                    .branch_stack
                    .last_mut()
                    .expect("ContinueElif requires an open -if frame");
                frame.active = active;
                if active {
                    frame.chain_active_seen = true;
                }
            }
        }
        Ok(())
    }

    /// Runs the scan loop until it can produce one event.
    ///
    /// See the module rustdoc for the loop contract.
    fn step_scan(&mut self) -> Event {
        type Step = fn(&mut Preprocessor) -> StepAction;
        const STEPS: [Step; 7] = [
            Preprocessor::try_rescan_queue_head,
            Preprocessor::drain_expansion_queue,
            Preprocessor::finish_condition_collect_if_ready,
            Preprocessor::handle_cursor_eof,
            Preprocessor::try_parse_directive_at_boundary,
            Preprocessor::try_scan_macro_call,
            Preprocessor::bump_cursor,
        ];
        loop {
            for step in STEPS {
                match step(self) {
                    StepAction::Emit(event) => return *event,
                    StepAction::Retry => break,
                    StepAction::Fall => continue,
                }
            }
        }
    }

    /// Makes a macro that appeared in an earlier expansion body
    /// expand itself before its tokens surface as regular output.
    fn try_rescan_queue_head(&mut self) -> StepAction {
        // Do not recognize macros while skipping an inactive branch,
        // unless we are expanding an `-if` / `-elif` condition (those
        // must expand even when the surrounding frame is inactive).
        if self.condition_collect.is_none() && self.is_in_inactive_branch() {
            return StepAction::Fall;
        }
        if !self
            .expansion_queue
            .front()
            .is_some_and(|ppt| is_symbol(*ppt.token(), erl_tokenize::Symbol::Question))
        {
            return StepAction::Fall;
        }
        match self.try_rescan_queue_call() {
            MacroCallOutcome::Fire(event) => StepAction::Emit(event),
            MacroCallOutcome::Enqueued => StepAction::Retry,
            // The `?` is not the start of a macro call — emit it and
            // any following queued tokens normally by falling through
            // to the drain.
            MacroCallOutcome::NotACall => StepAction::Fall,
        }
    }

    fn drain_expansion_queue(&mut self) -> StepAction {
        let Some(ppt) = self.expansion_queue.pop_front() else {
            return StepAction::Fall;
        };
        if let Some(collect) = self.condition_collect.as_mut() {
            // Condition expansion: buffer tokens; do not emit and do
            // not disturb the main source's form-boundary flag.
            collect.collected.push(ppt);
            return StepAction::Retry;
        }
        let token = *ppt.token();
        self.update_form_boundary_after_bump(token);
        if self.is_in_inactive_branch() {
            // Silently discard queued tokens while skipping.
            return StepAction::Retry;
        }
        emit_lexical_token(ppt)
    }

    /// When condition expansion has drained the queue, fire
    /// [`Event::AwaitingConditional`] with the collected tokens.
    fn finish_condition_collect_if_ready(&mut self) -> StepAction {
        if self.condition_collect.is_none() {
            return StepAction::Fall;
        }
        if !self.expansion_queue.is_empty() {
            return StepAction::Fall;
        }
        let collect = self
            .condition_collect
            .take()
            .expect("checked is_none above");
        StepAction::Emit(Box::new(self.fire_awaiting_conditional_expr(collect)))
    }

    fn handle_cursor_eof(&mut self) -> StepAction {
        if let Some(cursor) = &self.cursor
            && !cursor.is_at_eof()
        {
            return StepAction::Fall;
        }
        if let Some((parent_cursor, parent_origin)) = self.include_stack.pop() {
            self.cursor = Some(parent_cursor);
            self.current_origin = parent_origin;
            // Coming back to the parent source lands on the token
            // right after the include directive, which is always a
            // form boundary.
            self.at_form_boundary = true;
            return StepAction::Retry;
        }
        if let Some(id) = self.source_queue.pop_front() {
            self.cursor = Some(Cursor::new(id, self.sources.get(id)));
            self.current_origin = Arc::new(Origin::Source);
            self.at_form_boundary = true;
            return StepAction::Retry;
        }
        // Top-level EOF after the last source: an open conditional
        // is a syntax error. Report the opening directive's span for
        // the outermost still-open frame and clear the stack so the
        // error does not fire twice.
        if let Some(frame) = self.branch_stack.first() {
            let open_span = frame.open_span;
            self.branch_stack.clear();
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::UnclosedConditional { span: open_span },
            )));
        }
        self.state = State::Completed;
        StepAction::Emit(Box::new(Event::Complete))
    }

    fn try_parse_directive_at_boundary(&mut self) -> StepAction {
        if !self.at_form_boundary {
            return StepAction::Fall;
        }
        match parse_directive(self.cursor_mut()) {
            Ok(Some(directive)) => self.dispatch_directive(directive),
            Ok(None) => {
                // Cursor restored to entry. Fall through — if the next
                // token is `?` and a macro call expands to nothing
                // lexical, we want to stay at form boundary so the
                // *following* form can still be recognized as a
                // directive. `update_form_boundary_after_bump` on the
                // eventual bump path will drop the flag once a
                // non-`.` lexical token is emitted.
                StepAction::Fall
            }
            Err(pe) => {
                self.at_form_boundary = false;
                if self.is_in_inactive_branch() {
                    // Skipping: swallow the parse error silently
                    // rather than surface diagnostics in code the
                    // caller chose not to compile.
                    return StepAction::Retry;
                }
                StepAction::Emit(Box::new(Event::PreprocessError(pe.into())))
            }
        }
    }

    /// `true` when scanning should be silently skipping tokens
    /// because at least one open conditional frame is on the
    /// inactive side.
    fn is_in_inactive_branch(&self) -> bool {
        self.branch_stack.iter().any(|f| !f.active)
    }

    fn dispatch_directive(&mut self, directive: Directive) -> StepAction {
        // The parser consumed the whole directive including the
        // terminating `.`, so we are at a new form boundary.
        self.at_form_boundary = true;
        let inactive = self.is_in_inactive_branch();
        // Conditional directives must be tracked in both branches
        // so `-else` / `-endif` line up correctly.
        match &directive {
            Directive::Ifdef { span, name } => {
                if inactive {
                    self.push_nested_inactive_frame(*span, false);
                    return StepAction::Retry;
                }
                return StepAction::Emit(Box::new(self.fire_awaiting_defined(
                    name.clone(),
                    *span,
                    DefinedKind::Ifdef,
                )));
            }
            Directive::Ifndef { span, name } => {
                if inactive {
                    self.push_nested_inactive_frame(*span, false);
                    return StepAction::Retry;
                }
                return StepAction::Emit(Box::new(self.fire_awaiting_defined(
                    name.clone(),
                    *span,
                    DefinedKind::Ifndef,
                )));
            }
            Directive::If {
                span, arg_tokens, ..
            } => {
                if inactive {
                    self.push_nested_inactive_frame(*span, true);
                    return StepAction::Retry;
                }
                return self.start_condition_collect(
                    ExprKind::If,
                    *span,
                    PendingConditional::OpenNew {
                        directive_span: *span,
                        is_if_chain: true,
                    },
                    arg_tokens,
                );
            }
            Directive::Elif {
                span, arg_tokens, ..
            } => {
                return self.dispatch_elif(*span, arg_tokens);
            }
            Directive::Else { span } => {
                return self.dispatch_else(*span);
            }
            Directive::Endif { span } => {
                return self.dispatch_endif(*span);
            }
            _ => {}
        }
        // Non-conditional directive inside an inactive branch: no
        // event, no macro-table effect. The parser already advanced
        // the cursor past it.
        if inactive {
            return StepAction::Retry;
        }
        // `-include` / `-include_lib` fold into Event::AwaitingInclude;
        // `-error` / `-warning` fold into Event::Diagnostic;
        // `-define` / `-undef` update the table then emit
        // MacroDefined / MacroUndefined.
        match directive {
            Directive::Include { kind, path, span } => {
                StepAction::Emit(Box::new(self.fire_awaiting_include(kind, path, span)))
            }
            Directive::Error {
                arg_tokens,
                span,
                arg_span,
            } => StepAction::Emit(Box::new(self.fire_diagnostic(
                Severity::Error,
                &arg_tokens,
                span,
                arg_span,
            ))),
            Directive::Warning {
                arg_tokens,
                span,
                arg_span,
            } => StepAction::Emit(Box::new(self.fire_diagnostic(
                Severity::Warning,
                &arg_tokens,
                span,
                arg_span,
            ))),
            Directive::Define { .. } | Directive::Undef { .. } => {
                match self.apply_macro_directive(directive) {
                    Ok(event) => StepAction::Emit(Box::new(event)),
                    Err(e) => StepAction::Emit(Box::new(Event::PreprocessError(e))),
                }
            }
            Directive::Ifdef { .. }
            | Directive::Ifndef { .. }
            | Directive::If { .. }
            | Directive::Elif { .. }
            | Directive::Else { .. }
            | Directive::Endif { .. } => {
                unreachable!("conditionals already returned above")
            }
        }
    }

    /// Pushes a `BranchFrame` for a nested `-if` / `-ifdef` / `-ifndef`
    /// encountered while an outer frame is already inactive. The
    /// frame is marked `silent_close` so its `-else` / `-endif` do
    /// not fire boundary events and its `active` starts `false`
    /// (nested content is always skipped alongside the outer skip).
    fn push_nested_inactive_frame(&mut self, open_span: SourceSpan, is_if_chain: bool) {
        self.branch_stack.push(BranchFrame {
            active: false,
            else_seen: false,
            silent_close: true,
            open_span,
            chain_active_seen: false,
            is_if_chain,
        });
    }

    fn fire_awaiting_include(
        &mut self,
        kind: IncludeKind,
        path: SourceString,
        directive_span: SourceSpan,
    ) -> Event {
        let parent_origin = Arc::clone(&self.current_origin);
        let include = IncludeDirective {
            kind,
            path,
            directive_span,
            parent_origin: Arc::clone(&parent_origin),
        };
        self.state = State::AwaitingIncludeResolution(PendingInclude {
            parent_origin,
            directive_span,
            kind,
        });
        Event::AwaitingInclude(include)
    }

    fn fire_diagnostic(
        &self,
        severity: Severity,
        arg_tokens: &[erl_tokenize::Token],
        directive_span: SourceSpan,
        arg_span: SourceSpan,
    ) -> Event {
        let parent_origin = Arc::clone(&self.current_origin);
        let source_id = self.cursor().source_id();
        let source_arc = Arc::clone(self.cursor().source());
        let arguments = arg_tokens
            .iter()
            .map(|token| {
                SourceToken::new(
                    *token,
                    Arc::clone(&source_arc),
                    source_id,
                    (*parent_origin).clone(),
                )
            })
            .collect();
        Event::Diagnostic(Diagnostic {
            severity,
            arguments,
            directive_span,
            arg_span,
            parent_origin,
        })
    }

    /// Handles a `-else` directive: flips the top branch frame's
    /// active side (unless a taken `-if` chain must stay inactive),
    /// records the else, and fires `Event::BranchBoundary(Else)`
    /// unless the frame is a silent nested one. Stray `-else` (no
    /// matching opening directive) and double `-else` inside the
    /// same conditional surface as [`PreprocessError::StrayElse`] /
    /// [`PreprocessError::DoubleElse`].
    fn dispatch_else(&mut self, span: SourceSpan) -> StepAction {
        let Some(frame) = self.branch_stack.last_mut() else {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::StrayElse { span },
            )));
        };
        if frame.else_seen {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::DoubleElse { span },
            )));
        }
        frame.else_seen = true;
        if frame.is_if_chain && frame.chain_active_seen {
            // A later `-else` must not revive a branch after an
            // earlier `-if` / `-elif` was taken.
            frame.active = false;
        } else {
            frame.active = !frame.active;
        }
        let silent = frame.silent_close;
        if silent {
            StepAction::Retry
        } else {
            StepAction::Emit(Box::new(self.fire_else_boundary(span)))
        }
    }

    /// Handles an `-elif(...)` directive according to the open frame.
    fn dispatch_elif(
        &mut self,
        span: SourceSpan,
        arg_tokens: &[erl_tokenize::Token],
    ) -> StepAction {
        let Some(frame) = self.branch_stack.last() else {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::StrayElif { span },
            )));
        };
        if !frame.is_if_chain {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::StrayElif { span },
            )));
        }
        if frame.else_seen {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::ElifAfterElse { span },
            )));
        }
        if frame.silent_close {
            return StepAction::Retry;
        }
        if frame.chain_active_seen {
            // Taken earlier in the chain: skip this `-elif` body
            // without expanding its condition or asking the caller.
            let frame = self
                .branch_stack
                .last_mut()
                .expect("frame was present above");
            frame.active = false;
            return StepAction::Retry;
        }
        self.start_condition_collect(
            ExprKind::Elif,
            span,
            PendingConditional::ContinueElif,
            arg_tokens,
        )
    }

    /// Starts macro-expanding the condition tokens of an `-if` /
    /// `-elif` by injecting them into the expansion queue.
    fn start_condition_collect(
        &mut self,
        kind: ExprKind,
        directive_span: SourceSpan,
        pending: PendingConditional,
        arg_tokens: &[erl_tokenize::Token],
    ) -> StepAction {
        let source_id = self.cursor_mut().source_id();
        let source_arc = Arc::clone(self.cursor_mut().source());
        let origin = (*self.current_origin).clone();
        let mut wrapped = VecDeque::with_capacity(arg_tokens.len());
        for token in arg_tokens {
            wrapped.push_back(SourceToken::new(
                *token,
                Arc::clone(&source_arc),
                source_id,
                origin.clone(),
            ));
        }
        self.condition_collect = Some(ConditionCollect {
            kind,
            directive_span,
            pending,
            collected: Vec::new(),
        });
        self.prepend_to_queue(wrapped);
        StepAction::Retry
    }

    /// Handles a `-endif` directive: pops the top branch frame and
    /// fires `Event::BranchBoundary(Endif)` unless the popped frame
    /// was a silent nested one. Stray `-endif` (no matching opening
    /// directive) surfaces as [`PreprocessError::StrayEndif`].
    fn dispatch_endif(&mut self, span: SourceSpan) -> StepAction {
        let Some(frame) = self.branch_stack.pop() else {
            return StepAction::Emit(Box::new(Event::PreprocessError(
                PreprocessError::StrayEndif { span },
            )));
        };
        if frame.silent_close {
            StepAction::Retry
        } else {
            StepAction::Emit(Box::new(self.fire_endif_boundary(span)))
        }
    }

    fn fire_else_boundary(&self, directive_span: SourceSpan) -> Event {
        Event::BranchBoundary(BranchBoundary::Else {
            directive_span,
            parent_origin: Arc::clone(&self.current_origin),
        })
    }

    fn fire_endif_boundary(&self, directive_span: SourceSpan) -> Event {
        Event::BranchBoundary(BranchBoundary::Endif {
            directive_span,
            parent_origin: Arc::clone(&self.current_origin),
        })
    }

    fn fire_awaiting_defined(
        &mut self,
        name: SourceString,
        directive_span: SourceSpan,
        kind: DefinedKind,
    ) -> Event {
        let defined = self.macros.is_defined(name.as_str());
        let recommended = recommended_defined_branch(kind, defined);
        let payload = DefinedConditional {
            name,
            defined,
            recommended,
            directive_span,
            parent_origin: Arc::clone(&self.current_origin),
        };
        let conditional = match kind {
            DefinedKind::Ifdef => Conditional::Ifdef(payload),
            DefinedKind::Ifndef => Conditional::Ifndef(payload),
        };
        self.state = State::AwaitingConditionalDecision(PendingConditional::OpenNew {
            directive_span,
            is_if_chain: false,
        });
        Event::AwaitingConditional(conditional)
    }

    fn fire_awaiting_conditional_expr(&mut self, collect: ConditionCollect) -> Event {
        let payload = ExpressionConditional {
            condition_tokens: collect.collected,
            directive_span: collect.directive_span,
            parent_origin: Arc::clone(&self.current_origin),
        };
        let conditional = match collect.kind {
            ExprKind::If => Conditional::If(payload),
            ExprKind::Elif => Conditional::Elif(payload),
        };
        self.state = State::AwaitingConditionalDecision(collect.pending);
        Event::AwaitingConditional(conditional)
    }

    fn try_scan_macro_call(&mut self) -> StepAction {
        // Skipping: don't try to recognize any macro call. The `?`
        // and its following tokens will be silently consumed by the
        // bump path.
        if self.is_in_inactive_branch() {
            return StepAction::Fall;
        }
        if !self
            .cursor_mut()
            .peek()
            .is_some_and(|t| is_symbol(t, erl_tokenize::Symbol::Question))
        {
            return StepAction::Fall;
        }
        match self.try_recognize_macro_call() {
            MacroCallOutcome::Fire(event) => StepAction::Emit(event),
            MacroCallOutcome::Enqueued => StepAction::Retry,
            MacroCallOutcome::NotACall => StepAction::Fall,
        }
    }

    /// Returns `Retry` (not `Fall`) when the cursor produced nothing,
    /// so `handle_cursor_eof` gets a chance to pop the include stack,
    /// activate the next top-level source, or emit `Event::Complete`
    /// on the next round.
    fn bump_cursor(&mut self) -> StepAction {
        let Some(token) = self.cursor_mut().bump() else {
            return StepAction::Retry;
        };
        self.update_form_boundary_after_bump(token);
        if self.is_in_inactive_branch() {
            // Silently consume tokens while skipping.
            return StepAction::Retry;
        }
        let ppt = SourceToken::new(
            token,
            Arc::clone(self.cursor_mut().source()),
            self.cursor_mut().source_id(),
            (*self.current_origin).clone(),
        );
        emit_lexical_token(ppt)
    }

    /// Attempts to recognize a `?NAME` macro call at the cursor.
    ///
    /// Called only when the next raw token is `?`. Handles the
    /// constant-like shape (`?NAME`, no arguments) in this phase; the
    /// function-like shape (`?NAME(...)`) is left to a later phase
    /// and reported as [`MacroCallOutcome::NotACall`] with the cursor
    /// restored.
    fn try_recognize_macro_call(&mut self) -> MacroCallOutcome {
        let entry = self.cursor_mut().checkpoint();
        let question_tok = self
            .cursor_mut()
            .bump()
            .expect("caller checked next token is `?`");

        let Some(name_tok) = self.cursor_mut().peek_lexical() else {
            self.cursor_mut().restore(entry);
            return MacroCallOutcome::NotACall;
        };
        // `??` prefix is a stringification — deferred to a later phase.
        if is_symbol(name_tok, erl_tokenize::Symbol::Question) {
            self.cursor_mut().restore(entry);
            return MacroCallOutcome::NotACall;
        }
        if !matches!(
            name_tok.kind(),
            erl_tokenize::TokenKind::Atom | erl_tokenize::TokenKind::Variable
        ) {
            self.cursor_mut().restore(entry);
            return MacroCallOutcome::NotACall;
        }

        // Consume through the name token so the cursor sits just past
        // it. Any hidden tokens between `?` and the name are absorbed
        // by the call and dropped from the output stream (matching
        // OTP epp's behaviour on a whitespace-free token stream).
        while let Some(t) = self.cursor_mut().bump() {
            if t.start() == name_tok.start() {
                break;
            }
        }

        let source_id = self.cursor_mut().source_id();
        let source_text = self.cursor_mut().source_text();
        let name_text = match name_tok.value(source_text) {
            erl_tokenize::TokenValue::Atom(cow) => cow.into_owned(),
            erl_tokenize::TokenValue::Variable(name) => name.to_owned(),
            _ => {
                // Shouldn't happen given the kind check above, but
                // fall back to non-call rather than panic.
                self.cursor_mut().restore(entry);
                return MacroCallOutcome::NotACall;
            }
        };
        let name_span = SourceSpan::new(source_id, name_tok.start(), name_tok.end());
        let name_ss = SourceString::new(name_text.clone(), name_span);

        // Peek one lexical token ahead. `(` starts a function-like
        // call; anything else keeps the constant-like shape.
        let inner = self.cursor_mut().checkpoint();
        let is_function_like = self
            .cursor_mut()
            .peek_lexical()
            .is_some_and(|t| is_symbol(t, erl_tokenize::Symbol::OpenParen));
        self.cursor_mut().restore(inner);

        if !is_function_like {
            // Constant-like call.
            let call_site = SourceSpan::new(source_id, question_tok.start(), name_tok.end());
            return self.finish_recognized_call(
                name_text,
                name_ss,
                None,
                call_site,
                Vec::new(),
                Arc::clone(&self.current_origin),
            );
        }

        // Function-like call: consume through the opening `(` and
        // parse the argument list.
        let open_paren = 'find: loop {
            let Some(t) = self.cursor_mut().bump() else {
                self.cursor_mut().restore(entry);
                return MacroCallOutcome::NotACall;
            };
            if is_symbol(t, erl_tokenize::Symbol::OpenParen) {
                break 'find t;
            }
        };
        let _ = open_paren; // acknowledged — position is captured via the parse result
        let current_origin = Arc::clone(&self.current_origin);
        let mut arg_source = CursorArgSource {
            cursor: self.cursor_mut(),
            source_id,
            origin: &current_origin,
        };
        let parsed = match parse_macro_arguments(&mut arg_source) {
            Ok(p) => p,
            Err(kind) => {
                let end = self
                    .cursor_mut()
                    .peek()
                    .map(|t| t.start())
                    .unwrap_or_else(|| name_tok.end());
                let span = SourceSpan::new(source_id, question_tok.start(), end);
                return MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
                    kind.into_preprocess_error(span),
                )));
            }
        };
        let call_site = SourceSpan::new(source_id, question_tok.start(), parsed.close_end);
        let arity = Some(parsed.arguments.len());
        self.finish_recognized_call(
            name_text,
            name_ss,
            arity,
            call_site,
            parsed.arguments,
            current_origin,
        )
    }

    /// Common tail of the macro-call recognition paths.
    ///
    /// Either prepends a MacroTable hit's replacement to the
    /// expansion queue (constant-like or arity-matching function-like)
    /// or fires an [`Event::AwaitingMacroExpansion`] for the caller.
    ///
    /// `parent_origin` is the origin at the call site itself; it
    /// hangs under the newly minted [`Origin::MacroBody`],
    /// [`Origin::MacroArgument`], or [`Origin::CallerExpansion`] for
    /// every emitted token.
    fn finish_recognized_call(
        &mut self,
        name_text: String,
        name_ss: SourceString,
        arity: Option<usize>,
        call_site: SourceSpan,
        arguments: Vec<Vec<SourceToken>>,
        parent_origin: Arc<Origin>,
    ) -> MacroCallOutcome {
        match arity {
            None => {
                if let Some(def) = self.macros.get_constant(&name_text) {
                    if let Some(chain) = self
                        .macros
                        .check_circular_uses(&MacroKey::constant(name_text.clone()))
                    {
                        return fire_circular(name_text, None, call_site, chain);
                    }
                    let expanded = expand_constant_like(def, call_site, &parent_origin);
                    self.prepend_to_queue(expanded);
                    return MacroCallOutcome::Enqueued;
                }
                // erl_pp built-in `?FILE` / `?LINE`. User-defined
                // macros with the same name would have hit above and
                // shadow this path (matching OTP epp behaviour).
                if let Some(outcome) =
                    self.try_internal_source_info(&name_text, call_site, &parent_origin)
                {
                    return outcome;
                }
            }
            Some(n) => {
                if let Some(def) = self.macros.get_function(&name_text, n) {
                    if let Some(chain) = self
                        .macros
                        .check_circular_uses(&MacroKey::function(name_text.clone(), n))
                    {
                        return fire_circular(name_text, Some(n), call_site, chain);
                    }
                    match expand_function_like(
                        def,
                        &arguments,
                        call_site,
                        &parent_origin,
                        &self.sources,
                    ) {
                        Ok(expanded) => {
                            self.prepend_to_queue(expanded);
                            return MacroCallOutcome::Enqueued;
                        }
                        Err(kind) => {
                            return MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
                                kind.into_preprocess_error(call_site),
                            )));
                        }
                    }
                }
            }
        }

        // Unknown / caller-driven macro path: reject direct or indirect
        // caller-response recursion via the Origin ancestor chain.
        if let Some(chain) = collect_caller_ancestor_cycle(&parent_origin, &name_text) {
            return fire_circular(name_text, arity, call_site, chain);
        }

        let call = MacroCall {
            name: name_ss.clone(),
            arity,
            call_site,
            arguments,
        };
        self.state = State::AwaitingMacroExpansion(PendingExpansion {
            name: name_ss,
            call_site,
            parent_origin,
        });
        MacroCallOutcome::Fire(Box::new(Event::AwaitingMacroExpansion(call)))
    }

    /// Splices `tokens` in front of the current expansion queue so
    /// they surface before anything scheduled by earlier expansions.
    fn prepend_to_queue(&mut self, mut tokens: VecDeque<SourceToken>) {
        tokens.append(&mut self.expansion_queue);
        self.expansion_queue = tokens;
    }

    /// Evaluates `?FILE` or `?LINE` internally when the caller has
    /// not shadowed them via `-define`.
    ///
    /// Returns `Some` when the name matches one of the built-ins and
    /// the synth source was queued; returns `None` for any other name
    /// so the caller can fall through to the caller-driven expansion
    /// event.
    fn try_internal_source_info(
        &mut self,
        name_text: &str,
        call_site: SourceSpan,
        parent_origin: &Arc<Origin>,
    ) -> Option<MacroCallOutcome> {
        let (kind, synth_text, display_name) = match name_text {
            "FILE" => {
                let outer = outermost_call_context(parent_origin, call_site);
                let outer_source = self.sources.get(outer.source_id);
                let escaped = escape_erlang_string(outer_source.display_name());
                let display = format!(
                    "<synth:?FILE at {}:{}>",
                    outer_source.display_name(),
                    outer.start.line()
                );
                (SourceInfoMacroKind::File, escaped, display)
            }
            "LINE" => {
                let outer = outermost_call_context(parent_origin, call_site);
                let outer_source = self.sources.get(outer.source_id);
                let synth = outer.start.line().to_string();
                let display = format!(
                    "<synth:?LINE at {}:{}>",
                    outer_source.display_name(),
                    outer.start.line()
                );
                (SourceInfoMacroKind::Line, synth, display)
            }
            _ => return None,
        };
        // Synthesize the source. Because the text was built from a
        // simple integer or a well-escaped string literal, the scan
        // is expected to succeed.
        let (source_arc, source_id) = synthesize_source(&self.sources, display_name, synth_text)
            .expect("synth text for ?FILE/?LINE always tokenizes");
        let mut expanded: VecDeque<SourceToken> =
            VecDeque::with_capacity(source_arc.tokens().len());
        for token in source_arc.tokens() {
            let origin = Origin::SourceInfo {
                parent: Arc::clone(parent_origin),
                call_site,
                kind,
            };
            expanded.push_back(SourceToken::new(
                *token,
                Arc::clone(&source_arc),
                source_id,
                origin,
            ));
        }
        self.prepend_to_queue(expanded);
        Some(MacroCallOutcome::Enqueued)
    }

    /// Attempts to rescan a `?NAME` macro call whose `?` sits at the
    /// head of the expansion queue.
    ///
    /// Handles both the constant-like shape (`?NAME`) and the
    /// function-like shape (`?NAME(...)`) when the call's opening
    /// `(` and its arguments all sit inside the queue. If the
    /// arguments straddle queue and source cursor, this returns
    /// `NotACall` and the `?` is emitted as a regular token
    /// (documented in `docs/otp-differences.md`).
    fn try_rescan_queue_call(&mut self) -> MacroCallOutcome {
        // Locate the next lexical token after the leading `?`,
        // skipping any hidden tokens in the queue.
        let name_idx = {
            let mut i = 1;
            loop {
                let Some(ppt) = self.expansion_queue.get(i) else {
                    return MacroCallOutcome::NotACall;
                };
                if ppt.token().kind().is_lexical() {
                    break i;
                }
                i += 1;
            }
        };
        let name_ppt = self.expansion_queue[name_idx].clone();
        let name_tok = *name_ppt.token();
        // `??` is stringification, handled elsewhere.
        if is_symbol(name_tok, erl_tokenize::Symbol::Question) {
            return MacroCallOutcome::NotACall;
        }
        if !matches!(
            name_tok.kind(),
            erl_tokenize::TokenKind::Atom | erl_tokenize::TokenKind::Variable
        ) {
            return MacroCallOutcome::NotACall;
        }

        // Look for the first lexical token after the name to decide
        // between constant-like and function-like. `(` inside the
        // queue makes it function-like; `(` sitting in the cursor
        // (arguments would straddle queue and cursor) is out of
        // scope for the queue-rescan path.
        let after_name_lex = {
            let mut i = name_idx + 1;
            let mut found = None;
            while let Some(ppt) = self.expansion_queue.get(i) {
                if ppt.token().kind().is_lexical() {
                    found = Some(*ppt.token());
                    break;
                }
                i += 1;
            }
            found
        };
        let is_function_like_in_queue =
            after_name_lex.is_some_and(|t| is_symbol(t, erl_tokenize::Symbol::OpenParen));
        // Ambiguous straddling case: the queue holds `?NAME` (plus
        // maybe hidden tokens) with no lexical follow-up, and the
        // source cursor immediately shows `(`. Bail — otherwise a
        // trailing `?FOO` inside a body would swallow the outer
        // `(...)`.
        if after_name_lex.is_none()
            && self
                .cursor_mut()
                .peek_lexical()
                .is_some_and(|t| is_symbol(t, erl_tokenize::Symbol::OpenParen))
        {
            return MacroCallOutcome::NotACall;
        }

        // Consume the `?`, any hidden tokens, and the name token from
        // the queue.
        let question_ppt = self
            .expansion_queue
            .pop_front()
            .expect("caller verified `?` was at the head");
        let name_start = name_tok.start();
        loop {
            let popped = self
                .expansion_queue
                .pop_front()
                .expect("name token is still queued");
            if popped.token().start() == name_start {
                break;
            }
        }

        let source_text = name_ppt.source().text();
        let name_text = match name_tok.value(source_text) {
            erl_tokenize::TokenValue::Atom(cow) => cow.into_owned(),
            erl_tokenize::TokenValue::Variable(name) => name.to_owned(),
            _ => {
                // Should not happen given the kind check; if it does,
                // re-emit `?` as a regular token by pushing it back.
                self.expansion_queue.push_front(name_ppt);
                self.expansion_queue.push_front(question_ppt);
                return MacroCallOutcome::NotACall;
            }
        };
        let question_span = question_ppt.source_span();
        let name_span = SourceSpan::new(
            name_ppt.source_span().source_id,
            name_tok.start(),
            name_tok.end(),
        );
        let name_ss = SourceString::new(name_text.clone(), name_span);
        let parent_origin = Arc::new(question_ppt.origin().clone());

        if !is_function_like_in_queue {
            let call_site =
                SourceSpan::new(question_span.source_id, question_span.start, name_tok.end());
            return self.finish_recognized_call(
                name_text,
                name_ss,
                None,
                call_site,
                Vec::new(),
                parent_origin,
            );
        }

        // Function-like: drop any hidden tokens between the name and
        // the opening `(`, then consume the `(` itself.
        while let Some(front) = self.expansion_queue.front() {
            let tok = *front.token();
            if is_symbol(tok, erl_tokenize::Symbol::OpenParen) {
                self.expansion_queue
                    .pop_front()
                    .expect("front peeked as `(`");
                break;
            }
            if tok.kind().is_lexical() {
                // Should not happen — `after_name_lex` established
                // that the next lexical is `(`.
                return MacroCallOutcome::NotACall;
            }
            self.expansion_queue
                .pop_front()
                .expect("front peeked as hidden");
        }

        let mut arg_source = QueueArgSource {
            queue: &mut self.expansion_queue,
        };
        let parsed = match parse_macro_arguments(&mut arg_source) {
            Ok(p) => p,
            Err(kind) => {
                let span =
                    SourceSpan::new(question_span.source_id, question_span.start, name_tok.end());
                return MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
                    kind.into_preprocess_error(span),
                )));
            }
        };
        let call_site = SourceSpan::new(
            question_span.source_id,
            question_span.start,
            parsed.close_end,
        );
        let arity = Some(parsed.arguments.len());
        self.finish_recognized_call(
            name_text,
            name_ss,
            arity,
            call_site,
            parsed.arguments,
            parent_origin,
        )
    }

    fn apply_macro_directive(&mut self, directive: Directive) -> Result<Event, PreprocessError> {
        match directive {
            Directive::Undef { name, span } => {
                self.macros.remove_all_by_name(name.as_str());
                Ok(Event::MacroUndefined(UndefinedMacro {
                    name,
                    directive_span: span,
                    parent_origin: Arc::clone(&self.current_origin),
                }))
            }
            define @ Directive::Define { .. } => {
                let source = Arc::clone(self.cursor_mut().source());
                let source_id = self.cursor_mut().source_id();
                let def = MacroDefinition::from_directive(
                    &define,
                    source,
                    source_id,
                    (*self.current_origin).clone(),
                )?;
                self.macros.insert(def.clone());
                Ok(Event::MacroDefined(def))
            }
            _ => unreachable!("only define/undef reach apply_macro_directive"),
        }
    }

    fn update_form_boundary_after_bump(&mut self, token: erl_tokenize::Token) {
        // A lexical `.` symbol ends the current form; the next
        // scan step should attempt directive recognition. Any
        // other lexical token puts us mid-form. Hidden tokens
        // (comments, whitespace) leave the flag unchanged so that a
        // run of hidden tokens between the last `.` and the next
        // form still counts as a form boundary.
        match token.kind() {
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Dot) => {
                self.at_form_boundary = true
            }
            kind if kind.is_lexical() => self.at_form_boundary = false,
            _ => {}
        }
    }
}

/// Shares [`SourceStore`]. Cursor, macro table, and branch stack
/// are independent. Isolation, not undo after an error.
impl Clone for Preprocessor {
    fn clone(&self) -> Self {
        Self {
            sources: Arc::clone(&self.sources),
            cursor: self.cursor.clone(),
            source_queue: self.source_queue.clone(),
            current_origin: Arc::clone(&self.current_origin),
            include_stack: self.include_stack.clone(),
            branch_stack: self.branch_stack.clone(),
            macros: self.macros.clone(),
            state: self.state.clone(),
            at_form_boundary: self.at_form_boundary,
            expansion_queue: self.expansion_queue.clone(),
            condition_collect: self.condition_collect.clone(),
        }
    }
}

/// Result of one step in the `step_scan` orchestrator loop.
///
/// Each step in the scan loop looks at a single concern (queue
/// rescan, queue drain, cursor EOF, directive parse, macro-call
/// recognition, cursor bump) and reports one of three actions back
/// to the loop: emit an event to the caller, restart the loop
/// because state was mutated but no event is ready yet, or fall
/// through so the next concern gets a turn.
enum StepAction {
    /// Return this event from `step_scan` to the caller. Boxed
    /// because `Event` is the largest branch of the enum, mirroring
    /// `MacroCallOutcome::Fire`.
    Emit(Box<Event>),
    /// State changed but no event is ready; the loop should restart
    /// from the first step.
    Retry,
    /// This step did nothing; the loop should proceed to the next
    /// step.
    Fall,
}

/// Outcome of one macro-call recognition attempt inside the scan loop.
enum MacroCallOutcome {
    /// The scanner recognized a call and produced an event
    /// (`Event::AwaitingMacroExpansion`) that needs to be returned
    /// immediately by the current `step`. Boxed because the event
    /// payload is the largest branch of the enum.
    Fire(Box<Event>),
    /// The scanner recognized a call and queued its expansion result;
    /// the surrounding loop should continue and let the queue drain.
    Enqueued,
    /// The cursor did not look like a macro call at all; the cursor
    /// has been restored so the caller can proceed with the normal
    /// bump path.
    NotACall,
}

/// Which side of a `-ifdef` / `-ifndef` OTP `epp` would pick given
/// the directive kind and whether the target macro is currently
/// defined.
fn recommended_defined_branch(kind: DefinedKind, defined: bool) -> Branch {
    match (kind, defined) {
        (DefinedKind::Ifdef, true) | (DefinedKind::Ifndef, false) => Branch::Then,
        (DefinedKind::Ifdef, false) | (DefinedKind::Ifndef, true) => Branch::Else,
    }
}

fn is_symbol(token: erl_tokenize::Token, sym: erl_tokenize::Symbol) -> bool {
    matches!(token.kind(), erl_tokenize::TokenKind::Symbol(s) if s == sym)
}

/// Scans `text` and appends the resulting immutable [`Source`] to
/// `sources`, returning both the shared handle and the newly issued
/// [`SourceId`].
///
/// Used to materialise the small pseudo sources that hold the synth
/// text of `?FILE` / `?LINE` (and, in later phases, `??Param`).
fn synthesize_source(
    sources: &Arc<SourceStore>,
    display_name: String,
    text: String,
) -> Result<(Arc<Source>, SourceId), erl_tokenize::Error> {
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(token) = erl_tokenize::scan_token(&text, position)? {
        position = token.end();
        tokens.push(token);
    }
    let source = Source::new(display_name, text, tokens);
    let source_id = sources.append(source);
    let source_arc = sources.get(source_id);
    Ok((source_arc, source_id))
}

/// Walks the origin chain to find the outermost non-macro
/// [`SourceSpan`] — the position in the top-level (or included)
/// source where the currently expanding macro was actually invoked
/// by the user, matching OTP's "annotation of the call site" rule
/// for `?LINE` and `?FILE`.
fn outermost_call_context(origin: &Origin, current_call_site: SourceSpan) -> SourceSpan {
    match origin {
        Origin::Source | Origin::Include { .. } => current_call_site,
        Origin::MacroBody {
            parent, call_site, ..
        }
        | Origin::MacroArgument {
            parent, call_site, ..
        }
        | Origin::Stringification {
            parent, call_site, ..
        }
        | Origin::SourceInfo {
            parent, call_site, ..
        }
        | Origin::CallerExpansion {
            parent, call_site, ..
        } => outermost_call_context(parent, *call_site),
    }
}

/// Escapes `s` as an Erlang double-quoted string literal, quoting the
/// value and backslash-escaping characters that the tokenizer would
/// otherwise refuse to parse (`"`, `\`, and the common control
/// characters).
fn escape_erlang_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Builds a [`MacroCallOutcome::Fire`] carrying a
/// [`PreprocessError::CircularExpansion`].
fn fire_circular(
    name: String,
    arity: Option<usize>,
    call_site: SourceSpan,
    chain: Vec<(String, Option<usize>)>,
) -> MacroCallOutcome {
    MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
        PreprocessError::CircularExpansion {
            span: call_site,
            name,
            arity,
            chain,
        },
    )))
}

/// Walks a caller-driven expansion chain via [`Origin::CallerExpansion`]
/// entries, looking for the same macro name already in progress. On a
/// hit, returns the chain from the outer occurrence to the current
/// name. Only names are compared; the fired chain uses `None` for
/// arity because Origin::CallerExpansion does not carry it.
fn collect_caller_ancestor_cycle(
    origin: &Origin,
    target_name: &str,
) -> Option<Vec<(String, Option<usize>)>> {
    let mut ancestors: Vec<String> = Vec::new();
    let mut cur: &Origin = origin;
    loop {
        match cur {
            Origin::CallerExpansion { parent, name, .. } => {
                ancestors.push(name.value.clone());
                cur = parent;
            }
            Origin::MacroBody { parent, .. }
            | Origin::MacroArgument { parent, .. }
            | Origin::Stringification { parent, .. }
            | Origin::SourceInfo { parent, .. } => cur = parent,
            Origin::Include { parent, .. } => cur = parent,
            Origin::Source => break,
        }
    }
    if ancestors.iter().any(|n| n == target_name) {
        // Ancestors are outermost-first? We pushed as we descended
        // toward Origin::Source (deepest-first), so reverse to get
        // outermost-first for a readable chain.
        ancestors.reverse();
        let mut chain: Vec<(String, Option<usize>)> =
            ancestors.into_iter().map(|n| (n, None)).collect();
        chain.push((target_name.to_owned(), None));
        Some(chain)
    } else {
        None
    }
}

/// Builds the expansion of a constant-like macro definition.
///
/// Every replacement token is re-emitted with a fresh
/// [`Origin::MacroBody`] whose `call_site` points at the call and
/// whose `definition_span` points at the `-define(...)` directive.
fn expand_constant_like(
    def: &MacroDefinition,
    call_site: SourceSpan,
    parent_origin: &Arc<Origin>,
) -> VecDeque<SourceToken> {
    let definition_span = def.directive_span;
    let mut out: VecDeque<SourceToken> = VecDeque::with_capacity(def.replacement.len());
    for replacement in &def.replacement {
        let origin = Origin::MacroBody {
            parent: Arc::clone(parent_origin),
            call_site,
            definition_span,
        };
        let token = *replacement.token();
        let source = Arc::clone(replacement.source());
        let source_id = replacement.source_span().source_id;
        out.push_back(SourceToken::new(token, source, source_id, origin));
    }
    out
}

/// Builds the expansion of a function-like macro definition, doing
/// OTP-style parameter substitution: whenever a replacement token is
/// a `Variable` whose text matches a formal parameter name, its
/// occurrences are replaced with the tokens of the matching
/// argument.
///
/// Replacement tokens keep their definition source and become
/// [`Origin::MacroBody`]; argument tokens keep their argument source
/// and become [`Origin::MacroArgument`] tagged with the parameter
/// they were bound to.
fn expand_function_like(
    def: &MacroDefinition,
    arguments: &[Vec<SourceToken>],
    call_site: SourceSpan,
    parent_origin: &Arc<Origin>,
    sources: &Arc<SourceStore>,
) -> Result<VecDeque<SourceToken>, MacroCallErrorKind> {
    let definition_span = def.directive_span;
    let mut out: VecDeque<SourceToken> = VecDeque::new();
    let repl = &def.replacement;
    let mut i = 0;
    while i < repl.len() {
        let token = *repl[i].token();

        // Recognize `??Param` before falling into the normal
        // parameter-substitution path. The pattern is `?` + hidden* +
        // `?` + hidden* + Variable-that-matches-a-parameter.
        if is_symbol(token, erl_tokenize::Symbol::Question)
            && let Some(next_idx) = find_next_lexical_index(repl, i + 1)
            && is_symbol(*repl[next_idx].token(), erl_tokenize::Symbol::Question)
        {
            let target_idx = match find_next_lexical_index(repl, next_idx + 1) {
                Some(idx) => idx,
                None => {
                    return Err(MacroCallErrorKind::InvalidStringificationTarget {
                        span: repl[next_idx].source_span(),
                    });
                }
            };
            let target_ppt = &repl[target_idx];
            let target_tok = *target_ppt.token();
            if target_tok.kind() != erl_tokenize::TokenKind::Variable {
                return Err(MacroCallErrorKind::InvalidStringificationTarget {
                    span: target_ppt.source_span(),
                });
            }
            let var_text = target_tok.text(target_ppt.source().text());
            let Some(param_idx) = def.params.iter().position(|p| p.as_str() == var_text) else {
                return Err(MacroCallErrorKind::InvalidStringificationTarget {
                    span: target_ppt.source_span(),
                });
            };
            let parameter = def.params[param_idx].clone();
            let argument = &arguments[param_idx];
            let synth_tokens = stringify_argument(
                argument,
                sources,
                call_site,
                parameter,
                definition_span,
                parent_origin,
            );
            for t in synth_tokens {
                out.push_back(t);
            }
            i = target_idx + 1;
            continue;
        }

        if token.kind() == erl_tokenize::TokenKind::Variable {
            let var_text = token.text(repl[i].source().text());
            if let Some(idx) = def.params.iter().position(|p| p.as_str() == var_text)
                && idx < arguments.len()
            {
                let parameter = def.params[idx].clone();
                for arg_tok in &arguments[idx] {
                    let origin = Origin::MacroArgument {
                        parent: Arc::clone(parent_origin),
                        call_site,
                        parameter: parameter.clone(),
                        definition_span,
                    };
                    let arg_token = *arg_tok.token();
                    let source = Arc::clone(arg_tok.source());
                    let source_id = arg_tok.source_span().source_id;
                    out.push_back(SourceToken::new(arg_token, source, source_id, origin));
                }
                i += 1;
                continue;
            }
        }
        let origin = Origin::MacroBody {
            parent: Arc::clone(parent_origin),
            call_site,
            definition_span,
        };
        let source = Arc::clone(repl[i].source());
        let source_id = repl[i].source_span().source_id;
        out.push_back(SourceToken::new(token, source, source_id, origin));
        i += 1;
    }
    Ok(out)
}

/// Emits `ppt` as [`Event::Token`] when it is lexical; otherwise
/// retries so whitespace and comments never surface on the event
/// stream. The caller already holds those tokens in [`Source`].
fn emit_lexical_token(ppt: SourceToken) -> StepAction {
    if ppt.token().kind().is_lexical() {
        StepAction::Emit(Box::new(Event::Token(ppt)))
    } else {
        StepAction::Retry
    }
}

/// Returns the index of the next lexical token in `tokens` at or
/// after `start`, or `None` if none is found before the slice runs
/// out.
fn find_next_lexical_index(tokens: &[SourceToken], start: usize) -> Option<usize> {
    (start..tokens.len()).find(|&i| tokens[i].token().kind().is_lexical())
}

/// Materialises a stringified argument as a single Erlang string
/// literal token, matching the OTP `epp:stringify/2` shape: keep only
/// lexical tokens, print each with a per-kind formatter, and join
/// them with a single space.
fn stringify_argument(
    argument: &[SourceToken],
    sources: &Arc<SourceStore>,
    call_site: SourceSpan,
    parameter: SourceString,
    definition_span: SourceSpan,
    parent_origin: &Arc<Origin>,
) -> Vec<SourceToken> {
    let parts: Vec<String> = argument
        .iter()
        .filter(|ppt| ppt.token().kind().is_lexical())
        .map(stringify_token_text)
        .collect();
    let joined = parts.join(" ");
    let synth_text = escape_erlang_string(&joined);
    let display_name = format!("<synth:??{} at {}:{}>", parameter.as_str(), "?", "?");
    let (source_arc, source_id) = synthesize_source(sources, display_name, synth_text)
        .expect("synth text for ??Param always tokenizes as a string literal");
    source_arc
        .tokens()
        .iter()
        .map(|t| {
            SourceToken::new(
                *t,
                Arc::clone(&source_arc),
                source_id,
                Origin::Stringification {
                    parent: Arc::clone(parent_origin),
                    call_site,
                    parameter: parameter.clone(),
                    definition_span,
                },
            )
        })
        .collect()
}

/// Formats a single argument token into the text form that OTP's
/// `token_src/1` produces for `stringify_1/1`.
fn stringify_token_text(ppt: &SourceToken) -> String {
    let token = *ppt.token();
    let source_text = ppt.source().text();
    // Integer / Float use the decoded value when the tokenizer
    // produced one; other kinds pass through the source text.
    match token.value(source_text) {
        erl_tokenize::TokenValue::Integer(Some(n)) => n.to_string(),
        erl_tokenize::TokenValue::Float(f) => {
            let s = f.to_string();
            if s.contains('.') || s.contains('e') || s.contains('E') {
                s
            } else {
                format!("{s}.0")
            }
        }
        _ => token.text(source_text).to_owned(),
    }
}

/// Delimiter stack entry used while parsing macro-call arguments.
///
/// Each entry names the token that closes the enclosing bracket or
/// keyword block. `FunEnd` is the OTP-style sentinel for `fun ...`
/// expressions and types whose closer depends on later tokens: a
/// `when` or `->` promotes it to `End`, while an outer `)` / `,`
/// drains it as a terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delimiter {
    CloseParen,
    CloseSquare,
    CloseBrace,
    CloseDoubleAngle,
    End,
    FunEnd,
}

/// Result of one macro-argument-parse attempt.
struct ParsedArguments {
    /// Per-argument token streams. Empty when the call was `()` (arity 0).
    arguments: Vec<Vec<SourceToken>>,
    /// erl_tokenize::Position of the byte after the closing `)`. Used together with
    /// the call's opening `?` to build the call-site span.
    close_end: erl_tokenize::Position,
}

/// A token producer that [`parse_macro_arguments`] pulls tokens from.
///
/// Abstracts over the two argument-parse call sites: the source
/// cursor (initial recognition, tokens get [`Origin::Source`]) and
/// the expansion queue (function-like rescan, tokens carry the
/// Origin the earlier expansion assigned them).
trait ArgTokenSource {
    /// Returns the next token without consuming it, `None` at end.
    fn peek(&self) -> Option<erl_tokenize::Token>;

    /// Consumes the next token and returns it wrapped as a
    /// [`SourceToken`], `None` at end.
    fn bump(&mut self) -> Option<SourceToken>;
}

/// [`ArgTokenSource`] adapter that pulls from a raw source cursor,
/// wrapping each token with the caller's current origin (which is
/// `Origin::Source` at the top level and `Origin::Include { ... }`
/// inside an include source).
struct CursorArgSource<'a> {
    cursor: &'a mut Cursor,
    source_id: SourceId,
    origin: &'a Arc<Origin>,
}

impl ArgTokenSource for CursorArgSource<'_> {
    fn peek(&self) -> Option<erl_tokenize::Token> {
        self.cursor.peek()
    }

    fn bump(&mut self) -> Option<SourceToken> {
        let token = self.cursor.bump()?;
        Some(SourceToken::new(
            token,
            Arc::clone(self.cursor.source()),
            self.source_id,
            (**self.origin).clone(),
        ))
    }
}

/// [`ArgTokenSource`] adapter that pulls from the front of the
/// expansion queue, keeping each token's existing Origin intact.
struct QueueArgSource<'a> {
    queue: &'a mut VecDeque<SourceToken>,
}

impl ArgTokenSource for QueueArgSource<'_> {
    fn peek(&self) -> Option<erl_tokenize::Token> {
        self.queue.front().map(|ppt| *ppt.token())
    }

    fn bump(&mut self) -> Option<SourceToken> {
        self.queue.pop_front()
    }
}

/// Parses macro-call arguments starting immediately after the opening
/// `(`.
///
/// Tracks a small delimiter stack so top-level `,` and `)` are only
/// recognized at the outermost bracket depth. Follows OTP `epp:macro_arg`:
/// leading empty (`?NAME(, ...)`) and trailing empty (`?NAME(..., )`)
/// arguments are rejected as errors, middle empties (`?NAME(A, , B)`)
/// are valid arity-`N` groups. Hidden tokens (whitespace and comments)
/// are preserved inside arguments that contain lexical tokens, but a
/// hidden-only group between `(` and `)` is arity 0 (`?NAME(   )`
/// matches `?NAME()`).
///
/// Generic over [`ArgTokenSource`] so the same delimiter-tracking
/// logic serves both the source-cursor call site (initial recognition)
/// and the expansion-queue call site (rescan of a previous
/// expansion's body).
fn parse_macro_arguments<S: ArgTokenSource>(
    source: &mut S,
) -> Result<ParsedArguments, MacroCallErrorKind> {
    let mut arguments: Vec<Vec<SourceToken>> = Vec::new();
    let mut current: Vec<SourceToken> = Vec::new();
    let mut current_has_lexical = false;
    let mut stack: Vec<Delimiter> = Vec::new();
    // `true` while we have not yet seen any lexical token or comma.
    let mut before_first_content = true;

    loop {
        let Some(token) = source.peek() else {
            return Err(MacroCallErrorKind::UnclosedArgument);
        };

        // A `,` or `)` counts as top-level once every remaining
        // delimiter is a `FunEnd` sentinel; the sentinels get drained
        // when the enclosing group ends, matching OTP's re-drive.
        let effective_top = stack.iter().all(|d| *d == Delimiter::FunEnd);

        if effective_top && token.kind().is_lexical() {
            match token.kind() {
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Comma) => {
                    source.bump();
                    stack.clear();
                    if before_first_content {
                        return Err(MacroCallErrorKind::LeadingEmptyArgument);
                    }
                    arguments.push(std::mem::take(&mut current));
                    current_has_lexical = false;
                    before_first_content = false;
                    continue;
                }
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::CloseParen) => {
                    let close_end = token.end();
                    source.bump();
                    stack.clear();
                    if before_first_content && !current_has_lexical {
                        // `?NAME()` and `?NAME(   )` are both arity 0.
                        // Hidden tokens between the parentheses are
                        // not an argument; OTP epp never sees them
                        // because `erl_scan` has already dropped
                        // whitespace and comments.
                    } else if !current_has_lexical {
                        return Err(MacroCallErrorKind::TrailingEmptyArgument);
                    } else {
                        arguments.push(current);
                    }
                    return Ok(ParsedArguments {
                        arguments,
                        close_end,
                    });
                }
                _ => {}
            }
        }

        let ppt = source.bump().expect("peek returned Some, so bump must too");
        current.push(ppt);
        if token.kind().is_lexical() {
            current_has_lexical = true;
            before_first_content = false;
        }

        match token.kind() {
            erl_tokenize::TokenKind::Symbol(sym) => match sym {
                erl_tokenize::Symbol::OpenParen => stack.push(Delimiter::CloseParen),
                erl_tokenize::Symbol::OpenSquare => stack.push(Delimiter::CloseSquare),
                erl_tokenize::Symbol::OpenBrace => stack.push(Delimiter::CloseBrace),
                erl_tokenize::Symbol::DoubleLeftAngle => stack.push(Delimiter::CloseDoubleAngle),
                erl_tokenize::Symbol::CloseParen => {
                    pop_fun_ends(&mut stack);
                    if stack.last() == Some(&Delimiter::CloseParen) {
                        stack.pop();
                    }
                }
                erl_tokenize::Symbol::CloseSquare => {
                    pop_fun_ends(&mut stack);
                    if stack.last() == Some(&Delimiter::CloseSquare) {
                        stack.pop();
                    }
                }
                erl_tokenize::Symbol::CloseBrace => {
                    pop_fun_ends(&mut stack);
                    if stack.last() == Some(&Delimiter::CloseBrace) {
                        stack.pop();
                    }
                }
                erl_tokenize::Symbol::DoubleRightAngle => {
                    pop_fun_ends(&mut stack);
                    if stack.last() == Some(&Delimiter::CloseDoubleAngle) {
                        stack.pop();
                    }
                }
                erl_tokenize::Symbol::RightArrow => promote_fun_end_to_end(&mut stack),
                _ => {}
            },
            erl_tokenize::TokenKind::Keyword(kw) => match kw {
                erl_tokenize::Keyword::Begin
                | erl_tokenize::Keyword::If
                | erl_tokenize::Keyword::Case
                | erl_tokenize::Keyword::Maybe
                | erl_tokenize::Keyword::Receive
                | erl_tokenize::Keyword::Try
                | erl_tokenize::Keyword::Cond => stack.push(Delimiter::End),
                erl_tokenize::Keyword::Fun => stack.push(Delimiter::FunEnd),
                erl_tokenize::Keyword::End => {
                    if stack.last() == Some(&Delimiter::End)
                        || stack.last() == Some(&Delimiter::FunEnd)
                    {
                        stack.pop();
                    }
                }
                erl_tokenize::Keyword::When => promote_fun_end_to_end(&mut stack),
                _ => {}
            },
            _ => {}
        }
    }
}

fn pop_fun_ends(stack: &mut Vec<Delimiter>) {
    while stack.last() == Some(&Delimiter::FunEnd) {
        stack.pop();
    }
}

fn promote_fun_end_to_end(stack: &mut [Delimiter]) {
    if let Some(top) = stack.last_mut()
        && *top == Delimiter::FunEnd
    {
        *top = Delimiter::End;
    }
}

impl std::fmt::Debug for Preprocessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preprocessor")
            .field("sources_len", &self.sources.len())
            .field("include_stack_depth", &self.include_stack.len())
            .field("source_queue_len", &self.source_queue.len())
            .field("macros_len", &self.macros.len())
            .field("state", &self.state)
            .field("at_form_boundary", &self.at_form_boundary)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::assert_matches;

    use crate::error::PreprocessError;

    fn make(text: &str) -> Preprocessor {
        Preprocessor::new([
            Source::from_text("main.erl", text).expect("test input must scan without lex errors")
        ])
    }

    fn define_source(text: &str) -> Source {
        Source::from_text("<initial macro>", text).expect("test input must scan without lex errors")
    }

    fn drain(pp: &mut Preprocessor) -> Vec<Event> {
        let mut events = Vec::new();
        loop {
            let event = pp.step().expect("no protocol errors");
            let is_complete = matches!(event, Event::Complete);
            events.push(event);
            if is_complete {
                break;
            }
        }
        events
    }

    #[test]
    fn empty_source_returns_complete() {
        let mut pp = make("");
        let events = drain(&mut pp);
        assert_eq!(events.len(), 1);
        assert_matches!(events[0], Event::Complete);
    }

    #[test]
    fn complete_is_idempotent() {
        let mut pp = make("");
        assert_matches!(pp.step().expect("no protocol error"), Event::Complete);
        assert_matches!(pp.step().expect("no protocol error"), Event::Complete);
        assert_matches!(pp.step().expect("no protocol error"), Event::Complete);
    }

    #[test]
    fn resume_macro_expansion_while_scanning_is_unexpected_response() {
        let mut pp = make("foo");
        let response =
            Source::from_text("<synth:test>", "").expect("test input must scan without lex errors");
        assert_eq!(
            pp.resume_macro_expansion(response)
                .expect_err("protocol error expected"),
            ProtocolError
        );
    }

    #[test]
    fn resume_macro_expansion_while_completed_is_unexpected_response() {
        let mut pp = make("");
        // Drain to Completed.
        drain(&mut pp);
        let response =
            Source::from_text("<synth:test>", "").expect("test input must scan without lex errors");
        assert_eq!(
            pp.resume_macro_expansion(response)
                .expect_err("protocol error expected"),
            ProtocolError
        );
    }

    #[test]
    fn constant_like_call_with_table_hit_expands_from_body() {
        let mut pp = make("-define(FOO, bar).\n?FOO.");
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) => texts.push(ppt.text().to_owned()),
                Event::MacroDefined(_) => {}
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        // Whitespace, the expanded `bar`, then the trailing dot.
        assert!(texts.contains(&"bar".to_owned()));
        assert!(texts.contains(&".".to_owned()));
    }

    #[test]
    fn constant_like_expanded_token_carries_macro_body_origin() {
        let mut pp = make("-define(FOO, bar).\n?FOO.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "bar" => {
                    let Origin::MacroBody { call_site, .. } = ppt.origin() else {
                        panic!("expected MacroBody origin, got {:?}", ppt.origin());
                    };
                    // Call site points at the `?FOO` in the source.
                    assert!(call_site.start.offset() < call_site.end.offset());
                    return;
                }
                Event::Complete => panic!("bar token never emitted"),
                _ => {}
            }
        }
    }

    #[test]
    fn constant_like_call_without_table_fires_awaiting_event() {
        let mut pp = make("?UNKNOWN.");
        let event = pp.step().expect("no protocol errors");
        let call = match event {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        assert_eq!(call.name.as_str(), "UNKNOWN");
        assert_eq!(call.arity, None);
        assert!(call.arguments.is_empty());
    }

    #[test]
    fn resume_macro_expansion_enqueues_response_tokens() {
        let mut pp = make("?UNKNOWN.");
        let call = match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        let response = Source::from_text("<synth:UNKNOWN>", "bar")
            .expect("test input must scan without lex errors");
        pp.resume_macro_expansion(response).expect("resume accepts");
        // The response token surfaces before the trailing dot.
        let ppt = match pp.step().expect("no protocol errors") {
            Event::Token(t) => t,
            other => panic!("expected token from response, got {other:?}"),
        };
        assert_eq!(ppt.text(), "bar");
        let Origin::CallerExpansion {
            call_site: origin_call_site,
            name,
            ..
        } = ppt.origin()
        else {
            panic!("expected CallerExpansion origin, got {:?}", ppt.origin());
        };
        assert_eq!(*origin_call_site, call.call_site);
        assert_eq!(name.as_str(), "UNKNOWN");
    }

    #[test]
    fn resume_macro_expansion_with_empty_source_skips_call() {
        let mut pp = make("?UNKNOWN.");
        let _call = match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        let empty_response = Source::from_text("<synth:UNKNOWN>", "")
            .expect("test input must scan without lex errors");
        pp.resume_macro_expansion(empty_response)
            .expect("resume accepts");
        // Next token is the trailing dot, no error event surfaces.
        let ppt = match pp.step().expect("no protocol errors") {
            Event::Token(t) => t,
            other => panic!("expected token, got {other:?}"),
        };
        assert_eq!(ppt.text(), ".");
    }

    #[test]
    fn resume_nested_caller_expansion_splices_in_place() {
        // `?FOO` expands to `{?BAR}`. The caller fills `?BAR` with
        // `x`. The `{` is already on the expansion queue when BAR is
        // awaited; the response must land between `{` and `}`, not
        // after the closing `}`.
        let mut pp = make("-define(FOO, {?BAR}).\n?FOO.");
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol errors") {
                Event::AwaitingMacroExpansion(req) => {
                    assert_eq!(req.name.as_str(), "BAR");
                    pp.resume_macro_expansion(
                        Source::from_text("<synth:BAR>", "x")
                            .expect("test input must scan without lex errors"),
                    )
                    .expect("resume accepts");
                }
                Event::Token(t) => texts.push(t.text().to_string()),
                Event::Complete => break,
                Event::MacroDefined(_) => {}
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(texts, ["{", "x", "}", "."]);
    }

    #[test]
    fn step_while_awaiting_macro_expansion_is_protocol_error() {
        let mut pp = make("?UNKNOWN.");
        pp.step().expect("first step yields Awaiting event");
        assert_eq!(
            pp.step().expect_err("second step is a protocol error"),
            ProtocolError
        );
    }

    fn expect_awaiting(pp: &mut Preprocessor) -> MacroCall {
        match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        }
    }

    fn expect_preprocess_error(pp: &mut Preprocessor) -> PreprocessError {
        match pp.step().expect("no protocol errors") {
            Event::PreprocessError(err) => err,
            other => panic!("expected PreprocessError, got {other:?}"),
        }
    }

    #[test]
    fn function_like_call_arity_zero() {
        let mut pp = make("?FOO().");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.name.as_str(), "FOO");
        assert_eq!(call.arity, Some(0));
        assert!(call.arguments.is_empty());
    }

    #[test]
    fn function_like_call_whitespace_only_parens_is_arity_zero() {
        let mut pp = make("?FOO(   ).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.name.as_str(), "FOO");
        assert_eq!(call.arity, Some(0));
        assert!(call.arguments.is_empty());
    }

    #[test]
    fn function_like_call_single_argument() {
        let mut pp = make("?FOO(bar).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(1));
        assert_eq!(call.arguments.len(), 1);
        let arg_texts: Vec<_> = call.arguments[0]
            .iter()
            .map(|t| t.text().to_owned())
            .collect();
        assert!(arg_texts.contains(&"bar".to_owned()));
    }

    #[test]
    fn function_like_call_multiple_arguments() {
        let mut pp = make("?FOO(a, b, c).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(3));
        let names: Vec<_> = call
            .arguments
            .iter()
            .map(|arg| {
                arg.iter()
                    .find(|t| t.token().kind().is_lexical())
                    .expect("each arg has at least one lexical token")
                    .text()
                    .to_owned()
            })
            .collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[test]
    fn function_like_call_nested_parens_do_not_split_arguments() {
        let mut pp = make("?FOO((a, b), c).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(2));
    }

    #[test]
    fn function_like_call_nested_brackets_and_braces() {
        let mut pp = make("?FOO([a, b], {c, d}, <<1, 2>>).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(3));
    }

    #[test]
    fn function_like_call_middle_empty_is_valid() {
        let mut pp = make("?FOO(a, , b).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(3));
        // The middle argument has no lexical token, only hidden or empty.
        let middle_has_lexical = call.arguments[1]
            .iter()
            .any(|t| t.token().kind().is_lexical());
        assert!(!middle_has_lexical);
    }

    #[test]
    fn function_like_call_leading_empty_is_error() {
        let mut pp = make("?FOO(, a).");
        let err = expect_preprocess_error(&mut pp);
        assert_matches!(err, PreprocessError::LeadingEmptyArgument { .. });
    }

    #[test]
    fn function_like_call_trailing_empty_is_error() {
        let mut pp = make("?FOO(a, ).");
        let err = expect_preprocess_error(&mut pp);
        assert_matches!(err, PreprocessError::TrailingEmptyArgument { .. });
    }

    #[test]
    fn function_like_call_unclosed_argument_is_error() {
        let mut pp = make("?FOO(a, b");
        let err = expect_preprocess_error(&mut pp);
        assert_matches!(err, PreprocessError::UnclosedArgument { .. });
    }

    #[test]
    fn function_like_call_keyword_block_balancing() {
        let mut pp = make("?FOO(case X of Y -> Z end, W).");
        let call = expect_awaiting(&mut pp);
        // The commas inside case ... end are not top-level.
        assert_eq!(call.arity, Some(2));
    }

    #[test]
    fn function_like_call_fun_expression() {
        let mut pp = make("?FOO(fun() -> ok end, a).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(2));
    }

    #[test]
    fn function_like_call_fun_type_syntax() {
        // `fun((atom()) -> ok)` ends with `)`, not `end`; the FunEnd
        // sentinel must drain when the outer macro `)` is reached.
        let mut pp = make("?FOO(fun((atom()) -> ok), b).");
        let call = expect_awaiting(&mut pp);
        assert_eq!(call.arity, Some(2));
    }

    #[test]
    fn tokens_are_streamed_in_order() {
        let mut pp = make("foo bar");
        let mut streamed = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(ppt) => streamed.push(ppt),
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        // Whitespace between `foo` and `bar` is not re-emitted.
        let texts: Vec<&str> = streamed.iter().map(|t| t.text()).collect();
        assert_eq!(texts, ["foo", "bar"]);
        assert!(streamed.iter().all(|t| t.token().kind().is_lexical()));
        assert_matches!(streamed[0].origin(), crate::origin::Origin::Source);
    }

    #[test]
    fn unknown_attribute_yields_raw_tokens() {
        // `-module(m).` is not one of the recognised preprocessor
        // directives; parse_directive rolls back and the tokens flow
        // out as regular tokens.
        let mut pp = make("-module(m).");
        let mut kinds = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(ppt) => kinds.push(ppt.token().kind()),
                Event::Complete => break,
                Event::MacroDefined(_) | Event::MacroUndefined(_) => {
                    panic!("should not recognise -module")
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(!kinds.is_empty());
        let has_hyphen = kinds.iter().any(|k| {
            matches!(
                k,
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Hyphen)
            )
        });
        let has_dot = kinds.iter().any(|k| {
            matches!(
                k,
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Dot)
            )
        });
        assert!(has_hyphen && has_dot);
    }

    #[test]
    fn recognised_directive_becomes_event() {
        let mut pp = make("-undef(foo).");
        let first = pp.step().expect("no protocol error");
        let Event::MacroUndefined(undef) = first else {
            panic!("expected MacroUndefined, got {first:?}");
        };
        assert_eq!(undef.name.as_str(), "foo");
        assert!(pp.macros().is_empty());
        // Directive tokens are consumed by the parser, not streamed.
        let complete = pp.step().expect("no protocol error");
        assert_matches!(complete, Event::Complete);
    }

    #[test]
    fn mixed_forms_stream_correctly() {
        let mut pp = make("foo.-undef(foo).bar.");
        let mut description = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(ppt) => description.push(format!("token:{}", ppt.text())),
                Event::MacroUndefined(_) => description.push("directive:undef".into()),
                Event::Complete => {
                    description.push("complete".into());
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(description.iter().any(|s| s == "token:foo"));
        assert!(description.iter().any(|s| s == "token:bar"));
        assert!(description.iter().any(|s| s == "directive:undef"));
        assert!(description.last().expect("at least one description") == "complete");
    }

    #[test]
    fn clone_shares_store_and_advances_independently() {
        // Take the first token from pp, then fork. pp and fork share
        // the SourceStore but their cursors advance independently.
        let mut pp = make("foo bar");
        let first = pp.step().expect("no protocol error");
        assert_matches!(first, Event::Token(_));

        let mut fork = pp.clone();
        assert!(Arc::ptr_eq(pp.sources(), fork.sources()));

        let pp_tokens = collect_token_texts(&mut pp);
        let fork_tokens = collect_token_texts(&mut fork);

        // pp already emitted `foo`; the space is not re-emitted, so
        // the remainder is `bar`.
        assert_eq!(pp_tokens, ["bar"]);
        // The fork resumes from the same cursor position pp had at
        // clone time.
        assert_eq!(fork_tokens, ["bar"]);
    }

    fn collect_token_texts(pp: &mut Preprocessor) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(ppt) => out.push(ppt.text().to_string()),
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        out
    }

    #[test]
    fn define_directive_updates_macro_table_before_event() {
        let mut pp = make("-define(FOO, 1).");
        assert!(pp.macros().is_empty());
        let event = pp.step().expect("no protocol error");
        let Event::MacroDefined(def) = event else {
            panic!("expected MacroDefined, got {event:?}");
        };
        assert_eq!(def.name, "FOO");
        // State-then-event contract: when the caller observes the
        // event, the macro table already contains the definition.
        let table = pp.macros().get_constant("FOO").expect("defined");
        assert_eq!(table.name, def.name);
        assert_eq!(table.arity, def.arity);
        assert_eq!(pp.macros().len(), 1);
    }

    #[test]
    fn undef_directive_removes_macros_before_event() {
        let mut pp = make("-define(FOO, 1).\n-define(FOO(A), A).\n-undef(FOO).");
        // Drain define/undef; the table should end empty.
        loop {
            match pp.step().expect("no protocol error") {
                Event::MacroDefined(_) => {}
                Event::MacroUndefined(_) => {
                    // At the moment we observe the Undef event, all
                    // FOO entries are already gone.
                    assert!(pp.macros().is_empty());
                }
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(pp.macros().is_empty());
    }

    #[test]
    fn constant_like_and_arity_0_coexist_in_table() {
        let mut pp = make("-define(FOO, 1).\n-define(FOO(), 2).");
        drain(&mut pp);
        assert!(pp.macros().get_constant("FOO").is_some());
        assert!(pp.macros().get_function("FOO", 0).is_some());
        assert_eq!(pp.macros().len(), 2);
    }

    #[test]
    fn duplicate_parameter_surfaces_as_preprocess_error() {
        let mut pp = make("-define(BAD(A, A), A).");
        let event = pp.step().expect("no protocol error");
        assert_matches!(
            event,
            Event::PreprocessError(PreprocessError::DuplicateParameter { .. })
        );
        // The failing definition is not added to the table.
        assert!(pp.macros().is_empty());
    }

    #[test]
    fn empty_sequence_returns_complete() {
        let mut pp = Preprocessor::new([]);
        let events = drain(&mut pp);
        assert_eq!(events.len(), 1);
        assert_matches!(events[0], Event::Complete);
    }

    #[test]
    fn source_sequence_scans_in_order_and_carries_macros() {
        let mut pp = Preprocessor::new([
            define_source("-define(FOO, 1)."),
            Source::from_text("main.erl", "?FOO.")
                .expect("test input must scan without lex errors"),
        ]);
        assert!(pp.macros().is_empty());
        let mut saw_defined = false;
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::MacroDefined(def) => {
                    assert_eq!(def.name, "FOO");
                    assert!(pp.macros().get_constant("FOO").is_some());
                    saw_defined = true;
                }
                Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                    texts.push(ppt.text().to_owned());
                }
                Event::Token(_) => {}
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_defined);
        assert!(texts.contains(&"1".to_owned()));
    }

    #[test]
    fn leading_define_uses_source_origin() {
        let mut pp = Preprocessor::new([define_source("-define(FOO, 1).")]);
        match pp.step().expect("no protocol error") {
            Event::MacroDefined(_) => {
                let def = pp.macros().get_constant("FOO").expect("defined");
                assert_matches!(def.origin, Origin::Source);
            }
            other => panic!("expected MacroDefined, got {other:?}"),
        }
    }

    #[test]
    fn broken_leading_source_continues_to_next() {
        let mut pp = Preprocessor::new([
            define_source("-endif."),
            Source::from_text("main.erl", "ok.").expect("test input must scan without lex errors"),
        ]);
        let mut saw_error = false;
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::PreprocessError(PreprocessError::StrayEndif { .. }) => {
                    saw_error = true;
                }
                Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                    texts.push(ppt.text().to_owned());
                }
                Event::Token(_) => {}
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(saw_error);
        assert_eq!(texts, ["ok", "."]);
    }

    // ---- Phase 7: rescan --------------------------------------------

    #[test]
    fn rescan_expands_nested_constant_macro_body() {
        // ?FOO expands to the tokens of `?BAR`, which must then be
        // rescanned and expanded to `1`.
        let mut pp = make("-define(BAR, 1).\n-define(FOO, ?BAR).\n?FOO.");
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                    texts.push(ppt.text().to_owned());
                }
                Event::Token(_) => {}
                Event::MacroDefined(_) => {}
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert_eq!(texts, ["1", "."]);
    }

    #[test]
    fn rescan_preserves_parent_origin_in_chain() {
        // The rescanned `?BAR` token's origin should chain back
        // through the outer ?FOO expansion.
        let mut pp = make("-define(BAR, 1).\n-define(FOO, ?BAR).\n?FOO.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "1" => {
                    let Origin::MacroBody { parent, .. } = ppt.origin() else {
                        panic!("expected MacroBody, got {:?}", ppt.origin());
                    };
                    // Parent is the `?BAR` token's origin, which was
                    // itself a MacroBody from the ?FOO expansion.
                    assert_matches!(**parent, Origin::MacroBody { .. });
                    return;
                }
                Event::Complete => panic!("expected token `1` before Complete"),
                _ => {}
            }
        }
    }

    #[test]
    fn rescan_undefined_inner_fires_event() {
        // ?FOO expands to `?UNKNOWN`; because UNKNOWN is not in the
        // table, the rescan should surface Event::AwaitingMacroExpansion.
        let mut pp = make("-define(FOO, ?UNKNOWN).\n?FOO.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::AwaitingMacroExpansion(req) => {
                    assert_eq!(req.name.as_str(), "UNKNOWN");
                    return;
                }
                Event::MacroDefined(_) | Event::Token(_) => {}
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    #[test]
    fn rescan_caller_response_tokens_are_rescanned() {
        // Caller responds to ?FOO with tokens containing ?BAR. Since
        // BAR is defined, the rescan should expand it internally.
        let mut pp = make("-define(BAR, 1).\n?FOO.");
        loop {
            match pp.step().expect("no protocol error") {
                Event::AwaitingMacroExpansion(req) => {
                    assert_eq!(req.name.as_str(), "FOO");
                    break;
                }
                Event::MacroDefined(_) | Event::Token(_) => {}
                other => panic!("unexpected event before AwaitingMacroExpansion: {other:?}"),
            }
        }
        let response = Source::from_text("<synth:FOO>", "?BAR")
            .expect("test input must scan without lex errors");
        pp.resume_macro_expansion(response).expect("resume ok");
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(t) if t.token().kind().is_lexical() => {
                    assert_eq!(t.text(), "1");
                    return;
                }
                Event::Token(_) => {}
                Event::Complete => panic!("expected `1` token before Complete"),
                other => panic!("unexpected event: {other:?}"),
            }
        }
    }

    // ---- End of Phase 7 tests ---------------------------------------

    // ---- Phase 8: function-like expansion ---------------------------

    fn lexical_texts_from_source(pp: &mut Preprocessor) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                    out.push(ppt.text().to_owned());
                }
                Event::Token(_) | Event::MacroDefined(_) => {}
                Event::Complete => break,
                other => panic!("unexpected event: {other:?}"),
            }
        }
        out
    }

    #[test]
    fn function_like_arity_zero_hit_expands_body() {
        let mut pp = make("-define(FOO(), bar).\n?FOO().");
        let texts = lexical_texts_from_source(&mut pp);
        assert!(texts.contains(&"bar".to_owned()));
        assert!(!texts.contains(&"?".to_owned()));
    }

    #[test]
    fn whitespace_only_parens_match_arity_zero_define() {
        let mut pp = make("-define(FOO(), bar).\n?FOO(   ).");
        let texts = lexical_texts_from_source(&mut pp);
        assert!(texts.contains(&"bar".to_owned()));
        assert!(!texts.contains(&"?".to_owned()));
    }

    #[test]
    fn function_like_single_arg_substitutes_parameter() {
        let mut pp = make("-define(ID(X), X).\n?ID(42).");
        let texts = lexical_texts_from_source(&mut pp);
        assert!(texts.contains(&"42".to_owned()));
    }

    #[test]
    fn function_like_multiple_args_substitute_by_position() {
        let mut pp = make("-define(SWAP(A, B), [B, A]).\n?SWAP(1, 2).");
        let texts = lexical_texts_from_source(&mut pp);
        // Body is `[B, A]`; substituting A=1, B=2 yields `[2, 1]`.
        let joined: Vec<_> = texts
            .iter()
            .filter(|t| ["1", "2", "["].contains(&t.as_str()))
            .cloned()
            .collect();
        // Order: `[`, `2`, `1`
        assert_eq!(joined, ["[", "2", "1"]);
    }

    #[test]
    fn function_like_parameter_repeated_in_body() {
        let mut pp = make("-define(DUP(A), (A, A)).\n?DUP(x).");
        let texts = lexical_texts_from_source(&mut pp);
        // Body `(A, A)` with A=x gives `(x, x)` — two `x` tokens.
        let x_count = texts.iter().filter(|t| t.as_str() == "x").count();
        assert_eq!(x_count, 2);
    }

    #[test]
    fn function_like_argument_tokens_carry_macro_argument_origin() {
        let mut pp = make("-define(ID(X), X).\n?ID(42).");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "42" => {
                    let Origin::MacroArgument {
                        parameter,
                        call_site,
                        ..
                    } = ppt.origin()
                    else {
                        panic!("expected MacroArgument origin, got {:?}", ppt.origin());
                    };
                    assert_eq!(parameter.as_str(), "X");
                    assert!(call_site.start.offset() < call_site.end.offset());
                    return;
                }
                Event::Complete => panic!("expected `42` token"),
                _ => {}
            }
        }
    }

    #[test]
    fn function_like_body_tokens_keep_macro_body_origin() {
        let mut pp = make("-define(WRAP(X), [X]).\n?WRAP(42).");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "[" => {
                    // `[` came from the definition body, not from an argument.
                    assert_matches!(ppt.origin(), Origin::MacroBody { .. });
                    return;
                }
                Event::Complete => panic!("expected `[` token"),
                _ => {}
            }
        }
    }

    #[test]
    fn function_like_middle_empty_argument_yields_empty_expansion() {
        // `?FOO(A,,B)` with `-define(FOO(A, X, B), (A + X + B))`:
        // the middle argument is empty, so X substitutes to nothing.
        let mut pp = make("-define(FOO(A, X, B), (A + X + B)).\n?FOO(1, , 2).");
        let texts = lexical_texts_from_source(&mut pp);
        // Expect at least the `1` and `2` tokens, with only two `+`
        // between them (they end up adjacent).
        let plus_count = texts.iter().filter(|t| t.as_str() == "+").count();
        assert_eq!(plus_count, 2);
        assert!(texts.contains(&"1".to_owned()));
        assert!(texts.contains(&"2".to_owned()));
    }

    // ---- End of Phase 8 tests ---------------------------------------

    // ---- Phase 9: circular expansion detection ----------------------

    type CircularChain = Vec<(String, Option<usize>)>;

    fn expect_circular(pp: &mut Preprocessor) -> (String, Option<usize>, CircularChain) {
        loop {
            match pp.step().expect("no protocol errors") {
                Event::PreprocessError(PreprocessError::CircularExpansion {
                    name,
                    arity,
                    chain,
                    ..
                }) => return (name, arity, chain),
                Event::MacroDefined(_) | Event::Token(_) => {}
                other => panic!("expected CircularExpansion, got {other:?}"),
            }
        }
    }

    #[test]
    fn direct_recursion_in_constant_like_is_detected() {
        let mut pp = make("-define(FOO, ?FOO).\n?FOO.");
        let (name, arity, chain) = expect_circular(&mut pp);
        assert_eq!(name, "FOO");
        assert_eq!(arity, None);
        assert!(chain.iter().any(|(n, _)| n == "FOO"));
    }

    #[test]
    fn indirect_recursion_between_two_macros_is_detected() {
        let mut pp = make("-define(A, ?B).\n-define(B, ?A).\n?A.");
        let (name, _arity, chain) = expect_circular(&mut pp);
        // The chain should mention both A and B.
        assert!(chain.iter().any(|(n, _)| n == "A"));
        assert!(chain.iter().any(|(n, _)| n == "B"));
        // The chain closes on `name`.
        assert_eq!(chain.first().map(|(n, _)| n.as_str()), Some(name.as_str()));
    }

    #[test]
    fn nested_but_non_recursive_expansion_is_allowed() {
        // FOO -> BAR -> 1 : no cycle, expansion should complete without
        // a CircularExpansion error.
        let mut pp = make("-define(BAR, 1).\n-define(FOO, ?BAR).\n?FOO.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::PreprocessError(PreprocessError::CircularExpansion { .. }) => {
                    panic!("unexpected CircularExpansion for non-recursive macros")
                }
                Event::Complete => return,
                _ => {}
            }
        }
    }

    #[test]
    fn direct_recursion_in_function_like_is_detected() {
        let mut pp = make("-define(FOO(A), ?FOO(A)).\n?FOO(1).");
        let (name, arity, _chain) = expect_circular(&mut pp);
        assert_eq!(name, "FOO");
        assert_eq!(arity, Some(1));
    }

    #[test]
    fn caller_response_direct_recursion_is_detected() {
        // Unknown ?FOO fires an event; caller responds with ?FOO,
        // which should surface CircularExpansion on the second step
        // instead of firing another event.
        let mut pp = make("?FOO.");
        match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => assert_eq!(req.name.as_str(), "FOO"),
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        }
        let response = Source::from_text("<synth:FOO>", "?FOO")
            .expect("test input must scan without lex errors");
        pp.resume_macro_expansion(response).expect("resume ok");
        let (name, _arity, chain) = expect_circular(&mut pp);
        assert_eq!(name, "FOO");
        assert!(chain.iter().any(|(n, _)| n == "FOO"));
    }

    // ---- End of Phase 9 tests ---------------------------------------

    // ---- Phase 10: ?FILE / ?LINE ------------------------------------

    #[test]
    fn file_expands_to_source_display_name_as_string_literal() {
        let mut pp = make("?FILE.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.token().kind().is_lexical() => {
                    // The synth token should be a String whose decoded
                    // value is "main.erl".
                    assert_matches!(
                        ppt.value(),
                        erl_tokenize::TokenValue::String(ref cow) if cow.as_ref() == "main.erl"
                    );
                    assert_matches!(
                        ppt.origin(),
                        Origin::SourceInfo {
                            kind: SourceInfoMacroKind::File,
                            ..
                        }
                    );
                    return;
                }
                Event::Complete => panic!("expected ?FILE synth token"),
                _ => {}
            }
        }
    }

    #[test]
    fn line_expands_to_integer_literal_at_call_site() {
        // ?LINE on the second line should evaluate to 2.
        let mut pp = make("\n?LINE.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "2" => {
                    assert_matches!(ppt.token().kind(), erl_tokenize::TokenKind::Integer);
                    assert_matches!(
                        ppt.origin(),
                        Origin::SourceInfo {
                            kind: SourceInfoMacroKind::Line,
                            ..
                        }
                    );
                    return;
                }
                Event::Complete => panic!("expected ?LINE synth token"),
                _ => {}
            }
        }
    }

    #[test]
    fn line_inside_macro_body_uses_outer_call_site() {
        // -define(FOO, ?LINE) on line 1. ?FOO on line 2. Expansion
        // should evaluate ?LINE to 2 (the outer call site), not 1
        // (the ?LINE token's own line inside the definition).
        let mut pp = make("-define(FOO, ?LINE).\n?FOO.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "2" => {
                    return;
                }
                Event::Complete => panic!("expected `2` synth token"),
                _ => {}
            }
        }
    }

    #[test]
    fn user_shadow_of_file_wins() {
        // A user `-define(FILE, custom)` shadows the built-in
        // evaluation; the tokens should carry the user's atom.
        let mut pp = make("-define(FILE, custom).\n?FILE.");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.text() == "custom" => {
                    assert_matches!(ppt.origin(), Origin::MacroBody { .. });
                    return;
                }
                Event::Complete => panic!("expected `custom` token"),
                _ => {}
            }
        }
    }

    // ---- End of Phase 10 tests --------------------------------------

    // ---- Phase 11: ??Param stringification --------------------------

    fn expect_stringified_value(pp: &mut Preprocessor) -> String {
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.token().kind() == erl_tokenize::TokenKind::String => {
                    let erl_tokenize::TokenValue::String(cow) = ppt.value() else {
                        panic!("expected string value")
                    };
                    return cow.into_owned();
                }
                Event::Complete => panic!("expected string token"),
                _ => {}
            }
        }
    }

    #[test]
    fn stringification_of_atom_argument() {
        let mut pp = make("-define(S(A), ??A).\n?S(hello).");
        assert_eq!(expect_stringified_value(&mut pp), "hello");
    }

    #[test]
    fn stringification_of_multi_token_argument() {
        let mut pp = make("-define(S(A), ??A).\n?S(x + 1).");
        assert_eq!(expect_stringified_value(&mut pp), "x + 1");
    }

    #[test]
    fn stringification_drops_hidden_tokens() {
        // Whitespace inside the argument should not survive
        // stringification; consecutive lexical tokens are joined by a
        // single space regardless of the source spacing.
        let mut pp = make("-define(S(A), ??A).\n?S(a       b).");
        assert_eq!(expect_stringified_value(&mut pp), "a b");
    }

    #[test]
    fn stringification_uses_decoded_integer_value() {
        // `16#FF` in the argument should be stringified to `"255"`,
        // not `"16#FF"`, because Integer stringification uses the
        // decoded value.
        let mut pp = make("-define(S(A), ??A).\n?S(16#FF).");
        assert_eq!(expect_stringified_value(&mut pp), "255");
    }

    #[test]
    fn stringification_token_carries_stringification_origin() {
        let mut pp = make("-define(S(A), ??A).\n?S(hello).");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) if ppt.token().kind() == erl_tokenize::TokenKind::String => {
                    let Origin::Stringification { parameter, .. } = ppt.origin() else {
                        panic!("expected Stringification origin, got {:?}", ppt.origin());
                    };
                    assert_eq!(parameter.as_str(), "A");
                    return;
                }
                Event::Complete => panic!("expected string token"),
                _ => {}
            }
        }
    }

    #[test]
    fn stringification_of_non_parameter_is_error() {
        // ??Foo where Foo is not a parameter should produce an
        // InvalidStringificationTarget error.
        let mut pp = make("-define(S(A), ??Foo).\n?S(x).");
        loop {
            match pp.step().expect("no protocol errors") {
                Event::PreprocessError(PreprocessError::InvalidStringificationTarget {
                    ..
                }) => {
                    return;
                }
                Event::Complete => panic!("expected InvalidStringificationTarget"),
                _ => {}
            }
        }
    }

    // ---- End of Phase 11 tests --------------------------------------

    #[test]
    fn clone_isolates_macro_table_updates() {
        let mut original = Preprocessor::new([
            define_source("-define(FOO, 1)."),
            define_source("-define(BAR, 2)."),
        ]);
        match original.step().expect("no protocol error") {
            Event::MacroDefined(_) => {}
            other => panic!("expected MacroDefined, got {other:?}"),
        }
        assert!(original.macros().get_constant("FOO").is_some());
        assert!(original.macros().get_constant("BAR").is_none());

        let mut clone = original.clone();
        drain(&mut original);
        assert!(original.macros().get_constant("BAR").is_some());
        assert!(clone.macros().get_constant("FOO").is_some());
        assert!(clone.macros().get_constant("BAR").is_none());

        drain(&mut clone);
        assert!(clone.macros().get_constant("BAR").is_some());
    }
}
