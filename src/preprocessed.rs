//! Preprocessor output container.
//!
//! [`Preprocessed`] holds the token stream, the source store shared with
//! the preprocessor, and the per-token side tables that map each token
//! to its source and its [`crate::Origin`]. It is append-only during
//! construction and treated as immutable afterwards.

use std::sync::Arc;

use erl_tokenize::{Token, TokenValue};

use crate::origin::Origin;
use crate::source::{Source, SourceId, SourceSpan, SourceStore};

/// Append-only container that a preprocessor fills in during its
/// [`Sans-I/O`] action loop and hands to the caller when the loop
/// finishes.
///
/// The token type is [`erl_tokenize::Token`] as-is; there is no
/// per-token wrapper. Callers that want provenance or source-backed
/// text obtain them by index through the accessor methods on this
/// container.
///
/// After construction the three side tables (tokens, source ids and
/// origins) are treated as immutable. The [`Arc<SourceStore>`] shared
/// with the preprocessor may keep growing if other preprocessors that
/// share the same store keep appending sources, but every [`Source`]
/// is itself immutable, so text references handed out by
/// [`Preprocessed::text`] remain valid.
///
/// [`Sans-I/O`]: https://sans-io.readthedocs.io/
#[derive(Debug, Clone)]
pub struct Preprocessed {
    tokens: Vec<Token>,
    sources: Arc<SourceStore>,
    /// Per-token identifier of the source the token was scanned from.
    source_ids: Vec<SourceId>,
    /// Per-token cache of the [`Source`] handle, kept so that
    /// [`Preprocessed::text`] and [`Preprocessed::value`] can return
    /// borrows tied to `&self`.
    source_arcs: Vec<Arc<Source>>,
    origins: Vec<Origin>,
}

