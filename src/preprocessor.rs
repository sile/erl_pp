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
use crate::macros::{MacroDefinition, MacroTable};
use crate::origin::Origin;
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
            // Drain any pending macro-expansion tokens first so that
            // expansions surface in order before further scanning.
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
                        // Cursor was restored to entry; consume the
                        // form-boundary flag so the loop moves on to
                        // the regular bump path.
                        self.at_form_boundary = false;
                        continue;
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
            return self.finish_recognized_call(name_text, name_ss, None, call_site, Vec::new());
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
        self.finish_recognized_call(name_text, name_ss, arity, call_site, parsed.arguments)
    }

    /// Common tail of [`Preprocessor::try_recognize_macro_call`].
    ///
    /// Either enqueues a MacroTable hit (constant-like only in this
    /// phase) or fires an [`Event::AwaitingMacroExpansion`] for the
    /// caller.
    fn finish_recognized_call(
        &mut self,
        name_text: String,
        name_ss: SourceString,
        arity: Option<usize>,
        call_site: SourceSpan,
        arguments: Vec<Vec<PreprocessedToken>>,
    ) -> MacroCallOutcome {
        // Function-like table hits are handled in the next phase; for
        // now they fall through to the caller-driven expansion event.
        if arity.is_none()
            && let Some(def) = self.macros.get_constant(&name_text)
        {
            let parent_origin = Arc::new(Origin::Source);
            let definition_span = def.directive_span;
            for replacement in &def.replacement {
                let origin = Origin::MacroBody {
                    parent: Arc::clone(&parent_origin),
                    call_site,
                    definition_span,
                };
                let token = *replacement.token();
                let source = Arc::clone(replacement.source());
                let source_id = replacement.source_span().source_id;
                self.expansion_queue
                    .push_back(PreprocessedToken::new(token, source, source_id, origin));
            }
            return MacroCallOutcome::Enqueued;
        }

        let parent_origin = Arc::new(Origin::Source);
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
                    if before_first_content {
                        // `?NAME()` — arity 0.
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
