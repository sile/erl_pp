//! Sans-I/O preprocessor state machine.
//!
//! [`Preprocessor`] owns a [`Cursor`], a shared [`SourceStore`], an
//! append-only [`Preprocessed`] output container, and a small state
//! for pending requests. Callers drive the machine one action at a
//! time with [`Preprocessor::next_action`] and, when the machine
//! surfaces a request that only the caller can answer (a lexical
//! recovery point today; include and conditional requests in later
//! work), respond through one of the response methods before calling
//! `next_action` again.
//!
//! This module intentionally does no I/O and holds no runtime, path,
//! or logging dependency.

use std::sync::Arc;

use erl_tokenize::{Position, Symbol, Token, TokenKind};

use crate::action::{Action, RequestId};
use crate::cursor::Cursor;
use crate::directive::parse_directive;
use crate::error::{LexicalError, ParseFailure, ProtocolError, ProtocolErrorKind};
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
/// 3. When an action carries a pending request, respond through the
///    matching method (currently [`resume_lexical`](Self::resume_lexical))
///    before calling `next_action` again.
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
    /// Monotonic counter used to allocate [`RequestId`]s.
    next_request_id: u32,
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
    /// [`Preprocessor::resume_lexical`] with the associated
    /// [`RequestId`] to continue.
    LexicalErrorPending {
        request_id: RequestId,
        /// The resume position suggested by the tokenizer. Callers
        /// typically pass this back to `resume_lexical` unchanged.
        suggested_resume_position: Position,
    },
    /// Placeholder for future include response handling.
    #[allow(dead_code, reason = "constructed by later include work")]
    IncludeRequestPending { request_id: RequestId },
    /// Placeholder for future conditional response handling.
    #[allow(dead_code, reason = "constructed by later conditional work")]
    ConditionalRequestPending { request_id: RequestId },
    /// The input has been fully processed.
    Completed,
}

/// Snapshot of a pending request that the caller must answer.
///
/// Obtained via [`Preprocessor::pending_request`]. This is a read-only
/// view; call the appropriate response method to advance state.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// Identifier of the pending request.
    pub request_id: RequestId,
    /// Kind of the pending request, with any hint the state machine
    /// carries for the caller (e.g. a suggested resume position for
    /// lexical errors).
    pub kind: PendingRequestKind,
}

/// Kind of pending request.
///
/// The variants track the preprocessor's internal pending states.
/// Later work adds `Include` and `Conditional` variants when the
/// corresponding request infrastructure is wired up.
#[derive(Debug, Clone)]
pub enum PendingRequestKind {
    /// The cursor hit a lexical error. Caller may respond with
    /// [`Preprocessor::resume_lexical`] to continue scanning at
    /// `suggested_resume_position` (or any later position).
    Lexical {
        /// Resume position suggested by the tokenizer. Guaranteed to
        /// be strictly after the failing scan.
        suggested_resume_position: Position,
    },
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
            next_request_id: 0,
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

    /// Returns the pending request, if any.
    pub fn pending_request(&self) -> Option<PendingRequest> {
        match self.state {
            State::LexicalErrorPending {
                request_id,
                suggested_resume_position,
            } => Some(PendingRequest {
                request_id,
                kind: PendingRequestKind::Lexical {
                    suggested_resume_position,
                },
            }),
            State::Scanning
            | State::IncludeRequestPending { .. }
            | State::ConditionalRequestPending { .. }
            | State::Completed => None,
        }
    }

    /// Advances the state machine and returns one [`Action`].
    ///
    /// Returns `Err(ProtocolError { kind: NextActionWhilePending })`
    /// when a request is pending; the caller must respond before
    /// calling this method again.
    pub fn next_action(&mut self) -> Result<Action, ProtocolError> {
        match self.state {
            State::LexicalErrorPending { .. }
            | State::IncludeRequestPending { .. }
            | State::ConditionalRequestPending { .. } => Err(ProtocolError {
                kind: ProtocolErrorKind::NextActionWhilePending,
            }),
            State::Completed => Ok(Action::Complete),
            State::Scanning => Ok(self.step_scan()),
        }
    }

