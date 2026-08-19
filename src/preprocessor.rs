//! Sans-I/O preprocessor state machine.
//!
//! [`Preprocessor`] owns a [`Cursor`], a shared [`SourceStore`], and a
//! small state variable that tracks whether the machine is currently
//! awaiting a response. Callers drive the machine one step at a time
//! with [`Preprocessor::step`] and, when the returned event leaves
//! the machine awaiting a response (a lexical recovery point today;
//! include and conditional responses in later work), respond through
//! one of the response methods before calling `step` again.
//! [`Preprocessor::status`] reports the current state without
//! advancing it.
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

use std::sync::Arc;

use erl_tokenize::{Position, Symbol, Token, TokenKind};

use crate::cursor::Cursor;
use crate::directive::{Directive, parse_directive};
use crate::error::{LexicalError, ParseFailure, PreprocessError, ProtocolError};
use crate::event::Event;
use crate::macros::{MacroDefinition, MacroTable};
use crate::origin::Origin;
use crate::preprocessed_token::PreprocessedToken;
use crate::source::{Source, SourceStore};

/// Sans-I/O preprocessor state machine.
///
/// # Overview
///
/// 1. Create with [`Preprocessor::new`] and an initial [`Source`].
/// 2. Call [`step`](Self::step) repeatedly; every call advances the
///    machine by one transition and returns exactly one [`Event`].
/// 3. When the returned event leaves the machine awaiting a response,
///    invoke the matching response method (currently
///    [`resume_lexical`](Self::resume_lexical)) before calling
///    `step` again. Use [`status`](Self::status) to inspect what
///    response, if any, is expected.
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
}

/// State-machine state.
#[derive(Debug, Clone)]
enum State {
    /// Default state: `step` runs the scan loop.
    Scanning,
    /// A lexical error was surfaced; caller must call
    /// [`Preprocessor::resume_lexical`] to continue.
    AwaitingLexicalResume,
    /// Placeholder for future include response handling.
    #[allow(dead_code, reason = "constructed by later include work")]
    AwaitingIncludeResolution,
    /// Placeholder for future conditional response handling.
    #[allow(dead_code, reason = "constructed by later conditional work")]
    AwaitingConditionalDecision,
    /// The input has been fully processed.
    Completed,
}

/// Public view of the preprocessor's state.
///
/// Returned by [`Preprocessor::status`]. Payload for awaiting variants
/// is deliberately empty: the payload of the last event already
/// carries the information the caller needs to respond (e.g.
/// [`crate::PreprocessError::Lexical`] carries the resume
/// position).
#[derive(Debug, Clone)]
pub enum Status {
    /// The machine is ready to advance; call
    /// [`Preprocessor::step`] for the next event.
    Scanning,
    /// The machine paused after a lexical error and expects
    /// [`Preprocessor::resume_lexical`] before it can advance again.
    AwaitingLexicalResume,
    /// The machine paused waiting for an include to be resolved.
    /// Reserved for future work; not produced in this release.
    AwaitingIncludeResolution,
    /// The machine paused waiting for a conditional-branch decision.
    /// Reserved for future work; not produced in this release.
    AwaitingConditionalDecision,
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

    /// Registers an initial macro from a full `-define(...)` directive
    /// text.
    ///
    /// The text is scanned in a fresh pseudo source appended to the
    /// preprocessor's [`SourceStore`]; no direct `SourceStore` mutation
    /// is exposed to the caller. Typically called before the first
    /// [`step`](Self::step). When called mid-stream it simply adds a
    /// definition to the current macro table.
    ///
    /// Returns `Err(PreprocessError::Parse)` when the text does not
    /// parse as a `-define(...).` directive, or the underlying
    /// [`PreprocessError`] when the definition itself is invalid
    /// (duplicate parameter, etc.).
    pub fn define_initial(
        &mut self,
        directive_text: impl AsRef<str>,
    ) -> Result<(), PreprocessError> {
        let text = directive_text.as_ref();
        let source_id = self
            .sources
            .append_pseudo("<initial macro>", text.to_owned());
        let source = self.sources.get(source_id);
        let mut cursor = Cursor::new(source_id, Arc::clone(&source));
        let parsed = match parse_directive(&mut cursor) {
            Ok(Some(d)) => d,
            Ok(None) => {
                return Err(PreprocessError::Parse {
                    directive_start: crate::source::SourceSpan::new(
                        source_id,
                        Position::new(),
                        Position::new(),
                    ),
                    expected: "-define directive".to_owned(),
                    actual: crate::error::PreprocessParseFailure::UnexpectedEof,
                });
            }
            Err(pe) => {
                if let ParseFailure::Lexical(boxed) = pe.actual {
                    return Err(PreprocessError::from(*boxed));
                }
                return Err(pe.into());
            }
        };
        let define = match parsed {
            Directive::Define(d) => d,
            other => {
                return Err(PreprocessError::Parse {
                    directive_start: directive_span_of(&other),
                    expected: "-define directive".to_owned(),
                    actual: crate::error::PreprocessParseFailure::UnexpectedToken {
                        span: directive_span_of(&other),
                        kind: erl_tokenize::TokenKind::Symbol(Symbol::Hyphen),
                    },
                });
            }
        };
        let origin = Origin::Predefined(Arc::new(Origin::Source));
        let def = MacroDefinition::from_directive(&define, source, source_id, origin)?;
        self.macros.insert(def);
        Ok(())
    }

