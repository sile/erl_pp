//! Bundled source-token payload.
//!
//! Each [`SourceToken`] carries the raw [`Token`] together with the
//! [`Arc<Source>`] it indexes, its [`SourceId`], and its
//! [`Origin`](crate::Origin). [`Source`] is the buffer the token's offsets
//! refer to; [`Origin`](crate::Origin) is why the token appears (written in
//! that source, copied from a macro body, synthesized for `?FILE`,
//! and so on). It is the payload of [`Event::Token`](crate::Event::Token), so a
//! caller receives everything needed to inspect the token in one
//! value.
//!
//! Preprocessor callers that want to keep the full token stream
//! accumulate the tokens themselves; the preprocessor does not retain
//! them internally.

use std::sync::Arc;

use erl_tokenize::{Token, TokenValue};

use crate::origin::Origin;
use crate::source::{Source, SourceId, SourceSpan};

/// A scanned [`Token`] together with the [`Source`] it indexes and
/// the [`Origin`] the preprocessor assigned to it.
///
/// [`Source`] is the buffer the token's offsets refer to, not a claim
/// that [`origin`](Self::origin) is [`Origin::Source`]. A token copied
/// from a macro body still indexes the definition's [`Source`].
///
/// Values of this type are self-contained: they carry an
/// [`Arc<Source>`] rather than an index, so a caller can hold and
/// inspect a token without borrowing back into the preprocessor.
#[derive(Debug, Clone)]
pub struct SourceToken {
    token: Token,
    source: Arc<Source>,
    source_id: SourceId,
    origin: Origin,
}

impl SourceToken {
    /// Creates a source-token bundle from its components.
    ///
    /// This is `pub(crate)` because only preprocessor internals build
    /// these; external callers observe them as the payload of
    /// [`Event::Token`](crate::Event::Token).
    pub(crate) fn new(
        token: Token,
        source: Arc<Source>,
        source_id: SourceId,
        origin: Origin,
    ) -> Self {
        Self {
            token,
            source,
            source_id,
            origin,
        }
    }

    /// Returns the underlying [`Token`].
    pub fn token(&self) -> &Token {
        &self.token
    }

    /// Returns the substring of the source that this token covers.
    ///
    /// The returned slice borrows from the [`Source`] this bundle owns
    /// a handle to, so its lifetime is tied to `&self`.
    pub fn text(&self) -> &str {
        self.token.text(self.source.text())
    }

    /// Decodes the value of this token.
    ///
    /// See [`erl_tokenize::Token::value`] for the borrowed/owned
    /// contract of each variant. The returned [`TokenValue`] borrows
    /// from the [`Source`] this bundle owns a handle to.
    pub fn value(&self) -> TokenValue<'_> {
        self.token.value(self.source.text())
    }

    /// Returns a shared handle to the [`Source`] this token indexes.
    pub fn source(&self) -> &Arc<Source> {
        &self.source
    }

    /// Returns the [`SourceSpan`] this token covers.
    ///
    /// The span carries the [`SourceId`] and the token's start and end
    /// positions.
    pub fn source_span(&self) -> SourceSpan {
        SourceSpan::new(self.source_id, self.token.start(), self.token.end())
    }

    /// Returns the [`Origin`] the preprocessor assigned to this token.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use erl_tokenize::{Position, scan_token};

    use crate::source::SourceStore;

    fn scan_one(text: &str) -> Token {
        scan_token(text, Position::new())
            .expect("scan failed")
            .expect("expected at least one token")
    }

    #[test]
    fn methods_expose_bundled_state() {
        let store = SourceStore::new();
        let text = "foo";
        let source_id = store.append(Source::from_text("m.erl", text));
        let source = store.get(source_id);
        let token = scan_one(text);

        let tok = SourceToken::new(token, Arc::clone(&source), source_id, Origin::Source);

        assert_eq!(*tok.token(), token);
        assert_eq!(tok.text(), "foo");
        assert!(matches!(tok.value(), TokenValue::Atom(a) if a.as_ref() == "foo"));
        assert!(Arc::ptr_eq(tok.source(), &source));
        let span = tok.source_span();
        assert_eq!(span.source_id, source_id);
        assert_eq!(span.start.offset(), 0);
        assert_eq!(span.end.offset(), text.len());
        assert!(matches!(tok.origin(), Origin::Source));
    }

    #[test]
    fn text_survives_source_store_growth() {
        let store = Arc::new(SourceStore::new());
        let text = "bar";
        let source_id = store.append(Source::from_text("m.erl", text));
        let source = store.get(source_id);
        let token = scan_one(text);

        let tok = SourceToken::new(token, Arc::clone(&source), source_id, Origin::Source);

        // Grow the store from another handle; the token still resolves
        // through the Arc<Source> it captured.
        for i in 0..32 {
            store.append(Source::from_text(
                format!("extra{i}.erl"),
                format!("body {i}"),
            ));
        }
        assert_eq!(tok.text(), "bar");
    }

    #[test]
    fn clone_shares_source_arc() {
        let store = SourceStore::new();
        let source_id = store.append(Source::from_text("m.erl", "baz"));
        let source = store.get(source_id);
        let token = scan_one("baz");

        let tok = SourceToken::new(token, Arc::clone(&source), source_id, Origin::Source);
        let cloned = tok.clone();
        assert!(Arc::ptr_eq(tok.source(), cloned.source()));
        assert_eq!(cloned.text(), "baz");
    }
}
