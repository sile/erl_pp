//! Single-source token scanning primitive with lookahead and rollback.
//!
//! [`Cursor`] walks one [`Source`](crate::Source) with
//! [`erl_tokenize::scan_token`] and hands the resulting [`Token`]s to
//! the caller in the order they appear, including comments and
//! whitespace. It supports lookahead, snapshot-based rollback, and
//! recovery after a lexical error.
//!
//! Multi-source concerns like `-include` are the state machine's job;
//! the state machine builds a new [`Cursor`] for each entered source
//! and pushes the parent cursor onto its own stack.

use std::collections::VecDeque;
use std::sync::Arc;

use erl_tokenize::{Position, Token, scan_token};

use crate::error::LexicalError;
use crate::source::{Source, SourceId, SourceSpan};

/// Scans one [`Source`] with lookahead, checkpoint/rollback, and
/// lexical-error recovery.
///
/// A `Cursor` is created with the [`SourceId`] of the source it walks
/// and an [`Arc<Source>`] handle so it never has to re-query the
/// [`crate::SourceStore`] during scanning.
pub(crate) struct Cursor {
    source_id: SourceId,
    source: Arc<Source>,
    /// Position to hand to the next `scan_token` call.
    position: Position,
    /// Tokens that have been scanned ahead but not yet consumed. Kept
    /// as a FIFO: [`peek`](Cursor::peek) returns the front,
    /// [`bump`](Cursor::bump) pops the front.
    lookahead: VecDeque<Token>,
    /// When set, the last scan hit a lexical error. `position` is left
    /// at the error start; the outer state machine must call
    /// [`resume`](Cursor::resume) (typically with this stored position,
    /// which is `erl_tokenize::Error`'s `resume_position`) to continue.
    pending_resume: Option<Position>,
}

/// Snapshot of a [`Cursor`]'s mutable state, taken by
/// [`Cursor::checkpoint`] and used to rewind the cursor via
/// [`Cursor::restore`].
///
/// The captured state is the scan position, the lookahead queue, and
/// the pending-resume state. The [`SourceId`] and the [`Arc<Source>`]
/// are not captured because they never change during a cursor's life.
#[derive(Clone)]
pub(crate) struct Checkpoint {
    position: Position,
    lookahead: VecDeque<Token>,
    pending_resume: Option<Position>,
}

