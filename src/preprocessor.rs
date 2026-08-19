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
//! tokenization is the caller's responsibility (use
//! [`Source::from_text`] as a convenience or hand in a token stream
//! through [`Source::new`]). Lexical errors surface only when the
//! caller constructs a [`Source`], never through
//! [`Preprocessor::step`].
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

use erl_tokenize::{Symbol, Token, TokenKind};

use crate::cursor::Cursor;
use crate::directive::{Directive, parse_directive};
use crate::error::{PreprocessError, ProtocolError};
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
    /// The input has been fully processed.
    Completed,
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
        let origin = Origin::Predefined(Arc::new(Origin::Source));
        let def = MacroDefinition::from_directive(&define, source_arc, source_id, origin)?;
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
            State::AwaitingIncludeResolution | State::AwaitingConditionalDecision => {
                Err(ProtocolError::StepWhilePending)
            }
            State::Completed => Ok(Event::Complete),
            State::Scanning => Ok(self.step_scan()),
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
                        return Event::PreprocessError(pe.into());
                    }
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
        Preprocessor::new(Source::from_text("main.erl", text).expect("valid tokens"))
    }

    fn define_source(text: &str) -> Source {
        Source::from_text("<initial macro>", text).expect("valid define tokens")
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
    fn define_initial_uses_predefined_origin() {
        let mut pp = make("");
        pp.define_initial(define_source("-define(FOO, 1)."))
            .expect("valid define text");
        let def = pp.macros().get_constant("FOO").expect("defined");
        assert!(matches!(def.origin, Origin::Predefined(_)));
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
