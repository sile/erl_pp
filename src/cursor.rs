//! Single-source token stream walker with lookahead and rollback.
//!
//! [`Cursor`] walks the pre-scanned token stream of one
//! [`Source`](crate::Source) and hands the tokens to the caller in the
//! order they appear, including comments and whitespace. It supports
//! lookahead and snapshot-based rollback.
//!
//! Tokenization is not this module's job. The tokens are produced up
//! front by the caller and handed to [`Source::new`](crate::Source::new); the
//! cursor merely indexes into that stored token slice.
//!
//! Multi-source concerns like `-include` are the state machine's job;
//! the state machine builds a new [`Cursor`] for each entered source
//! and pushes the parent cursor onto its own stack.

use std::sync::Arc;

use erl_tokenize::Token;

use crate::source::{Source, SourceId};

/// Walks one [`Source`]'s token stream with lookahead and
/// checkpoint/rollback.
///
/// A `Cursor` is created with the [`SourceId`] of the source it walks
/// and an [`Arc<Source>`] handle so it never has to re-query the
/// [`SourceStore`](crate::SourceStore) during scanning.
///
/// `Cursor` derives [`Clone`] so that a state machine can fork its
/// scanning state; the clone owns an independent read index.
#[derive(Clone)]
pub(crate) struct Cursor {
    source_id: SourceId,
    source: Arc<Source>,
    /// Index of the next token to hand out from `source.tokens()`.
    index: usize,
}

/// Snapshot of a [`Cursor`]'s mutable state, taken by
/// [`Cursor::checkpoint`] and used to rewind the cursor via
/// [`Cursor::restore`].
///
/// The [`SourceId`] and the [`Arc<Source>`] are not captured because
/// they never change during a cursor's life.
#[derive(Clone, Copy)]
pub(crate) struct Checkpoint {
    index: usize,
}

impl Cursor {
    /// Creates a new cursor positioned at the start of `source`.
    pub(crate) fn new(source_id: SourceId, source: Arc<Source>) -> Self {
        Self {
            source_id,
            source,
            index: 0,
        }
    }

    /// Returns the identifier of the source the cursor walks.
    pub(crate) fn source_id(&self) -> SourceId {
        self.source_id
    }

    /// Returns the source text the cursor walks.
    ///
    /// Callers pass this string to [`erl_tokenize::Token::text`] or
    /// [`erl_tokenize::Token::value`] to decode a token that this
    /// cursor produced.
    pub(crate) fn source_text(&self) -> &str {
        self.source.text()
    }

    /// Returns a shared handle to the source the cursor walks.
    pub(crate) fn source(&self) -> &Arc<Source> {
        &self.source
    }

    /// Returns `true` when the cursor has consumed the whole source.
    pub(crate) fn is_at_eof(&self) -> bool {
        self.index >= self.source.tokens().len()
    }

    /// Returns the next token, including hidden tokens (comments and
    /// whitespace), without advancing the cursor.
    ///
    /// `Some(token)` yields the next token, `None` marks
    /// end-of-source. Multiple calls with no intervening
    /// [`bump`](Self::bump) return the same token.
    pub(crate) fn peek(&self) -> Option<Token> {
        self.source.tokens().get(self.index).copied()
    }

    /// Returns the next lexical (non-hidden) token without advancing
    /// the cursor.
    ///
    /// Hidden tokens between the current position and the returned
    /// lexical token stay unread; a following [`bump`](Self::bump)
    /// yields them in source order before reaching the lexical token.
    /// `None` when no lexical token remains.
    pub(crate) fn peek_lexical(&self) -> Option<Token> {
        let tokens = self.source.tokens();
        let mut i = self.index;
        while let Some(token) = tokens.get(i) {
            if token.kind().is_lexical() {
                return Some(*token);
            }
            i += 1;
        }
        None
    }

    /// Consumes and returns the next token, including hidden tokens.
    ///
    /// `Some(token)` yields the token, `None` marks end-of-source.
    pub(crate) fn bump(&mut self) -> Option<Token> {
        let token = self.source.tokens().get(self.index).copied();
        if token.is_some() {
            self.index += 1;
        }
        token
    }

    /// Captures the cursor's mutable state so it can be restored later
    /// with [`restore`](Self::restore).
    ///
    /// Multiple checkpoints can be taken and restored in LIFO order.
    pub(crate) fn checkpoint(&self) -> Checkpoint {
        Checkpoint { index: self.index }
    }

    /// Rewinds the cursor to the state saved in `checkpoint`.
    pub(crate) fn restore(&mut self, checkpoint: Checkpoint) {
        self.index = checkpoint.index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use erl_tokenize::TokenKind;

    use crate::source::{Source, SourceStore};

    fn make_cursor(text: &str) -> Cursor {
        let store = SourceStore::new();
        let id = store.append(Source::from_text("main.erl", text));
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
            kinds.push(token.kind());
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
        let peeked = cursor.peek().expect("token available");
        let peeked_again = cursor.peek().expect("token available");
        let bumped = cursor.bump().expect("token available");
        assert_eq!(peeked.start(), bumped.start());
        assert_eq!(peeked_again.start(), bumped.start());
    }

    #[test]
    fn utf8_atom_round_trips_through_value() {
        let text = "'日本語'";
        let mut cursor = make_cursor(text);
        let token = cursor.bump().expect("token available");
        assert_eq!(token.text(text), text);
    }

    #[test]
    fn peek_lexical_skips_hidden_tokens() {
        let mut cursor = make_cursor("% cmt\nfoo");
        let lexical = cursor.peek_lexical().expect("token available");
        assert_eq!(lexical.kind(), TokenKind::Atom);

        // Bumps still yield comment, whitespace, atom in source order.
        let a = cursor.bump().expect("token available");
        let b = cursor.bump().expect("token available");
        let c = cursor.bump().expect("token available");
        assert_eq!(a.kind(), TokenKind::Comment);
        assert_eq!(b.kind(), TokenKind::Whitespace);
        assert_eq!(c.kind(), TokenKind::Atom);
        assert_eq!(c.start(), lexical.start());
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn checkpoint_and_restore_rewind_stream() {
        let mut cursor = make_cursor("foo bar baz");
        let first = cursor.bump().expect("token available");
        let saved = cursor.checkpoint();
        let _ = cursor.bump();
        let _ = cursor.bump();

        cursor.restore(saved);
        let after_restore = cursor.bump().expect("token available");
        assert_ne!(first.start(), after_restore.start());
        assert_eq!(after_restore.kind(), TokenKind::Whitespace);

        while cursor.bump().is_some() {}
        assert!(cursor.is_at_eof());
    }

    #[test]
    fn nested_checkpoints_are_lifo() {
        let mut cursor = make_cursor("a b c");
        let outer = cursor.checkpoint();
        let _ = cursor.bump(); // 'a'
        let inner = cursor.checkpoint();
        let _ = cursor.bump(); // ws
        let _ = cursor.bump(); // 'b'

        cursor.restore(inner);
        let after_inner = cursor.bump().expect("token available");
        assert_eq!(after_inner.kind(), TokenKind::Whitespace);

        cursor.restore(outer);
        let after_outer = cursor.bump().expect("token available");
        assert_eq!(after_outer.kind(), TokenKind::Atom);
    }
}