    /// Reports the current state of the state machine.
    ///
    /// This is a read-only view; call the appropriate response method
    /// to advance state.
    pub fn status(&self) -> Status {
        match self.state {
            State::Scanning => Status::Scanning,
            State::AwaitingLexicalResume => Status::AwaitingLexicalResume,
            State::AwaitingIncludeResolution => Status::AwaitingIncludeResolution,
            State::AwaitingConditionalDecision => Status::AwaitingConditionalDecision,
            State::Completed => Status::Completed,
        }
    }

    /// Advances the state machine and returns one [`Event`].
    ///
    /// Returns `Err(ProtocolError::StepWhilePending)` when the
    /// machine is awaiting a response; the caller must respond before
    /// calling this method again.
    pub fn step(&mut self) -> Result<Event, ProtocolError> {
        match self.state {
            State::AwaitingLexicalResume
            | State::AwaitingIncludeResolution
            | State::AwaitingConditionalDecision => Err(ProtocolError::StepWhilePending),
            State::Completed => Ok(Event::Complete),
            State::Scanning => Ok(self.step_scan()),
        }
    }

    /// Resumes scanning after a lexical error.
    ///
    /// `at_position` is typically the `resume_position` carried on
    /// the [`crate::PreprocessError::Lexical`] variant of the most
    /// recent [`Event::PreprocessError`], but any position strictly
    /// after the failing scan is accepted.
    pub fn resume_lexical(&mut self, at_position: Position) -> Result<(), ProtocolError> {
        match self.state {
            State::AwaitingLexicalResume => {
                self.cursor.resume(at_position);
                self.state = State::Scanning;
                Ok(())
            }
            State::AwaitingIncludeResolution | State::AwaitingConditionalDecision => {
                Err(ProtocolError::WrongResponseKind)
            }
            State::Scanning | State::Completed => Err(ProtocolError::UnexpectedResponse),
        }
    }