    /// Resumes scanning after a lexical error.
    ///
    /// `at_position` is typically the `suggested_resume_position`
    /// reported by [`pending_request`](Self::pending_request), but any
    /// position strictly after the failing scan is accepted.
    pub fn resume_lexical(
        &mut self,
        request_id: RequestId,
        at_position: Position,
    ) -> Result<(), ProtocolError> {
        match self.state {
            State::LexicalErrorPending {
                request_id: expected,
                ..
            } => {
                if expected != request_id {
                    return Err(ProtocolError {
                        kind: ProtocolErrorKind::UnknownRequestId,
                    });
                }
                self.cursor.resume(at_position);
                self.state = State::Scanning;
                Ok(())
            }
            State::IncludeRequestPending { .. } | State::ConditionalRequestPending { .. } => {
                Err(ProtocolError {
                    kind: ProtocolErrorKind::WrongResponseKind,
                })
            }
            State::Scanning | State::Completed => Err(ProtocolError {
                kind: ProtocolErrorKind::UnexpectedResponse,
            }),
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
                        // up our own pending-request bookkeeping so
                        // callers can recover with resume_lexical
                        // identically to the direct-scan lexical
                        // error path.
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

    /// Records pending-resume bookkeeping for a lexical error and
    /// returns the action that surfaces it to the caller.
    ///
    /// Called from both the direct scan path and the parse-directive
    /// path so that a lexical failure recovered inside the directive
    /// parser reaches the caller with the same resume protocol as a
    /// bare scan failure.
    fn emit_lexical_error(&mut self, lex_err: LexicalError) -> Action {
        let resume_position = lex_err.resume_position;
        let request_id = self.allocate_request_id();
        self.state = State::LexicalErrorPending {
            request_id,
            suggested_resume_position: resume_position,
        };
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

    fn allocate_request_id(&mut self) -> RequestId {
        let index = self.next_request_id;
        self.next_request_id = index
            .checked_add(1)
            .expect("Preprocessor issued more than u32::MAX requests");
        RequestId::from_index(index)
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
            next_request_id: self.next_request_id,
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
            .field("next_request_id", &self.next_request_id)
            .field("at_form_boundary", &self.at_form_boundary)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::directive::Directive;
    use crate::error::PreprocessErrorKind;

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
        // Verify the `-` and `.` both surface.
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
        // `foo` + `.` + directive:endif + `bar` + `.` + complete
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
        match pp.next_action().unwrap() {
            Action::PreprocessError(err) => match err.kind {
                PreprocessErrorKind::Lexical { .. } => {}
                other => panic!("expected lexical kind, got {other:?}"),
            },
            other => panic!("expected PreprocessError, got {other:?}"),
        }
        // next_action while pending should fail with ProtocolError.
        let err = pp.next_action().unwrap_err();
        assert_eq!(err.kind, ProtocolErrorKind::NextActionWhilePending);
        // pending_request reflects the state.
        let pending = pp.pending_request().expect("a request is pending");
        let PendingRequestKind::Lexical {
            suggested_resume_position,
        } = pending.kind;
        // Resume with the suggested position; scanning continues past
        // the bad character.
        pp.resume_lexical(pending.request_id, suggested_resume_position)
            .unwrap();
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
    fn wrong_request_id_on_resume_is_protocol_error() {
        let mut pp = make("\"oops");
        let _ = pp.next_action().unwrap(); // PreprocessError
        let bad_id = RequestId::from_index(999);
        // Get the position from the pending request; use it with a
        // bogus request id.
        let PendingRequestKind::Lexical {
            suggested_resume_position,
        } = pp.pending_request().unwrap().kind;
        let err = pp
            .resume_lexical(bad_id, suggested_resume_position)
            .unwrap_err();
        assert_eq!(err.kind, ProtocolErrorKind::UnknownRequestId);
    }

    #[test]
    fn resume_without_pending_is_protocol_error() {
        let mut pp = make("foo");
        let err = pp
            .resume_lexical(RequestId::from_index(0), Position::new())
            .unwrap_err();
        assert_eq!(err.kind, ProtocolErrorKind::UnexpectedResponse);
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
        // Fork read the same source from its own cursor state and
        // appended everything from position where the fork was taken.
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
