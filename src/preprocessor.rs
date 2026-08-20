//! Sans-I/O preprocessor state machine.
//!
//! [`Preprocessor`] owns a [`Cursor`], a shared [`SourceStore`], and a
//! small state variable that tracks whether the machine is currently
//! awaiting a response. Callers drive the machine one step at a time
//! with [`Preprocessor::step`] and, when the returned event leaves the
//! machine awaiting a response (include and conditional responses in
//! later work), respond through one of the response methods before
//! calling `step` again. [`Preprocessor::status`] reports the current
//! state without advancing it.
//!
//! The preprocessor consumes pre-scanned [`Source`] token streams;
//! tokenization is the caller's responsibility (scan with
//! [`erl_tokenize::scan_token`] and hand the resulting tokens to
//! [`Source::new`]). Lexical errors surface only when the caller
//! scans, never through [`Preprocessor::step`].
//!
//! The preprocessor does not retain scanned tokens; each
//! [`crate::Event::Token`] carries a self-contained
//! [`PreprocessedToken`] and the caller keeps whatever accumulator
//! they need.
//!
//! This module intentionally does no I/O and holds no runtime, path,
//! or logging dependency.
#![expect(
    clippy::result_large_err,
    reason = "PreprocessError deliberately carries structured spans; \
              boxing every Result would add allocation overhead on every define"
)]

use std::collections::VecDeque;
use std::sync::Arc;

use erl_tokenize::{Keyword, Position, Symbol, Token, TokenKind, TokenValue};

use crate::cursor::Cursor;
use crate::directive::{Directive, parse_directive};
use crate::error::{MacroCallErrorKind, PreprocessError, ProtocolError};
use crate::event::{Event, MacroExpansionRequest};
use crate::macros::{MacroDefinition, MacroKey, MacroTable};
use crate::origin::{Origin, SourceInfoMacroKind};
use crate::preprocessed_token::PreprocessedToken;
use crate::source::{Source, SourceId, SourceSpan, SourceStore};
use crate::source_string::SourceString;

/// Sans-I/O preprocessor state machine.
///
/// # Overview
///
/// 1. Create with [`Preprocessor::new`] and an initial [`Source`].
/// 2. Call [`step`](Self::step) repeatedly; every call advances the
///    machine by one transition and returns exactly one [`Event`].
/// 3. When the returned event leaves the machine awaiting a response,
///    invoke the matching response method before calling `step` again.
///    Use [`status`](Self::status) to inspect what response, if any, is
///    expected.
/// 4. When [`Event::Complete`] is returned, later `step` calls keep
///    returning `Event::Complete`.
///
/// The preprocessor does not retain scanned tokens; each
/// [`Event::Token`] carries a self-contained [`PreprocessedToken`]
/// and the caller keeps whatever accumulator they need.
///
/// The preprocessor implements [`Clone`] so that state machine forks
/// (used by later conditional-branching work) can drive the two sides
/// independently. The clone shares the [`SourceStore`].
pub struct Preprocessor {
    sources: Arc<SourceStore>,
    /// Cursor for the source currently being scanned.
    cursor: Cursor,
    /// Parent cursors saved when include support pushes a new source.
    /// Placeholder for future include support; not populated in this
    /// release.
    include_stack: Vec<Cursor>,
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
    expansion_queue: VecDeque<PreprocessedToken>,
}

/// State-machine state.
#[derive(Debug, Clone)]
enum State {
    /// Default state: `step` runs the scan loop.
    Scanning,
    /// Placeholder for future include response handling.
    #[expect(dead_code, reason = "constructed by later include work")]
    AwaitingIncludeResolution,
    /// Placeholder for future conditional response handling.
    #[expect(dead_code, reason = "constructed by later conditional work")]
    AwaitingConditionalDecision,
    /// The scan loop emitted [`Event::AwaitingMacroExpansion`] and is
    /// waiting for a [`Preprocessor::resume_macro_expansion`] call.
    /// The payload retains what the resume path needs to attach the
    /// caller-supplied tokens to the right call site.
    AwaitingMacroExpansion(PendingExpansion),
    /// The input has been fully processed.
    Completed,
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

/// Public view of the preprocessor's state.
///
/// Returned by [`Preprocessor::status`]. Payload for awaiting variants
/// is deliberately empty: the payload of the last event already
/// carries the information the caller needs to respond.
#[derive(Debug, Clone)]
pub enum Status {
    /// The machine is ready to advance; call
    /// [`Preprocessor::step`] for the next event.
    Scanning,
    /// The machine paused waiting for an include to be resolved.
    /// Reserved for future work; not produced in this release.
    AwaitingIncludeResolution,
    /// The machine paused waiting for a conditional-branch decision.
    /// Reserved for future work; not produced in this release.
    AwaitingConditionalDecision,
    /// The machine paused waiting for the caller to expand a macro
    /// through [`Preprocessor::resume_macro_expansion`].
    AwaitingMacroExpansion,
    /// All input has been consumed; further `step` calls
    /// return [`Event::Complete`].
    Completed,
}

impl Preprocessor {
    /// Creates a preprocessor positioned at the start of `source`.
    ///
    /// The source is appended to a freshly created shared
    /// [`SourceStore`] and given the first [`crate::SourceId`].
    pub fn new(source: Source) -> Self {
        let sources = Arc::new(SourceStore::new());
        let source_id = sources.append(source);
        let arc_source = sources.get(source_id);
        let cursor = Cursor::new(source_id, arc_source);
        Self {
            sources,
            cursor,
            include_stack: Vec::new(),
            macros: MacroTable::new(),
            state: State::Scanning,
            at_form_boundary: true,
            expansion_queue: VecDeque::new(),
        }
    }

    /// Returns a shared handle to the underlying source store.
    pub fn sources(&self) -> &Arc<SourceStore> {
        &self.sources
    }

