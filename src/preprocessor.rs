//! Sans-I/O preprocessor state machine.
//!
//! [`Preprocessor`] owns a [`Cursor`], a shared [`SourceStore`], an
//! append-only [`Preprocessed`] output container, and a small state
//! variable that tracks whether the machine is currently awaiting a
//! response. Callers drive the machine one action at a time with
//! [`Preprocessor::next_action`] and, when the machine surfaces an
//! action that leaves it awaiting (a lexical recovery point today;
//! include and conditional responses in later work), respond through
//! one of the response methods before calling `next_action` again.
//! [`Preprocessor::status`] reports the current state without
//! advancing it.
//!
//! This module intentionally does no I/O and holds no runtime, path,
//! or logging dependency.

use std::sync::Arc;

use erl_tokenize::{Position, Symbol, Token, TokenKind};

use crate::action::Action;
use crate::cursor::Cursor;
use crate::directive::parse_directive;
use crate::error::{LexicalError, ParseFailure, ProtocolError};
use crate::origin::Origin;
use crate::preprocessed::Preprocessed;
use crate::source::{Source, SourceStore};

/// Sans-I/O preprocessor state machine.
///
/// # Overview
///
/// 1. Create with [`Preprocessor::new`] and an initial [`Source`].
/// 2. Call [`next_action`](Self::next_action) repeatedly; every call
///    returns exactly one [`Action`].
/// 3. When an action leaves the machine awaiting a response, invoke
///    the matching response method (currently
///    [`resume_lexical`](Self::resume_lexical)) before calling
///    `next_action` again. Use [`status`](Self::status) to inspect
///    what response, if any, is expected.
/// 4. When [`Action::Complete`] is returned, later `next_action`
///    calls keep returning `Action::Complete`. Call
///    [`into_preprocessed`](Self::into_preprocessed) to take ownership
///    of the finished output container.
///
/// The preprocessor implements [`Clone`] so that state machine forks
/// (used by later conditional-branching work) can drive the two sides
/// independently. The clone shares the [`SourceStore`] but starts a
/// fresh, empty output container.
pub struct Preprocessor {
    sources: Arc<SourceStore>,
    /// Cursor for the source currently being scanned.
    cursor: Cursor,
    /// Parent cursors saved when include support pushes a new source.
    /// Placeholder for future include support; not populated in this
    /// release.
    include_stack: Vec<Cursor>,
    /// Output container. Shares its `Arc<SourceStore>` with `self.sources`.
    preprocessed: Preprocessed,
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
    /// Default state: `next_action` runs the scan loop.
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
/// is deliberately empty: the payload of the last action already
/// carries the information the caller needs to respond (e.g.
/// [`crate::PreprocessError::Lexical`] carries the resume
/// position).
#[derive(Debug, Clone)]
pub enum Status {
    /// The machine is ready to advance; call
    /// [`Preprocessor::next_action`] for the next event.
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
    /// All input has been consumed; further `next_action` calls
    /// return [`Action::Complete`].
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
        let preprocessed = Preprocessed::new(Arc::clone(&sources));
        Self {
            sources,
            cursor,
            include_stack: Vec::new(),
            preprocessed,
            state: State::Scanning,
            at_form_boundary: true,
        }
    }

    /// Returns a shared handle to the underlying source store.
    pub fn sources(&self) -> &Arc<SourceStore> {
        &self.sources
    }

    /// Returns the output container built so far.
    pub fn preprocessed(&self) -> &Preprocessed {
        &self.preprocessed
    }