impl Preprocessed {
    /// Creates an empty container backed by the given store.
    #[allow(
        dead_code,
        reason = "constructed by preprocessor internals to be added"
    )]
    pub(crate) fn new(sources: Arc<SourceStore>) -> Self {
        Self {
            tokens: Vec::new(),
            sources,
            source_ids: Vec::new(),
            source_arcs: Vec::new(),
            origins: Vec::new(),
        }
    }

    /// Appends a token together with its source and origin, and
    /// returns the token index the append created.
    ///
    /// The three side tables are updated in a single call so their
    /// lengths cannot drift apart. This method is `pub(crate)` because
    /// only preprocessor internals should build a [`Preprocessed`];
    /// external callers observe the finished container.
    ///
    /// # Panics
    ///
    /// Panics if `source_id` was not issued by the source store this
    /// container is backed by.
    pub(crate) fn append(&mut self, token: Token, source_id: SourceId, origin: Origin) -> usize {
        let source = self.sources.get(source_id);
        let index = self.tokens.len();
        self.tokens.push(token);
        self.source_ids.push(source_id);
        self.source_arcs.push(source);
        self.origins.push(origin);
        index
    }

    /// Returns a shared handle to the underlying source store.
    pub fn sources(&self) -> &Arc<SourceStore> {
        &self.sources
    }

    /// Returns the token stream as a slice.
    ///
    /// Use standard slice APIs (`len`, `iter`, `is_empty`, `windows`,
    /// and so on) to work with the tokens.
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Returns the per-token identifiers of the source each token was
    /// scanned from.
    ///
    /// The slice has the same length as [`tokens`](Self::tokens); index
    /// `i` refers to the same token as `tokens()[i]`.
    pub fn source_ids(&self) -> &[SourceId] {
        &self.source_ids
    }

    /// Returns the per-token origins.
    ///
    /// The slice has the same length as [`tokens`](Self::tokens); index
    /// `i` refers to the same token as `tokens()[i]`.
    pub fn origins(&self) -> &[Origin] {
        &self.origins
    }

    /// Returns the substring of the source that the token at `index`
    /// covers.
    ///
    /// The returned slice borrows from the [`Source`] cached in this
    /// container, so its lifetime is tied to `&self`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.tokens().len()`.
    pub fn text(&self, index: usize) -> &str {
        self.tokens[index].text(self.source_arcs[index].text())
    }

    /// Decodes the value of the token at `index`.
    ///
    /// See [`erl_tokenize::Token::value`] for the borrowed/owned
    /// contract of each variant. The returned [`TokenValue`] borrows
    /// from the [`Source`] cached in this container.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.tokens().len()`.
    pub fn value(&self, index: usize) -> TokenValue<'_> {
        self.tokens[index].value(self.source_arcs[index].text())
    }

    /// Returns the [`SourceSpan`] covered by the token at `index`.
    ///
    /// # Panics
    ///
    /// Panics if `index >= self.tokens().len()`.
    pub fn source_span(&self, index: usize) -> SourceSpan {
        SourceSpan::new(
            self.source_ids[index],
            self.tokens[index].start(),
            self.tokens[index].end(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use erl_tokenize::{Position, TokenKind, scan_token};

    use crate::origin::Origin;
    use crate::source::Source;

    fn scan_all(text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut pos = Position::new();
        while let Some(token) = scan_token(text, pos).expect("scan failed") {
            pos = token.end();
            tokens.push(token);
        }
        tokens
    }

    #[test]
    fn append_and_read_ascii() {
        let store = Arc::new(SourceStore::new());
        let text = "foo bar";
        let source_id = store.append(Source::new("main.erl", text));

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scan_all(text) {
            out.append(token, source_id, Origin::Source);
        }

        assert_eq!(out.tokens().len(), 3);
        assert_eq!(out.text(0), "foo");
        assert_eq!(out.text(1), " ");
        assert_eq!(out.text(2), "bar");
        assert!(out.tokens()[1].kind().is_hidden());
        assert!(!out.tokens()[0].kind().is_hidden());
    }

    #[test]
    fn append_and_read_utf8() {
        let store = Arc::new(SourceStore::new());
        let text = "'日本語'";
        let source_id = store.append(Source::new("m.erl", text));

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scan_all(text) {
            out.append(token, source_id, Origin::Source);
        }

        assert_eq!(out.tokens().len(), 1);
        assert_eq!(out.text(0), "'日本語'");
        match out.value(0) {
            TokenValue::Atom(atom) => assert_eq!(atom.as_ref(), "日本語"),
            other => panic!("expected Atom, got {other:?}"),
        }
    }

    #[test]
    fn hidden_tokens_preserved() {
        let store = Arc::new(SourceStore::new());
        let text = "% comment\nfoo";
        let source_id = store.append(Source::new("m.erl", text));

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scan_all(text) {
            out.append(token, source_id, Origin::Source);
        }

        let kinds: Vec<_> = out.tokens().iter().map(|t| t.kind()).collect();
        assert_eq!(
            kinds,
            [TokenKind::Comment, TokenKind::Whitespace, TokenKind::Atom]
        );
        assert_eq!(out.text(0), "% comment");
        assert_eq!(out.text(1), "\n");
        assert_eq!(out.text(2), "foo");
    }

    #[test]
    fn synthetic_pseudo_source_round_trip() {
        let store = Arc::new(SourceStore::new());
        let real = store.append(Source::new("main.erl", "-module(m)."));
        let pseudo_text = "\"main.erl\"";
        let pseudo = store.append_pseudo("<synth:?FILE at main.erl:1>", pseudo_text);

        let real_tokens = scan_all("-module(m).");
        let pseudo_tokens = scan_all(pseudo_text);
        assert_eq!(pseudo_tokens.len(), 1);
        let pseudo_parent = Arc::new(Origin::Source);

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in real_tokens {
            out.append(token, real, Origin::Source);
        }
        for token in pseudo_tokens {
            out.append(
                token,
                pseudo,
                Origin::Predefined(Arc::clone(&pseudo_parent)),
            );
        }

        let last = out.tokens().len() - 1;
        assert!(matches!(out.origins()[last], Origin::Predefined(_)));
        assert_eq!(out.source_ids()[last], pseudo);
        assert_eq!(out.text(last), "\"main.erl\"");
        match out.value(last) {
            TokenValue::String(cow) => assert_eq!(cow.as_ref(), "main.erl"),
            other => panic!("expected String, got {other:?}"),
        }
    }

    #[test]
    fn source_span_carries_source_and_positions() {
        let store = Arc::new(SourceStore::new());
        let text = "foo";
        let source_id = store.append(Source::new("m.erl", text));

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scan_all(text) {
            out.append(token, source_id, Origin::Source);
        }

        let span = out.source_span(0);
        assert_eq!(span.source_id, source_id);
        assert_eq!(span.start.offset(), 0);
        assert_eq!(span.end.offset(), text.len());
    }

    #[test]
    fn text_references_survive_store_growth() {
        let store = Arc::new(SourceStore::new());
        let text = "foo";
        let source_id = store.append(Source::new("m.erl", text));

        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scan_all(text) {
            out.append(token, source_id, Origin::Source);
        }

        // Grow the store from another handle. This mimics a forked
        // preprocessor appending sources after `out` was built.
        let fork = Arc::clone(&store);
        for i in 0..32 {
            fork.append(Source::new(format!("extra{i}.erl"), format!("body {i}")));
        }

        // The cached Arc<Source> in Preprocessed keeps the text alive.
        assert_eq!(out.text(0), "foo");
    }

    #[test]
    fn tokens_slice_matches_append_order() {
        let store = Arc::new(SourceStore::new());
        let text = "a b c";
        let source_id = store.append(Source::new("m.erl", text));

        let scanned = scan_all(text);
        let mut out = Preprocessed::new(Arc::clone(&store));
        for token in scanned.iter().copied() {
            out.append(token, source_id, Origin::Source);
        }

        assert_eq!(out.tokens(), scanned.as_slice());
    }
}