    /// Returns a read-only view of the macro table.
    ///
    /// Entries are added by `-define(...)` directives and removed by
    /// `-undef(...)` directives observed on the current source, and by
    /// [`Preprocessor::define_initial`]. Once a directive is applied
    /// the table update is visible from this method before the
    /// caller receives the matching [`Event::Directive`] — the
    /// state-then-event ordering is fixed (see [`step`](Self::step)).
    pub fn macros(&self) -> &MacroTable {
        &self.macros
    }

    /// Registers an initial macro from a pre-scanned `-define(...)`
    /// [`Source`].
    ///
    /// The source is appended to the preprocessor's [`SourceStore`]
    /// and parsed as a single `-define(...).` directive. Typically
    /// called before the first [`step`](Self::step). When called
    /// mid-stream it simply adds a definition to the current macro
    /// table.
    ///
    /// Returns `Err(PreprocessError::Parse)` when the source does not
    /// parse as a `-define(...).` directive, or
    /// [`PreprocessError::MacroDefinition`] when the definition
    /// itself is invalid (duplicate parameter, etc.).
    pub fn define_initial(&mut self, source: Source) -> Result<(), PreprocessError> {
        let source_id = self.sources.append(source);
        let source_arc = self.sources.get(source_id);
        let mut cursor = Cursor::new(source_id, Arc::clone(&source_arc));
        let parsed = match parse_directive(&mut cursor) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Err(PreprocessError::Parse {
                    directive_start: crate::source::SourceSpan::new(
                        source_id,
                        erl_tokenize::Position::new(),
                        erl_tokenize::Position::new(),
                    ),
                    expected: "-define directive".to_owned(),
                    actual: crate::error::PreprocessParseFailure::UnexpectedEof,
                });
            }
            Err(pe) => return Err(pe.into()),
        };
        let define = match parsed {
            Directive::Define(d) => d,
            other => {
                return Err(PreprocessError::Parse {
                    directive_start: directive_span_of(&other),
                    expected: "-define directive".to_owned(),
                    actual: crate::error::PreprocessParseFailure::UnexpectedToken {
                        span: directive_span_of(&other),
                        kind: TokenKind::Symbol(Symbol::Hyphen),
                    },
                });
            }
        };
        let def = MacroDefinition::from_directive(&define, source_arc, source_id, Origin::Source)?;
        self.macros.insert(def);
        Ok(())
    }

    /// Reports the current state of the state machine.
    ///
    /// This is a read-only view; call the appropriate response method
    /// to advance state.
    pub fn status(&self) -> Status {
        match &self.state {
            State::Scanning => Status::Scanning,
            State::AwaitingIncludeResolution => Status::AwaitingIncludeResolution,
            State::AwaitingConditionalDecision => Status::AwaitingConditionalDecision,
            State::AwaitingMacroExpansion(_) => Status::AwaitingMacroExpansion,
            State::Completed => Status::Completed,
        }
    }

    /// Advances the state machine and returns one [`Event`].
    ///
    /// Returns `Err(ProtocolError::StepWhilePending)` when the
    /// machine is awaiting a response; the caller must respond before
    /// calling this method again.
    pub fn step(&mut self) -> Result<Event, ProtocolError> {
        match &self.state {
            State::AwaitingIncludeResolution
            | State::AwaitingConditionalDecision
            | State::AwaitingMacroExpansion(_) => Err(ProtocolError::StepWhilePending),
            State::Completed => Ok(Event::Complete),
            State::Scanning => Ok(self.step_scan()),
        }
    }

    /// Resumes the scan loop after an
    /// [`Event::AwaitingMacroExpansion`] event.
    ///
    /// `source` is the caller-supplied expansion result whose tokens
    /// are spliced into the token stream. Pass a token-free
    /// [`Source`] to skip the call without emitting any expansion
    /// tokens; the caller is responsible for surfacing any error
    /// diagnostic in its own error stream.
    ///
    /// Returns:
    /// * [`ProtocolError::UnexpectedResponse`] when the machine is
    ///   scanning or completed (no macro expansion is pending);
    /// * [`ProtocolError::WrongResponseKind`] when the machine is
    ///   awaiting a different response (include, conditional).
    pub fn resume_macro_expansion(&mut self, source: Source) -> Result<(), ProtocolError> {
        match &self.state {
            State::AwaitingMacroExpansion(_) => {}
            State::AwaitingIncludeResolution | State::AwaitingConditionalDecision => {
                return Err(ProtocolError::WrongResponseKind);
            }
            State::Scanning | State::Completed => {
                return Err(ProtocolError::UnexpectedResponse);
            }
        }
        let State::AwaitingMacroExpansion(pending) =
            std::mem::replace(&mut self.state, State::Scanning)
        else {
            unreachable!("state was checked immediately above");
        };
        let source_id = self.sources.append(source);
        let source_arc = self.sources.get(source_id);
        for token in source_arc.tokens() {
            let origin = Origin::CallerExpansion {
                parent: Arc::clone(&pending.parent_origin),
                call_site: pending.call_site,
                name: pending.name.clone(),
            };
            self.expansion_queue.push_back(PreprocessedToken::new(
                *token,
                Arc::clone(&source_arc),
                source_id,
                origin,
            ));
        }
        Ok(())
    }

    /// Runs the scan loop until it can produce one event.
    ///
    /// See the module rustdoc for the loop contract.
    fn step_scan(&mut self) -> Event {
        loop {
            // Rescan `?NAME` at the head of the expansion queue so
            // that macros produced by a prior expansion are expanded
            // themselves before their tokens surface.
            if matches!(
                self.expansion_queue.front(),
                Some(ppt) if is_symbol(*ppt.token(), Symbol::Question)
            ) {
                match self.try_rescan_queue_call() {
                    MacroCallOutcome::Fire(event) => return *event,
                    MacroCallOutcome::Enqueued => continue,
                    MacroCallOutcome::NotACall => {
                        // The `?` is not the start of a macro call —
                        // emit it and any following queued tokens
                        // normally by falling through to the drain.
                    }
                }
            }
            // Drain any pending macro-expansion tokens.
            if let Some(ppt) = self.expansion_queue.pop_front() {
                let token = *ppt.token();
                self.update_form_boundary_after_bump(token);
                return Event::Token(ppt);
            }

            if self.cursor.is_at_eof() {
                if let Some(parent) = self.include_stack.pop() {
                    self.cursor = parent;
                    continue;
                }
                self.state = State::Completed;
                return Event::Complete;
            }

            if self.at_form_boundary {
                match parse_directive(&mut self.cursor) {
                    Ok(Some(directive)) => {
                        // The parser consumed the whole directive
                        // including the terminating `.`, so we are at
                        // a new form boundary.
                        self.at_form_boundary = true;
                        // Apply state effects (macro table updates)
                        // BEFORE emitting the event so the caller
                        // observes the post-update table state when
                        // matching Event::Directive.
                        if let Err(e) = self.apply_directive_effects(&directive) {
                            return Event::PreprocessError(e);
                        }
                        return Event::Directive(directive);
                    }
                    Ok(None) => {
                        // Cursor restored to entry. Fall through — if
                        // the next token is `?` and a macro call
                        // expands to nothing lexical, we want to stay
                        // at form boundary so the *following* form can
                        // still be recognized as a directive.
                        // `update_form_boundary_after_bump` on the
                        // eventual bump path will drop the flag once a
                        // non-`.` lexical token is emitted.
                    }
                    Err(pe) => {
                        self.at_form_boundary = false;
                        return Event::PreprocessError(pe.into());
                    }
                }
            }

            // Check for a macro call at the cursor before doing the
            // normal bump. try_recognize_macro_call consumes the call
            // when it matches, either queuing an expansion or emitting
            // an AwaitingMacroExpansion event; when it does not match,
            // it leaves the cursor untouched.
            if matches!(
                self.cursor.peek(),
                Some(t) if is_symbol(t, Symbol::Question)
            ) {
                match self.try_recognize_macro_call() {
                    MacroCallOutcome::Fire(event) => return *event,
                    MacroCallOutcome::Enqueued => continue,
                    MacroCallOutcome::NotACall => {}
                }
            }

            match self.cursor.bump() {
                Some(token) => {
                    self.update_form_boundary_after_bump(token);
                    let ppt = PreprocessedToken::new(
                        token,
                        Arc::clone(self.cursor.source()),
                        self.cursor.source_id(),
                        Origin::Source,
                    );
                    return Event::Token(ppt);
                }
                None => continue,
            }
        }
    }

    /// Attempts to recognize a `?NAME` macro call at the cursor.
    ///
    /// Called only when the next raw token is `?`. Handles the
    /// constant-like shape (`?NAME`, no arguments) in this phase; the
    /// function-like shape (`?NAME(...)`) is left to a later phase
    /// and reported as [`MacroCallOutcome::NotACall`] with the cursor
    /// restored.
    fn try_recognize_macro_call(&mut self) -> MacroCallOutcome {
        let entry = self.cursor.checkpoint();
        let question_tok = self
            .cursor
            .bump()
            .expect("caller checked next token is `?`");

        let Some(name_tok) = self.cursor.peek_lexical() else {
            self.cursor.restore(entry);
            return MacroCallOutcome::NotACall;
        };
        // `??` prefix is a stringification — deferred to a later phase.
        if is_symbol(name_tok, Symbol::Question) {
            self.cursor.restore(entry);
            return MacroCallOutcome::NotACall;
        }
        if !matches!(name_tok.kind(), TokenKind::Atom | TokenKind::Variable) {
            self.cursor.restore(entry);
            return MacroCallOutcome::NotACall;
        }

        // Consume through the name token so the cursor sits just past
        // it. Any hidden tokens between `?` and the name are absorbed
        // by the call and dropped from the output stream (matching
        // OTP epp's behaviour on a whitespace-free token stream).
        while let Some(t) = self.cursor.bump() {
            if t.start() == name_tok.start() {
                break;
            }
        }

        let source_id = self.cursor.source_id();
        let source_text = self.cursor.source_text();
        let name_text = match name_tok.value(source_text) {
            TokenValue::Atom(cow) => cow.into_owned(),
            TokenValue::Variable(name) => name.to_owned(),
            _ => {
                // Shouldn't happen given the kind check above, but
                // fall back to non-call rather than panic.
                self.cursor.restore(entry);
                return MacroCallOutcome::NotACall;
            }
        };
        let name_span = SourceSpan::new(source_id, name_tok.start(), name_tok.end());
        let name_ss = SourceString::new(name_text.clone(), name_span);

        // Peek one lexical token ahead. `(` starts a function-like
        // call; anything else keeps the constant-like shape.
        let inner = self.cursor.checkpoint();
        let is_function_like = matches!(
            self.cursor.peek_lexical(),
            Some(t) if is_symbol(t, Symbol::OpenParen)
        );
        self.cursor.restore(inner);

        if !is_function_like {
            // Constant-like call.
            let call_site = SourceSpan::new(source_id, question_tok.start(), name_tok.end());
            return self.finish_recognized_call(
                name_text,
                name_ss,
                None,
                call_site,
                Vec::new(),
                Arc::new(Origin::Source),
            );
        }

        // Function-like call: consume through the opening `(` and
        // parse the argument list.
        let open_paren = 'find: loop {
            let Some(t) = self.cursor.bump() else {
                self.cursor.restore(entry);
                return MacroCallOutcome::NotACall;
            };
            if is_symbol(t, Symbol::OpenParen) {
                break 'find t;
            }
        };
        let _ = open_paren; // acknowledged — position is captured via the parse result
        let parsed = match parse_macro_arguments(&mut self.cursor, source_id) {
            Ok(p) => p,
            Err(kind) => {
                let end = self
                    .cursor
                    .peek()
                    .map(|t| t.start())
                    .unwrap_or_else(|| name_tok.end());
                let span = SourceSpan::new(source_id, question_tok.start(), end);
                return MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
                    PreprocessError::MacroCall { span, kind },
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
            Arc::new(Origin::Source),
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
        arguments: Vec<Vec<PreprocessedToken>>,
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
                                PreprocessError::MacroCall {
                                    span: call_site,
                                    kind,
                                },
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

        let request = MacroExpansionRequest {
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
        MacroCallOutcome::Fire(Box::new(Event::AwaitingMacroExpansion(request)))
    }

    /// Splices `tokens` in front of the current expansion queue so
    /// they surface before anything scheduled by earlier expansions.
    fn prepend_to_queue(&mut self, mut tokens: VecDeque<PreprocessedToken>) {
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
        let mut expanded: VecDeque<PreprocessedToken> =
            VecDeque::with_capacity(source_arc.tokens().len());
        for token in source_arc.tokens() {
            let origin = Origin::SourceInfo {
                parent: Arc::clone(parent_origin),
                call_site,
                kind,
            };
            expanded.push_back(PreprocessedToken::new(
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
    /// This handles the constant-like shape only (matching the
    /// current phase). Function-like calls surfacing from a prior
    /// expansion are treated as `NotACall` for now and the `?` is
    /// emitted as a regular token.
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
        if is_symbol(name_tok, Symbol::Question) {
            return MacroCallOutcome::NotACall;
        }
        if !matches!(name_tok.kind(), TokenKind::Atom | TokenKind::Variable) {
            return MacroCallOutcome::NotACall;
        }
        // If the next lexical after the name is `(`, this is a
        // function-like call; deferred to a later phase.
        let after_name_idx = {
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
        let cursor_next_lexical = if after_name_idx.is_none() {
            self.cursor.peek_lexical()
        } else {
            None
        };
        let following_lexical = after_name_idx.or(cursor_next_lexical);
        if matches!(following_lexical, Some(t) if is_symbol(t, Symbol::OpenParen)) {
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
            TokenValue::Atom(cow) => cow.into_owned(),
            TokenValue::Variable(name) => name.to_owned(),
            _ => {
                // Should not happen given the kind check; if it does,
                // re-emit `?` as a regular token by pushing it back.
                self.expansion_queue.push_front(name_ppt);
                self.expansion_queue.push_front(question_ppt);
                return MacroCallOutcome::NotACall;
            }
        };
        let question_span = question_ppt.source_span();
        let call_site =
            SourceSpan::new(question_span.source_id, question_span.start, name_tok.end());
        let name_span = SourceSpan::new(
            name_ppt.source_span().source_id,
            name_tok.start(),
            name_tok.end(),
        );
        let name_ss = SourceString::new(name_text.clone(), name_span);
        let parent_origin = Arc::new(question_ppt.origin().clone());
        self.finish_recognized_call(
            name_text,
            name_ss,
            None,
            call_site,
            Vec::new(),
            parent_origin,
        )
    }

    fn apply_directive_effects(&mut self, directive: &Directive) -> Result<(), PreprocessError> {
        match directive {
            Directive::Define(d) => {
                let source = Arc::clone(self.cursor.source());
                let source_id = self.cursor.source_id();
                let def = MacroDefinition::from_directive(d, source, source_id, Origin::Source)?;
                self.macros.insert(def);
                Ok(())
            }
            Directive::Undef(u) => {
                self.macros.remove_all_by_name(u.name.as_str());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn update_form_boundary_after_bump(&mut self, token: Token) {
        // A lexical `.` symbol ends the current form; the next
        // scan step should attempt directive recognition. Any
        // other lexical token puts us mid-form. Hidden tokens
        // (comments, whitespace) leave the flag unchanged so that a
        // run of hidden tokens between the last `.` and the next
        // form still counts as a form boundary.
        match token.kind() {
            TokenKind::Symbol(Symbol::Dot) => self.at_form_boundary = true,
            kind if kind.is_lexical() => self.at_form_boundary = false,
            _ => {}
        }
    }
}

impl Clone for Preprocessor {
    fn clone(&self) -> Self {
        Self {
            sources: Arc::clone(&self.sources),
            cursor: self.cursor.clone(),
            include_stack: self.include_stack.clone(),
            macros: self.macros.clone(),
            state: self.state.clone(),
            at_form_boundary: self.at_form_boundary,
            expansion_queue: self.expansion_queue.clone(),
        }
    }
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

fn is_symbol(token: Token, sym: Symbol) -> bool {
    matches!(token.kind(), TokenKind::Symbol(s) if s == sym)
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
    let mut position = Position::new();
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
        Origin::Source | Origin::Include(_) => current_call_site,
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
/// [`PreprocessError::MacroCall`] with a `CircularExpansion` kind.
fn fire_circular(
    name: String,
    arity: Option<usize>,
    call_site: SourceSpan,
    chain: Vec<(String, Option<usize>)>,
) -> MacroCallOutcome {
    MacroCallOutcome::Fire(Box::new(Event::PreprocessError(
        PreprocessError::MacroCall {
            span: call_site,
            kind: MacroCallErrorKind::CircularExpansion { name, arity, chain },
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
            Origin::Include(parent) => cur = parent,
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
) -> VecDeque<PreprocessedToken> {
    let definition_span = def.directive_span;
    let mut out: VecDeque<PreprocessedToken> = VecDeque::with_capacity(def.replacement.len());
    for replacement in &def.replacement {
        let origin = Origin::MacroBody {
            parent: Arc::clone(parent_origin),
            call_site,
            definition_span,
        };
        let token = *replacement.token();
        let source = Arc::clone(replacement.source());
        let source_id = replacement.source_span().source_id;
        out.push_back(PreprocessedToken::new(token, source, source_id, origin));
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
    arguments: &[Vec<PreprocessedToken>],
    call_site: SourceSpan,
    parent_origin: &Arc<Origin>,
    sources: &Arc<SourceStore>,
) -> Result<VecDeque<PreprocessedToken>, MacroCallErrorKind> {
    let definition_span = def.directive_span;
    let mut out: VecDeque<PreprocessedToken> = VecDeque::new();
    let repl = &def.replacement;
    let mut i = 0;
    while i < repl.len() {
        let token = *repl[i].token();

        // Recognize `??Param` before falling into the normal
        // parameter-substitution path. The pattern is `?` + hidden* +
        // `?` + hidden* + Variable-that-matches-a-parameter.
        if is_symbol(token, Symbol::Question)
            && let Some(next_idx) = find_next_lexical_index(repl, i + 1)
            && is_symbol(*repl[next_idx].token(), Symbol::Question)
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
            if target_tok.kind() != TokenKind::Variable {
                return Err(MacroCallErrorKind::InvalidStringificationTarget {
                    span: target_ppt.source_span(),
                });
            }
            let var_text = target_tok.text(target_ppt.source().text());
            let Some(param_idx) = def.params.iter().position(|p| p.name.as_str() == var_text)
            else {
                return Err(MacroCallErrorKind::InvalidStringificationTarget {
                    span: target_ppt.source_span(),
                });
            };
            let parameter = def.params[param_idx].name.clone();
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

        if token.kind() == TokenKind::Variable {
            let var_text = token.text(repl[i].source().text());
            if let Some(idx) = def.params.iter().position(|p| p.name.as_str() == var_text)
                && idx < arguments.len()
            {
                let parameter = def.params[idx].name.clone();
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
                    out.push_back(PreprocessedToken::new(arg_token, source, source_id, origin));
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
        out.push_back(PreprocessedToken::new(token, source, source_id, origin));
        i += 1;
    }
    Ok(out)
}

/// Returns the index of the next lexical token in `tokens` at or
/// after `start`, or `None` if none is found before the slice runs
/// out.
fn find_next_lexical_index(tokens: &[PreprocessedToken], start: usize) -> Option<usize> {
    (start..tokens.len()).find(|&i| tokens[i].token().kind().is_lexical())
}

/// Materialises a stringified argument as a single Erlang string
/// literal token, matching the OTP `epp:stringify/2` shape: keep only
/// lexical tokens, print each with a per-kind formatter, and join
/// them with a single space.
fn stringify_argument(
    argument: &[PreprocessedToken],
    sources: &Arc<SourceStore>,
    call_site: SourceSpan,
    parameter: SourceString,
    definition_span: SourceSpan,
    parent_origin: &Arc<Origin>,
) -> Vec<PreprocessedToken> {
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
            PreprocessedToken::new(
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
fn stringify_token_text(ppt: &PreprocessedToken) -> String {
    let token = *ppt.token();
    let source_text = ppt.source().text();
    // Integer / Float use the decoded value when the tokenizer
    // produced one; other kinds pass through the source text.
    match token.value(source_text) {
        TokenValue::Integer(Some(n)) => n.to_string(),
        TokenValue::Float(f) => {
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
    arguments: Vec<Vec<PreprocessedToken>>,
    /// Position of the byte after the closing `)`. Used together with
    /// the call's opening `?` to build the call-site span.
    close_end: Position,
}

/// Parses macro-call arguments starting immediately after the opening
/// `(`.
///
/// Tracks a small delimiter stack so top-level `,` and `)` are only
/// recognized at the outermost bracket depth. Follows OTP `epp:macro_arg`:
/// leading empty (`?NAME(, ...)`) and trailing empty (`?NAME(..., )`)
/// arguments are rejected as errors, middle empties (`?NAME(A, , B)`)
/// are valid arity-`N` groups. Hidden tokens (whitespace and comments)
/// are preserved inside the returned argument streams.
fn parse_macro_arguments(
    cursor: &mut Cursor,
    source_id: SourceId,
) -> Result<ParsedArguments, MacroCallErrorKind> {
    let mut arguments: Vec<Vec<PreprocessedToken>> = Vec::new();
    let mut current: Vec<PreprocessedToken> = Vec::new();
    let mut current_has_lexical = false;
    let mut stack: Vec<Delimiter> = Vec::new();
    // `true` while we have not yet seen any lexical token or comma.
    let mut before_first_content = true;

    loop {
        let Some(token) = cursor.peek() else {
            return Err(MacroCallErrorKind::UnclosedArgument);
        };

        // A `,` or `)` counts as top-level once every remaining
        // delimiter is a `FunEnd` sentinel; the sentinels get drained
        // when the enclosing group ends, matching OTP's re-drive.
        let effective_top = stack.iter().all(|d| *d == Delimiter::FunEnd);

        if effective_top && token.kind().is_lexical() {
            match token.kind() {
                TokenKind::Symbol(Symbol::Comma) => {
                    cursor.bump();
                    stack.clear();
                    if before_first_content {
                        return Err(MacroCallErrorKind::LeadingEmptyArgument);
                    }
                    arguments.push(std::mem::take(&mut current));
                    current_has_lexical = false;
                    before_first_content = false;
                    continue;
                }
                TokenKind::Symbol(Symbol::CloseParen) => {
                    let close_end = token.end();
                    cursor.bump();
                    stack.clear();
                    if before_first_content && current.is_empty() {
                        // `?NAME()` — arity 0.
                    } else if !current_has_lexical {
                        if before_first_content {
                            // `?NAME(  )` — arity 1, hidden-only
                            // argument (valid, semantically empty).
                            arguments.push(current);
                        } else {
                            return Err(MacroCallErrorKind::TrailingEmptyArgument);
                        }
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

        cursor.bump();
        let ppt = PreprocessedToken::new(
            token,
            Arc::clone(cursor.source()),
            source_id,
            Origin::Source,
        );
        current.push(ppt);
        if token.kind().is_lexical() {
            current_has_lexical = true;
            before_first_content = false;
        }

        match token.kind() {
            TokenKind::Symbol(sym) => match sym {
                Symbol::OpenParen => stack.push(Delimiter::CloseParen),
                Symbol::OpenSquare => stack.push(Delimiter::CloseSquare),
                Symbol::OpenBrace => stack.push(Delimiter::CloseBrace),
                Symbol::DoubleLeftAngle => stack.push(Delimiter::CloseDoubleAngle),
                Symbol::CloseParen => {
                    pop_fun_ends(&mut stack);
                    if matches!(stack.last(), Some(Delimiter::CloseParen)) {
                        stack.pop();
                    }
                }
                Symbol::CloseSquare => {
                    pop_fun_ends(&mut stack);
                    if matches!(stack.last(), Some(Delimiter::CloseSquare)) {
                        stack.pop();
                    }
                }
                Symbol::CloseBrace => {
                    pop_fun_ends(&mut stack);
                    if matches!(stack.last(), Some(Delimiter::CloseBrace)) {
                        stack.pop();
                    }
                }
                Symbol::DoubleRightAngle => {
                    pop_fun_ends(&mut stack);
                    if matches!(stack.last(), Some(Delimiter::CloseDoubleAngle)) {
                        stack.pop();
                    }
                }
                Symbol::RightArrow => promote_fun_end_to_end(&mut stack),
                _ => {}
            },
            TokenKind::Keyword(kw) => match kw {
                Keyword::Begin
                | Keyword::If
                | Keyword::Case
                | Keyword::Maybe
                | Keyword::Receive
                | Keyword::Try
                | Keyword::Cond => stack.push(Delimiter::End),
                Keyword::Fun => stack.push(Delimiter::FunEnd),
                Keyword::End => {
                    if matches!(stack.last(), Some(Delimiter::End) | Some(Delimiter::FunEnd)) {
                        stack.pop();
                    }
                }
                Keyword::When => promote_fun_end_to_end(&mut stack),
                _ => {}
            },
            _ => {}
        }
    }
}

fn pop_fun_ends(stack: &mut Vec<Delimiter>) {
    while matches!(stack.last(), Some(Delimiter::FunEnd)) {
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
            .field("macros_len", &self.macros.len())
            .field("state", &self.state)
            .field("at_form_boundary", &self.at_form_boundary)
            .finish()
    }
}

fn directive_span_of(directive: &Directive) -> crate::source::SourceSpan {
    match directive {
        Directive::Include(d) => d.span,
        Directive::IncludeLib(d) => d.span,
        Directive::Define(d) => d.span,
        Directive::Undef(d) => d.span,
        Directive::Ifdef(d) => d.span,
        Directive::Ifndef(d) => d.span,
        Directive::Else(d) => d.span,
        Directive::Endif(d) => d.span,
        Directive::Error(d) => d.span,
        Directive::Warning(d) => d.span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::directive::Directive;
    use crate::error::PreprocessError;

    fn make(text: &str) -> Preprocessor {
        Preprocessor::new(Source::from_text("main.erl", text))
    }

    fn define_source(text: &str) -> Source {
        Source::from_text("<initial macro>", text)
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
        assert!(matches!(events[0], Event::Complete));
        assert!(matches!(pp.status(), Status::Completed));
    }

    #[test]
    fn complete_is_idempotent() {
        let mut pp = make("");
        assert!(matches!(
            pp.step().expect("no protocol error"),
            Event::Complete
        ));
        assert!(matches!(
            pp.step().expect("no protocol error"),
            Event::Complete
        ));
        assert!(matches!(
            pp.step().expect("no protocol error"),
            Event::Complete
        ));
    }

    #[test]
    fn resume_macro_expansion_while_scanning_is_unexpected_response() {
        let mut pp = make("foo");
        let response = Source::from_text("<synth:test>", "");
        assert_eq!(
            pp.resume_macro_expansion(response)
                .expect_err("protocol error expected"),
            ProtocolError::UnexpectedResponse
        );
    }

    #[test]
    fn resume_macro_expansion_while_completed_is_unexpected_response() {
        let mut pp = make("");
        // Drain to Completed.
        drain(&mut pp);
        let response = Source::from_text("<synth:test>", "");
        assert_eq!(
            pp.resume_macro_expansion(response)
                .expect_err("protocol error expected"),
            ProtocolError::UnexpectedResponse
        );
    }

    #[test]
    fn constant_like_call_with_table_hit_expands_from_body() {
        let mut pp = make("-define(FOO, bar).\n?FOO.");
        let mut texts = Vec::new();
        loop {
            match pp.step().expect("no protocol errors") {
                Event::Token(ppt) => texts.push(ppt.text().to_owned()),
                Event::Directive(Directive::Define(_)) => {}
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
        let request = match event {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        assert_eq!(request.name.as_str(), "UNKNOWN");
        assert_eq!(request.arity, None);
        assert!(request.arguments.is_empty());
        assert!(matches!(pp.status(), Status::AwaitingMacroExpansion));
    }

    #[test]
    fn resume_macro_expansion_enqueues_response_tokens() {
        let mut pp = make("?UNKNOWN.");
        let request = match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        let response = Source::from_text("<synth:UNKNOWN>", "bar");
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
        assert_eq!(*origin_call_site, request.call_site);
        assert_eq!(name.as_str(), "UNKNOWN");
    }

    #[test]
    fn resume_macro_expansion_with_empty_source_skips_call() {
        let mut pp = make("?UNKNOWN.");
        let _request = match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        };
        let empty_response = Source::from_text("<synth:UNKNOWN>", "");
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
    fn step_while_awaiting_macro_expansion_is_protocol_error() {
        let mut pp = make("?UNKNOWN.");
        pp.step().expect("first step yields Awaiting event");
        assert_eq!(
            pp.step().expect_err("second step is a protocol error"),
            ProtocolError::StepWhilePending
        );
    }

    fn expect_awaiting(pp: &mut Preprocessor) -> MacroExpansionRequest {
        match pp.step().expect("no protocol errors") {
            Event::AwaitingMacroExpansion(req) => req,
            other => panic!("expected AwaitingMacroExpansion, got {other:?}"),
        }
    }

    fn expect_preprocess_error(pp: &mut Preprocessor) -> MacroCallErrorKind {
        match pp.step().expect("no protocol errors") {
            Event::PreprocessError(PreprocessError::MacroCall { kind, .. }) => kind,
            other => panic!("expected PreprocessError::MacroCall, got {other:?}"),
        }
    }

    #[test]
    fn function_like_call_arity_zero() {
        let mut pp = make("?FOO().");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.name.as_str(), "FOO");
        assert_eq!(request.arity, Some(0));
        assert!(request.arguments.is_empty());
    }

    #[test]
    fn function_like_call_single_argument() {
        let mut pp = make("?FOO(bar).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(1));
        assert_eq!(request.arguments.len(), 1);
        let arg_texts: Vec<_> = request.arguments[0]
            .iter()
            .map(|t| t.text().to_owned())
            .collect();
        assert!(arg_texts.contains(&"bar".to_owned()));
    }

    #[test]
    fn function_like_call_multiple_arguments() {
        let mut pp = make("?FOO(a, b, c).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(3));
        let names: Vec<_> = request
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
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(2));
    }

    #[test]
    fn function_like_call_nested_brackets_and_braces() {
        let mut pp = make("?FOO([a, b], {c, d}, <<1, 2>>).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(3));
    }

    #[test]
    fn function_like_call_middle_empty_is_valid() {
        let mut pp = make("?FOO(a, , b).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(3));
        // The middle argument has no lexical token, only hidden or empty.
        let middle_has_lexical = request.arguments[1]
            .iter()
            .any(|t| t.token().kind().is_lexical());
        assert!(!middle_has_lexical);
    }

    #[test]
    fn function_like_call_leading_empty_is_error() {
        let mut pp = make("?FOO(, a).");
        let kind = expect_preprocess_error(&mut pp);
        assert!(matches!(kind, MacroCallErrorKind::LeadingEmptyArgument));
    }

    #[test]
    fn function_like_call_trailing_empty_is_error() {
        let mut pp = make("?FOO(a, ).");
        let kind = expect_preprocess_error(&mut pp);
        assert!(matches!(kind, MacroCallErrorKind::TrailingEmptyArgument));
    }

    #[test]
    fn function_like_call_unclosed_argument_is_error() {
        let mut pp = make("?FOO(a, b");
        let kind = expect_preprocess_error(&mut pp);
        assert!(matches!(kind, MacroCallErrorKind::UnclosedArgument));
    }

    #[test]
    fn function_like_call_keyword_block_balancing() {
        let mut pp = make("?FOO(case X of Y -> Z end, W).");
        let request = expect_awaiting(&mut pp);
        // The commas inside case ... end are not top-level.
        assert_eq!(request.arity, Some(2));
    }

    #[test]
    fn function_like_call_fun_expression() {
        let mut pp = make("?FOO(fun() -> ok end, a).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(2));
    }

    #[test]
    fn function_like_call_fun_type_syntax() {
        // `fun((atom()) -> ok)` ends with `)`, not `end`; the FunEnd
        // sentinel must drain when the outer macro `)` is reached.
        let mut pp = make("?FOO(fun((atom()) -> ok), b).");
        let request = expect_awaiting(&mut pp);
        assert_eq!(request.arity, Some(2));
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
        // foo, whitespace, bar
        let texts: Vec<&str> = streamed.iter().map(|t| t.text()).collect();
        assert_eq!(texts, ["foo", " ", "bar"]);
        assert!(matches!(
            streamed[0].origin(),
            crate::origin::Origin::Source
        ));
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
                Event::Directive(_) => panic!("should not recognise -module"),
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(!kinds.is_empty());
        let has_hyphen = kinds
            .iter()
            .any(|k| matches!(k, TokenKind::Symbol(Symbol::Hyphen)));
        let has_dot = kinds
            .iter()
            .any(|k| matches!(k, TokenKind::Symbol(Symbol::Dot)));
        assert!(has_hyphen && has_dot);
    }

    #[test]
    fn recognised_directive_becomes_event() {
        let mut pp = make("-endif.");
        let first = pp.step().expect("no protocol error");
        assert!(matches!(first, Event::Directive(Directive::Endif(_))));
        // Directive tokens are consumed by the parser, not streamed.
        let complete = pp.step().expect("no protocol error");
        assert!(matches!(complete, Event::Complete));
    }

    #[test]
    fn mixed_forms_stream_correctly() {
        let mut pp = make("foo.-endif.bar.");
        let mut description = Vec::new();
        loop {
            match pp.step().expect("no protocol error") {
                Event::Token(ppt) => description.push(format!("token:{}", ppt.text())),
                Event::Directive(Directive::Endif(_)) => description.push("directive:endif".into()),
                Event::Complete => {
                    description.push("complete".into());
                    break;
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }
        assert!(description.iter().any(|s| s == "token:foo"));
        assert!(description.iter().any(|s| s == "token:bar"));
        assert!(description.iter().any(|s| s == "directive:endif"));
        assert!(description.last().expect("at least one description") == "complete");
    }

    #[test]
    fn clone_shares_store_and_advances_independently() {
        // Take the first token from pp, then fork. pp and fork share
        // the SourceStore but their cursors advance independently.
        let mut pp = make("foo bar");
        let first = pp.step().expect("no protocol error");
        assert!(matches!(first, Event::Token(_)));

        let mut fork = pp.clone();
        assert!(Arc::ptr_eq(pp.sources(), fork.sources()));

        let pp_tokens = collect_token_texts(&mut pp);
        let fork_tokens = collect_token_texts(&mut fork);

        // pp already emitted `foo`; the remainder is ` ` and `bar`.
        assert_eq!(pp_tokens, [" ", "bar"]);
        // The fork resumes from the same cursor position pp had at
        // clone time.
        assert_eq!(fork_tokens, [" ", "bar"]);
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
        assert!(matches!(event, Event::Directive(Directive::Define(_))));
        // State-then-event contract: when the caller observes the
        // event, the macro table already contains the definition.
        assert!(pp.macros().get_constant("FOO").is_some());
        assert_eq!(pp.macros().len(), 1);
    }

    #[test]
    fn undef_directive_removes_macros_before_event() {
        let mut pp = make("-define(FOO, 1).\n-define(FOO(A), A).\n-undef(FOO).");
        // Drain define/undef; the table should end empty.
        loop {
            match pp.step().expect("no protocol error") {
                Event::Directive(Directive::Define(_)) => {}
                Event::Directive(Directive::Undef(_)) => {
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
        assert!(matches!(
            event,
            Event::PreprocessError(PreprocessError::MacroDefinition { .. })
        ));
        // The failing definition is not added to the table.
        assert!(pp.macros().is_empty());
    }

    #[test]
    fn define_initial_registers_before_step() {
        let mut pp = make("");
        pp.define_initial(define_source("-define(FOO, 1)."))
            .expect("valid define text");
        pp.define_initial(define_source("-define(BAR(A), A)."))
            .expect("valid define text");
        assert_eq!(pp.macros().len(), 2);
        assert!(pp.macros().get_constant("FOO").is_some());
        assert!(pp.macros().get_function("BAR", 1).is_some());
        let event = pp.step().expect("no protocol error");
        assert!(matches!(event, Event::Complete));
    }

    #[test]
    fn define_initial_uses_source_origin() {
        let mut pp = make("");
        pp.define_initial(define_source("-define(FOO, 1)."))
            .expect("valid define text");
        let def = pp.macros().get_constant("FOO").expect("defined");
        assert!(matches!(def.origin, Origin::Source));
    }

    #[test]
    fn define_initial_rejects_non_define_text() {
        let mut pp = make("");
        // A recognised but non-define directive is rejected.
        let err = pp
            .define_initial(define_source("-endif."))
            .expect_err("preprocess error expected");
        assert!(matches!(err, PreprocessError::Parse { .. }));
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
                Event::Directive(_) => {}
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
                    assert!(matches!(**parent, Origin::MacroBody { .. }));
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
                Event::Directive(_) | Event::Token(_) => {}
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
                Event::Directive(_) | Event::Token(_) => {}
                other => panic!("unexpected event before AwaitingMacroExpansion: {other:?}"),
            }
        }
        let response = Source::from_text("<synth:FOO>", "?BAR");
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
                Event::Token(_) | Event::Directive(_) => {}
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
                    assert!(matches!(ppt.origin(), Origin::MacroBody { .. }));
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
                Event::PreprocessError(PreprocessError::MacroCall {
                    kind: MacroCallErrorKind::CircularExpansion { name, arity, chain },
                    ..
                }) => return (name, arity, chain),
                Event::Directive(_) | Event::Token(_) => {}
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
                Event::PreprocessError(PreprocessError::MacroCall {
                    kind: MacroCallErrorKind::CircularExpansion { .. },
                    ..
                }) => panic!("unexpected CircularExpansion for non-recursive macros"),
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
        let response = Source::from_text("<synth:FOO>", "?FOO");
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
                    assert!(matches!(
                        ppt.value(),
                        erl_tokenize::TokenValue::String(ref cow) if cow.as_ref() == "main.erl"
                    ));
                    assert!(matches!(
                        ppt.origin(),
                        Origin::SourceInfo {
                            kind: SourceInfoMacroKind::File,
                            ..
                        }
                    ));
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
                    assert!(matches!(
                        ppt.token().kind(),
                        erl_tokenize::TokenKind::Integer
                    ));
                    assert!(matches!(
                        ppt.origin(),
                        Origin::SourceInfo {
                            kind: SourceInfoMacroKind::Line,
                            ..
                        }
                    ));
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
                    assert!(matches!(ppt.origin(), Origin::MacroBody { .. }));
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
                Event::PreprocessError(PreprocessError::MacroCall {
                    kind: MacroCallErrorKind::InvalidStringificationTarget { .. },
                    ..
                }) => return,
                Event::Complete => panic!("expected InvalidStringificationTarget"),
                _ => {}
            }
        }
    }

    // ---- End of Phase 11 tests --------------------------------------

    #[test]
    fn clone_isolates_macro_table_updates() {
        let mut original = make("-define(FOO, 1).");
        original.step().expect("no protocol error");
        assert!(original.macros().get_constant("FOO").is_some());

        // Clone before original scans further.
        let mut clone = original.clone();
        // Add another define into the clone.
        clone
            .define_initial(define_source("-define(BAR, 2)."))
            .expect("valid define text");

        assert!(clone.macros().get_constant("BAR").is_some());
        assert!(original.macros().get_constant("BAR").is_none());
    }
}