    /// Consumes the preprocessor and returns the accumulated output
    /// container.
    ///
    /// Typically called after [`Action::Complete`].
    pub fn into_preprocessed(self) -> Preprocessed {
        self.preprocessed
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

    /// Advances the state machine and returns one [`Action`].
    ///
    /// Returns `Err(ProtocolError::NextActionWhilePending)` when the
    /// machine is awaiting a response; the caller must respond before
    /// calling this method again.
    pub fn next_action(&mut self) -> Result<Action, ProtocolError> {
        match self.state {
            State::AwaitingLexicalResume
            | State::AwaitingIncludeResolution
            | State::AwaitingConditionalDecision => Err(ProtocolError::NextActionWhilePending),
            State::Completed => Ok(Action::Complete),
            State::Scanning => Ok(self.step_scan()),
        }
    }

    /// Resumes scanning after a lexical error.
    ///
    /// `at_position` is typically the `resume_position` carried on
    /// the [`crate::PreprocessError::Lexical`] variant of the most
    /// recent [`Action::PreprocessError`], but any position strictly
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

    /// Runs the scan loop until it can produce one action.
    ///
    /// See the module rustdoc for the loop contract.
    fn step_scan(&mut self) -> Action {
        loop {
            if self.cursor.is_at_eof() {
                if let Some(parent) = self.include_stack.pop() {
                    self.cursor = parent;
                    continue;
                }
                self.state = State::Completed;
                return Action::Complete;
            }

            if self.at_form_boundary {
                match parse_directive(&mut self.cursor) {
                    Ok(Some(directive)) => {
                        // The parser consumed the whole directive
                        // including the terminating `.`, so we are at
                        // a new form boundary.
                        self.at_form_boundary = true;
                        return Action::Directive(directive);
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
                        return Action::PreprocessError(pe.into());
                    }
                }
            }

            match self.cursor.bump() {
                Some(Ok(token)) => {
                    self.update_form_boundary_after_bump(token);
                    let index =
                        self.preprocessed
                            .append(token, self.cursor.source_id(), Origin::Source);
                    return Action::Token { index };
                }
                Some(Err(lex_err)) => return self.emit_lexical_error(lex_err),
                None => continue,
            }
        }
    }

    /// Marks the machine as awaiting a lexical resume and returns
    /// the action that surfaces the error to the caller.
    ///
    /// Called from both the direct scan path and the parse-directive
    /// path so that a lexical failure recovered inside the directive
    /// parser reaches the caller with the same resume protocol as a
    /// bare scan failure.
    fn emit_lexical_error(&mut self, lex_err: LexicalError) -> Action {
        self.state = State::AwaitingLexicalResume;
        Action::PreprocessError(lex_err.into())
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

// Manual Clone: the fork shares the SourceStore but starts a fresh
// empty Preprocessed. Deriving Clone would duplicate the accumulated
// output, which is not what a state-machine fork wants.
impl Clone for Preprocessor {
    fn clone(&self) -> Self {
        Self {
            sources: Arc::clone(&self.sources),
            cursor: self.cursor.clone(),
            include_stack: self.include_stack.clone(),
            preprocessed: Preprocessed::new(Arc::clone(&self.sources)),
            state: self.state.clone(),
            at_form_boundary: self.at_form_boundary,
        }
    }
}

// Manual Debug so users can `dbg!(preprocessor)` without printing the
// whole Preprocessed contents (which can be very large).
impl std::fmt::Debug for Preprocessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preprocessor")
            .field("sources_len", &self.sources.len())
            .field("include_stack_depth", &self.include_stack.len())
            .field("preprocessed_len", &self.preprocessed.tokens().len())
            .field("state", &self.state)
            .field("at_form_boundary", &self.at_form_boundary)
            .finish()
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

    fn drain(pp: &mut Preprocessor) -> Vec<Action> {
        let mut actions = Vec::new();
        loop {
            let action = pp.next_action().expect("no protocol errors");
            let is_complete = matches!(action, Action::Complete);
            actions.push(action);
            if is_complete {
                break;
            }
        }
        actions
    }

    #[test]
    fn empty_source_returns_complete() {
        let mut pp = make("");
        let actions = drain(&mut pp);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], Action::Complete));
        assert!(matches!(pp.status(), Status::Completed));
    }

    #[test]
    fn complete_is_idempotent() {
        let mut pp = make("");
        assert!(matches!(pp.next_action().unwrap(), Action::Complete));
        assert!(matches!(pp.next_action().unwrap(), Action::Complete));
        assert!(matches!(pp.next_action().unwrap(), Action::Complete));
    }

    #[test]
    fn tokens_are_appended_and_indexed() {
        let mut pp = make("foo bar");
        let mut indices = Vec::new();
        loop {
            match pp.next_action().unwrap() {
                Action::Token { index } => indices.push(index),
                Action::Complete => break,
                other => panic!("unexpected action: {other:?}"),
            }
        }
        // foo, whitespace, bar
        assert_eq!(indices, [0, 1, 2]);
        assert_eq!(pp.preprocessed().tokens().len(), 3);
        assert_eq!(pp.preprocessed().text(0), "foo");
        assert_eq!(pp.preprocessed().text(1), " ");
        assert_eq!(pp.preprocessed().text(2), "bar");
        assert!(matches!(
            pp.preprocessed().origins()[0],
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
            match pp.next_action().unwrap() {
                Action::Token { index } => kinds.push(pp.preprocessed().tokens()[index].kind()),
                Action::Complete => break,
                Action::Directive(_) => panic!("should not recognise -module"),
                other => panic!("unexpected action: {other:?}"),
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
    fn recognised_directive_becomes_action() {
        let mut pp = make("-endif.");
        let action = pp.next_action().unwrap();
        assert!(matches!(action, Action::Directive(Directive::Endif(_))));
        let complete = pp.next_action().unwrap();
        assert!(matches!(complete, Action::Complete));
        // No tokens were appended (directives are not appended).
        assert_eq!(pp.preprocessed().tokens().len(), 0);
    }

    #[test]
    fn mixed_forms_stream_correctly() {
        let mut pp = make("foo.-endif.bar.");
        let mut description = Vec::new();
        loop {
            match pp.next_action().unwrap() {
                Action::Token { index } => description.push(format!(
                    "token:{}",
                    pp.preprocessed().tokens()[index].text(
                        pp.sources()
                            .get(pp.preprocessed().source_ids()[index])
                            .text()
                    )
                )),
                Action::Directive(Directive::Endif(_)) => {
                    description.push("directive:endif".into())
                }
                Action::Complete => {
                    description.push("complete".into());
                    break;
                }
                other => panic!("unexpected action: {other:?}"),
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
        let resume_position = match pp.next_action().unwrap() {
            Action::PreprocessError(PreprocessError::Lexical {
                resume_position, ..
            }) => resume_position,
            other => panic!("expected PreprocessError::Lexical, got {other:?}"),
        };
        // status reflects the awaiting state.
        assert!(matches!(pp.status(), Status::AwaitingLexicalResume));
        // next_action while awaiting should fail with ProtocolError.
        assert_eq!(
            pp.next_action().unwrap_err(),
            ProtocolError::NextActionWhilePending
        );
        // Resume with the suggested position; scanning continues past
        // the bad character.
        pp.resume_lexical(resume_position).unwrap();
        assert!(matches!(pp.status(), Status::Scanning));
        // The remaining `unterminated` scans as an atom, then EOF.
        let after = pp.next_action().unwrap();
        assert!(
            matches!(after, Action::Token { .. }),
            "expected token action after resume, got {after:?}"
        );
        let last = pp.next_action().unwrap();
        assert!(matches!(last, Action::Complete));
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
        let resume_position = match pp.next_action().unwrap() {
            Action::PreprocessError(PreprocessError::Lexical {
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
    fn clone_shares_store_and_forks_output() {
        let mut pp = make("foo bar");
        let action = pp.next_action().unwrap();
        assert!(matches!(action, Action::Token { .. }));
        assert_eq!(pp.preprocessed().tokens().len(), 1);

        let mut fork = pp.clone();
        // The fork starts with an empty Preprocessed but the same store.
        assert_eq!(fork.preprocessed().tokens().len(), 0);
        assert!(Arc::ptr_eq(pp.sources(), fork.sources()));

        // Both continue independently.
        let _ = pp.next_action().unwrap();
        let _ = pp.next_action().unwrap();
        let _ = pp.next_action().unwrap(); // Complete
        assert_eq!(pp.preprocessed().tokens().len(), 3);

        let _ = fork.next_action().unwrap();
        let _ = fork.next_action().unwrap();
        let _ = fork.next_action().unwrap();
        let _ = fork.next_action().unwrap(); // Complete
        assert_eq!(fork.preprocessed().tokens().len(), 2); // whitespace + bar
    }

    #[test]
    fn into_preprocessed_returns_the_container() {
        let mut pp = make("foo");
        while !matches!(pp.next_action().unwrap(), Action::Complete) {}
        let pre = pp.into_preprocessed();
        assert_eq!(pre.tokens().len(), 1);
        assert_eq!(pre.text(0), "foo");
    }
}