    /// Runs the scan loop until it can produce one event.
    ///
    /// See the module rustdoc for the loop contract.
    fn step_scan(&mut self) -> Event {
        loop {
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
                        // A lexical failure that surfaced inside
                        // parse_directive left the cursor in the
                        // pending-resume state. Recognise it and set
                        // up our own awaiting bookkeeping so callers
                        // can recover with resume_lexical identically
                        // to the direct-scan lexical error path.
                        if let ParseFailure::Lexical(boxed_lex) = pe.actual {
                            return self.emit_lexical_error(*boxed_lex);
                        }
                        return Event::PreprocessError(pe.into());
                    }
                }
            }

            match self.cursor.bump() {
                Some(Ok(token)) => {
                    self.update_form_boundary_after_bump(token);
                    let ppt = PreprocessedToken::new(
                        token,
                        Arc::clone(self.cursor.source()),
                        self.cursor.source_id(),
                        Origin::Source,
                    );
                    return Event::Token(ppt);
                }
                Some(Err(lex_err)) => return self.emit_lexical_error(lex_err),
                None => continue,
            }
        }
    }

    /// Marks the machine as awaiting a lexical resume and returns
    /// the event that surfaces the error to the caller.
    ///
    /// Called from both the direct scan path and the parse-directive
    /// path so that a lexical failure recovered inside the directive
    /// parser reaches the caller with the same resume protocol as a
    /// bare scan failure.
    fn emit_lexical_error(&mut self, lex_err: LexicalError) -> Event {
        self.state = State::AwaitingLexicalResume;
        Event::PreprocessError(lex_err.into())
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
                self.macros.remove_all_by_name(&u.name);
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
        }
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
        Preprocessor::new(Source::new("main.erl", text))
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
        assert!(matches!(pp.step().unwrap(), Event::Complete));
        assert!(matches!(pp.step().unwrap(), Event::Complete));
        assert!(matches!(pp.step().unwrap(), Event::Complete));
    }

    #[test]
    fn tokens_are_streamed_in_order() {
        let mut pp = make("foo bar");
        let mut streamed = Vec::new();
        loop {
            match pp.step().unwrap() {
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
            match pp.step().unwrap() {
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
        let first = pp.step().unwrap();
        assert!(matches!(first, Event::Directive(Directive::Endif(_))));
        // Directive tokens are consumed by the parser, not streamed.
        let complete = pp.step().unwrap();
        assert!(matches!(complete, Event::Complete));
    }

    #[test]
    fn mixed_forms_stream_correctly() {
        let mut pp = make("foo.-endif.bar.");
        let mut description = Vec::new();
        loop {
            match pp.step().unwrap() {
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
        assert!(description.last().unwrap() == "complete");
    }

    #[test]
    fn lexical_error_pauses_the_machine() {
        // The bare `"` is a lexical failure; the tokenizer's suggested
        // resume position skips past that single character and the
        // trailing `unterminated` then scans as a plain atom.
        let mut pp = make("\"unterminated");
        let resume_position = match pp.step().unwrap() {
            Event::PreprocessError(PreprocessError::Lexical {
                resume_position, ..
            }) => resume_position,
            other => panic!("expected PreprocessError::Lexical, got {other:?}"),
        };
        // status reflects the awaiting state.
        assert!(matches!(pp.status(), Status::AwaitingLexicalResume));
        // step while awaiting should fail with ProtocolError.
        assert_eq!(pp.step().unwrap_err(), ProtocolError::StepWhilePending);
        // Resume with the suggested position; scanning continues past
        // the bad character.
        pp.resume_lexical(resume_position).unwrap();
        assert!(matches!(pp.status(), Status::Scanning));
        // The remaining `unterminated` scans as an atom, then EOF.
        let after = pp.step().unwrap();
        assert!(
            matches!(after, Event::Token(_)),
            "expected token event after resume, got {after:?}"
        );
        let last = pp.step().unwrap();
        assert!(matches!(last, Event::Complete));
    }

    #[test]
    fn resume_without_pending_is_protocol_error() {
        let mut pp = make("foo");
        assert_eq!(
            pp.resume_lexical(Position::new()).unwrap_err(),
            ProtocolError::UnexpectedResponse
        );
    }

    #[test]
    fn double_resume_is_protocol_error() {
        // First response transitions the machine back to Scanning, so
        // a second resume_lexical is treated as UnexpectedResponse.
        let mut pp = make("\"oops");
        let resume_position = match pp.step().unwrap() {
            Event::PreprocessError(PreprocessError::Lexical {
                resume_position, ..
            }) => resume_position,
            other => panic!("expected PreprocessError::Lexical, got {other:?}"),
        };
        pp.resume_lexical(resume_position).unwrap();
        assert_eq!(
            pp.resume_lexical(resume_position).unwrap_err(),
            ProtocolError::UnexpectedResponse
        );
    }

    #[test]
    fn clone_shares_store_and_advances_independently() {
        // Take the first token from pp, then fork. pp and fork share
        // the SourceStore but their cursors advance independently.
        let mut pp = make("foo bar");
        let first = pp.step().unwrap();
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
            match pp.step().unwrap() {
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
        let event = pp.step().unwrap();
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
            match pp.step().unwrap() {
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
        let event = pp.step().unwrap();
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
        pp.define_initial("-define(FOO, 1).").unwrap();
        pp.define_initial("-define(BAR(A), A).").unwrap();
        assert_eq!(pp.macros().len(), 2);
        assert!(pp.macros().get_constant("FOO").is_some());
        assert!(pp.macros().get_function("BAR", 1).is_some());
        let event = pp.step().unwrap();
        assert!(matches!(event, Event::Complete));
    }

    #[test]
    fn define_initial_uses_predefined_origin() {
        let mut pp = make("");
        pp.define_initial("-define(FOO, 1).").unwrap();
        let def = pp.macros().get_constant("FOO").expect("defined");
        assert!(matches!(def.origin, Origin::Predefined(_)));
    }

    #[test]
    fn define_initial_rejects_non_define_text() {
        let mut pp = make("");
        // A recognised but non-define directive is rejected.
        let err = pp.define_initial("-endif.").unwrap_err();
        assert!(matches!(err, PreprocessError::Parse { .. }));
    }

    #[test]
    fn clone_isolates_macro_table_updates() {
        let mut original = make("-define(FOO, 1).");
        original.step().unwrap();
        assert!(original.macros().get_constant("FOO").is_some());

        // Clone before original scans further.
        let mut clone = original.clone();
        // Add another define into the clone by feeding it a fresh
        // source through define_initial.
        clone.define_initial("-define(BAR, 2).").unwrap();

        assert!(clone.macros().get_constant("BAR").is_some());
        assert!(original.macros().get_constant("BAR").is_none());
    }
}