impl Cursor {
    /// Creates a new cursor positioned at the start of `source`.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn new(source_id: SourceId, source: Arc<Source>) -> Self {
        Self {
            source_id,
            source,
            position: Position::new(),
            lookahead: VecDeque::new(),
            pending_resume: None,
        }
    }

    /// Returns the identifier of the source the cursor walks.
    #[allow(dead_code, reason = "consulted by preprocessor internals to be added")]
    pub(crate) fn source_id(&self) -> SourceId {
        self.source_id
    }

    /// Returns `Some(position)` when the cursor is waiting for a
    /// [`resume`](Self::resume) call after emitting a
    /// [`LexicalError`]. The position is the resume point suggested by
    /// `erl_tokenize` (`Error`'s `resume_position`).
    #[allow(dead_code, reason = "consulted by preprocessor internals to be added")]
    pub(crate) fn pending_resume(&self) -> Option<Position> {
        self.pending_resume
    }

    /// Resumes scanning at `position`, clearing any pending-resume
    /// state left by a previous [`LexicalError`].
    ///
    /// Callers typically pass the position stored in
    /// [`pending_resume`](Self::pending_resume) (the resume point
    /// suggested by `erl_tokenize`, which is guaranteed to be strictly
    /// after the failing scan).
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn resume(&mut self, position: Position) {
        self.position = position;
        self.pending_resume = None;
    }

    /// Returns `true` when the cursor has consumed the whole source
    /// and no scanning error is pending.
    #[allow(dead_code, reason = "consulted by preprocessor internals to be added")]
    pub(crate) fn is_at_eof(&self) -> bool {
        self.lookahead.is_empty()
            && self.pending_resume.is_none()
            && self.position.offset() == self.source.text().len()
    }

    /// Returns the next token, including hidden tokens (comments and
    /// whitespace), without advancing the cursor.
    ///
    /// Returns `None` at end of source, `Some(Err(_))` when the scan
    /// fails. Multiple calls with no intervening [`bump`](Self::bump)
    /// return the same token.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn peek(&mut self) -> Option<Result<Token, LexicalError>> {
        match self.ensure_lookahead(1) {
            Ok(true) => Some(Ok(self.lookahead[0])),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// Returns the next lexical (non-hidden) token without advancing
    /// the cursor.
    ///
    /// Hidden tokens scanned along the way are queued internally so
    /// that the next [`bump`](Self::bump) calls yield them in the
    /// original order before the lexical token.
    ///
    /// Returns `None` when no lexical token remains, `Some(Err(_))`
    /// when scanning fails before one is found.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn peek_lexical(&mut self) -> Option<Result<Token, LexicalError>> {
        let mut i = 0;
        loop {
            match self.ensure_lookahead(i + 1) {
                Ok(true) => {}
                Ok(false) => return None,
                Err(e) => return Some(Err(e)),
            }
            if self.lookahead[i].kind().is_lexical() {
                return Some(Ok(self.lookahead[i]));
            }
            i += 1;
        }
    }

    /// Consumes and returns the next token, including hidden tokens.
    ///
    /// Returns `None` at end of source, `Some(Err(_))` when the scan
    /// fails.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn bump(&mut self) -> Option<Result<Token, LexicalError>> {
        match self.ensure_lookahead(1) {
            Ok(true) => Some(Ok(self
                .lookahead
                .pop_front()
                .expect("ensure_lookahead(1) guarantees a queued token"))),
            Ok(false) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// Captures the cursor's mutable state so it can be restored later
    /// with [`restore`](Self::restore).
    ///
    /// Multiple checkpoints can be taken and restored in LIFO order.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            position: self.position,
            lookahead: self.lookahead.clone(),
            pending_resume: self.pending_resume,
        }
    }

    /// Rewinds the cursor to the state saved in `checkpoint`.
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn restore(&mut self, checkpoint: Checkpoint) {
        self.position = checkpoint.position;
        self.lookahead = checkpoint.lookahead;
        self.pending_resume = checkpoint.pending_resume;
    }

    /// Scans one more token from the source and appends it to the
    /// lookahead queue until the queue holds at least `wanted` tokens.
    ///
    /// Returns `Ok(true)` when the queue reaches (or already had)
    /// `wanted` tokens, `Ok(false)` when the source ran out before
    /// that, and `Err(e)` when a scan fails. On error the cursor's
    /// `position` is left at the error start and `pending_resume` is
    /// set; tokens already queued before the failure are preserved.
    fn ensure_lookahead(&mut self, wanted: usize) -> Result<bool, LexicalError> {
        while self.lookahead.len() < wanted {
            match scan_token(self.source.text(), self.position) {
                Ok(None) => return Ok(false),
                Ok(Some(token)) => {
                    self.position = token.end();
                    self.lookahead.push_back(token);
                }
                Err(e) => {
                    let span = SourceSpan::new(self.source_id, e.position, e.resume_position);
                    self.pending_resume = Some(e.resume_position);
                    return Err(LexicalError {
                        span,
                        kind: e.kind,
                        resume_position: e.resume_position,
                    });
                }
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use erl_tokenize::{ErrorKind, TokenKind};

    use crate::source::{Source, SourceStore};

    fn make_cursor(text: &str) -> Cursor {
        let store = SourceStore::new();
        let id = store.append(Source::new("main.erl", text));
        Cursor::new(id, store.get(id))
    }

    #[test]
    fn empty_source_reports_eof() {
        let mut cursor = make_cursor("");
        assert!(cursor.is_at_eof());
        assert!(cursor.peek().is_none());
        assert!(cursor.bump().is_none());
    }

    #[test]
    fn bump_yields_tokens_in_source_order() {
        let mut cursor = make_cursor("foo bar");
        let mut kinds = Vec::new();
        while let Some(token) = cursor.bump() {
            kinds.push(token.expect("no lexical errors").kind());
        }
        assert_eq!(
            kinds,
            [TokenKind::Atom, TokenKind::Whitespace, TokenKind::Atom]
        );
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn peek_does_not_advance() {
        let mut cursor = make_cursor("foo");
        let peeked = cursor.peek().unwrap().unwrap();
        let peeked_again = cursor.peek().unwrap().unwrap();
        let bumped = cursor.bump().unwrap().unwrap();
        assert_eq!(peeked.start(), bumped.start());
        assert_eq!(peeked_again.start(), bumped.start());
    }

    #[test]
    fn utf8_atom_round_trips_through_value() {
        let text = "'日本語'";
        let mut cursor = make_cursor(text);
        let token = cursor.bump().unwrap().unwrap();
        assert_eq!(token.text(text), text);
    }

    #[test]
    fn peek_lexical_queues_hidden_tokens() {
        let mut cursor = make_cursor("% cmt\nfoo");
        let lexical = cursor.peek_lexical().unwrap().unwrap();
        assert_eq!(lexical.kind(), TokenKind::Atom);

        // Bumps should yield comment, whitespace, atom in source order.
        let a = cursor.bump().unwrap().unwrap();
        let b = cursor.bump().unwrap().unwrap();
        let c = cursor.bump().unwrap().unwrap();
        assert_eq!(a.kind(), TokenKind::Comment);
        assert_eq!(b.kind(), TokenKind::Whitespace);
        assert_eq!(c.kind(), TokenKind::Atom);
        assert_eq!(c.start(), lexical.start());
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn checkpoint_and_restore_rewind_stream() {
        let mut cursor = make_cursor("foo bar baz");
        let first = cursor.bump().unwrap().unwrap();
        let saved = cursor.checkpoint();
        let _ = cursor.bump().unwrap();
        let _ = cursor.bump().unwrap();

        cursor.restore(saved);
        let after_restore = cursor.bump().unwrap().unwrap();
        assert_ne!(first.start(), after_restore.start());
        assert_eq!(after_restore.kind(), TokenKind::Whitespace);

        // Continue scanning to the end.
        while cursor.bump().is_some() {}
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn nested_checkpoints_are_lifo() {
        let mut cursor = make_cursor("a b c");
        let outer = cursor.checkpoint();
        let _ = cursor.bump().unwrap(); // 'a'
        let inner = cursor.checkpoint();
        let _ = cursor.bump().unwrap(); // ws
        let _ = cursor.bump().unwrap(); // 'b'

        cursor.restore(inner);
        let after_inner = cursor.bump().unwrap().unwrap();
        assert_eq!(after_inner.kind(), TokenKind::Whitespace);

        cursor.restore(outer);
        let after_outer = cursor.bump().unwrap().unwrap();
        assert_eq!(after_outer.kind(), TokenKind::Atom);
    }

    #[test]
    fn lexical_error_carries_span_and_resume_position() {
        // Unterminated string literal triggers a NoClosingQuotation
        // error.
        let mut cursor = make_cursor("\"oops");
        let err = cursor.bump().unwrap().unwrap_err();
        assert_eq!(err.kind, ErrorKind::NoClosingQuotation);
        assert_eq!(err.span.source_id, cursor.source_id());
        assert!(err.span.start.offset() < err.span.end.offset());
        assert_eq!(err.span.end, err.resume_position);
        assert_eq!(cursor.pending_resume(), Some(err.resume_position));
    }

    #[test]
    fn resume_advances_past_lexical_error() {
        let mut cursor = make_cursor("\"oops\nfoo");
        let err = cursor.bump().unwrap().unwrap_err();
        assert!(cursor.pending_resume().is_some());

        cursor.resume(err.resume_position);
        assert!(cursor.pending_resume().is_none());

        // After resume, the cursor should read the remaining tokens.
        let mut kinds = Vec::new();
        while let Some(token) = cursor.bump() {
            kinds.push(token.expect("recovered stream has no errors").kind());
        }
        assert!(kinds.contains(&TokenKind::Atom));
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn calling_bump_without_resume_re_emits_the_same_error() {
        let mut cursor = make_cursor("\"oops");
        let first = cursor.bump().unwrap().unwrap_err();
        let second = cursor.bump().unwrap().unwrap_err();
        assert_eq!(first.span, second.span);
        assert_eq!(first.resume_position, second.resume_position);
    }

    #[test]
    fn queued_hidden_tokens_survive_a_scan_error() {
        // A comment then an unterminated string. peek_lexical should
        // scan forward, queue the hidden tokens, and surface the error
        // because no lexical token is reachable.
        let mut cursor = make_cursor("% hidden\n\"oops");
        let peeked = cursor
            .peek_lexical()
            .expect("scan reached the error before EOF");
        assert!(peeked.is_err());

        // Drain the queued hidden tokens before the error re-emerges.
        let a = cursor.bump().unwrap().unwrap();
        let b = cursor.bump().unwrap().unwrap();
        assert_eq!(a.kind(), TokenKind::Comment);
        assert_eq!(b.kind(), TokenKind::Whitespace);

        // Now bump re-hits the error.
        let err = cursor.bump().unwrap().unwrap_err();
        assert_eq!(err.kind, ErrorKind::NoClosingQuotation);
    }
}
