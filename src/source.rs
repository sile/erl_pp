//! Source text, source store, source identifier, and source span.
//!
//! These types form the storage layer that every
//! [`crate::PreprocessedToken`] refers to. See the crate-level rustdoc
//! for how they compose.

use std::num::NonZeroU32;
use std::sync::{Arc, RwLock};

use erl_tokenize::{Position, Token, scan_token};

use crate::error::LexicalError;

/// Identifier of a [`Source`] inside a [`SourceStore`].
///
/// Values are only meaningful inside the store that issued them; do
/// not compare identifiers from different stores.
//
// Internally represented as `NonZeroU32` so that `Option<SourceId>`
// fits in four bytes and 0 is unavailable as a valid handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceId(NonZeroU32);

impl SourceId {
    fn from_index(index: usize) -> Self {
        let one_based = u32::try_from(index)
            .ok()
            .and_then(|n| n.checked_add(1))
            .and_then(NonZeroU32::new)
            .expect("SourceStore holds too many sources to fit in a SourceId");
        Self(one_based)
    }

    fn index(self) -> usize {
        (self.0.get() - 1) as usize
    }
}

/// UTF-8 source text with a display name and its pre-scanned token
/// stream.
///
/// A `Source` is immutable once constructed; its text and its token
/// stream are not appended to later. [`Source::clone`] shares the
/// internal buffers without copying the text or the tokens.
///
/// The display name is an identifier (e.g. a file path, a URL, or an
/// editor buffer name). It is not required to be an existing file path.
/// Synthesized "pseudo" sources used for macro expansion also live here
/// with a display name that marks them as synthesized.
///
/// Preprocessor consumers own tokenization. Use [`Source::from_text`]
/// as a convenience to have the crate scan the text on your behalf, or
/// [`Source::new`] to hand in a token stream you already produced (for
/// example from a shared cache or a differently configured tokenizer).
#[derive(Debug, Clone)]
pub struct Source {
    display_name: Arc<str>,
    text: Arc<str>,
    tokens: Arc<Vec<Token>>,
}

impl Source {
    /// Creates a source with the given display name, text, and
    /// pre-scanned token stream.
    ///
    /// The tokens must have been scanned from `text` (their internal
    /// offsets are indexed into that same string). No consistency
    /// check is performed; passing tokens scanned from a different
    /// text produces incorrect spans and decoded values.
    pub fn new<N, T>(display_name: N, text: T, tokens: Vec<Token>) -> Self
    where
        N: Into<Arc<str>>,
        T: Into<Arc<str>>,
    {
        Self {
            display_name: display_name.into(),
            text: text.into(),
            tokens: Arc::new(tokens),
        }
    }

    /// Creates a source with the given display name and text, scanning
    /// `text` into tokens with [`erl_tokenize::scan_token`].
    ///
    /// Returns [`LexicalError`] when the scan fails; the error carries
    /// the display name so the caller can report which source failed.
    pub fn from_text<N, T>(display_name: N, text: T) -> Result<Self, LexicalError>
    where
        N: Into<Arc<str>>,
        T: Into<Arc<str>>,
    {
        let display_name = display_name.into();
        let text = text.into();
        let tokens = scan_all(&display_name, &text)?;
        Ok(Self {
            display_name,
            text,
            tokens: Arc::new(tokens),
        })
    }

    /// Returns the display name.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the source text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the pre-scanned token stream (in source order,
    /// including hidden tokens like comments and whitespace).
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }
}

fn scan_all(display_name: &Arc<str>, text: &str) -> Result<Vec<Token>, LexicalError> {
    let mut tokens = Vec::new();
    let mut position = Position::new();
    loop {
        match scan_token(text, position) {
            Ok(None) => return Ok(tokens),
            Ok(Some(token)) => {
                position = token.end();
                tokens.push(token);
            }
            Err(e) => {
                return Err(LexicalError {
                    display_name: Arc::clone(display_name),
                    position: e.position,
                    kind: e.kind,
                    resume_position: e.resume_position,
                });
            }
        }
    }
}

/// Append-only store of [`Source`]s shared between a preprocessor and
/// the [`crate::PreprocessedToken`]s it emits.
///
/// A store is designed to be wrapped in an [`Arc`] and shared with any
/// preprocessor forks. Appending is done through a `&self` method with
/// internal synchronization, so a fork and its parent can add sources
/// concurrently without external locking.
///
/// [`SourceId`] values that have already been issued keep their meaning
/// after further appends. The internal storage uses
/// [`Arc<Source>`] slots so that a reference obtained via [`get`] is
/// unaffected by later appends.
///
/// [`get`]: SourceStore::get
#[derive(Debug, Default)]
pub struct SourceStore {
    sources: RwLock<Vec<Arc<Source>>>,
}

impl SourceStore {
    /// Creates an empty source store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a source and returns its identifier.
    pub(crate) fn append(&self, source: Source) -> SourceId {
        let mut sources = self.sources.write().expect("SourceStore lock was poisoned");
        let index = sources.len();
        sources.push(Arc::new(source));
        SourceId::from_index(index)
    }

    /// Appends a synthesized pseudo source built from raw text and
    /// returns its identifier.
    ///
    /// The text is scanned with [`erl_tokenize::scan_token`]; a lex
    /// failure surfaces as [`LexicalError`] and the store is left
    /// unchanged.
    ///
    /// The display name should mark the source as synthesized so that
    /// callers can distinguish it from real files
    /// (e.g. `"<synth:?FILE at main.erl:3>"`).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by later work on ?FILE / ?LINE macro expansion"
        )
    )]
    pub(crate) fn append_pseudo<N, T>(
        &self,
        display_name: N,
        text: T,
    ) -> Result<SourceId, LexicalError>
    where
        N: Into<Arc<str>>,
        T: Into<Arc<str>>,
    {
        Ok(self.append(Source::from_text(display_name, text)?))
    }

    /// Returns a shared handle to the source with the given identifier.
    ///
    /// # Panics
    ///
    /// Panics if `id` was not issued by this store.
    pub fn get(&self, id: SourceId) -> Arc<Source> {
        let sources = self.sources.read().expect("SourceStore lock was poisoned");
        Arc::clone(
            sources
                .get(id.index())
                .expect("SourceId was not issued by this SourceStore"),
        )
    }

    /// Returns the number of sources currently in the store.
    pub fn len(&self) -> usize {
        self.sources
            .read()
            .expect("SourceStore lock was poisoned")
            .len()
    }

    /// Returns `true` if the store has no sources.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Half-open range within a specific [`Source`].
///
/// This is the common shape used by
/// [`crate::PreprocessedToken::source_span`] and, where needed, by
/// later error, include, conditional, and diagnostic APIs so that ad
/// hoc types do not proliferate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    /// Identifier of the source that this span lies in.
    pub source_id: SourceId,
    /// Inclusive start position within the source.
    pub start: Position,
    /// Exclusive end position within the source.
    pub end: Position,
}

impl SourceSpan {
    /// Creates a span from its components.
    pub const fn new(source_id: SourceId, start: Position, end: Position) -> Self {
        Self {
            source_id,
            start,
            end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_text(name: &str, text: &str) -> Source {
        Source::from_text(name, text).expect("valid tokens")
    }

    #[test]
    fn clone_shares_text() {
        let src = from_text("main.erl", "foo");
        let cloned = src.clone();
        assert!(std::ptr::eq(src.text(), cloned.text()));
        assert!(std::ptr::eq(src.display_name(), cloned.display_name()));
        assert!(std::ptr::eq(
            src.tokens().as_ptr(),
            cloned.tokens().as_ptr()
        ));
    }

    #[test]
    fn from_text_scans_tokens() {
        let src = from_text("main.erl", "foo bar");
        // foo, whitespace, bar
        assert_eq!(src.tokens().len(), 3);
    }

    #[test]
    fn from_text_surfaces_lex_error() {
        let err = Source::from_text("main.erl", "\"oops").expect_err("lex error expected");
        assert_eq!(&*err.display_name, "main.erl");
        assert!(err.position.offset() < err.resume_position.offset());
    }

    #[test]
    fn new_accepts_external_tokens() {
        // Same tokens as would come from from_text but constructed via
        // Source::new to prove the two paths converge.
        let scanned = from_text("main.erl", "foo bar");
        let by_new = Source::new("main.erl", "foo bar", scanned.tokens().to_vec());
        assert_eq!(by_new.tokens().len(), scanned.tokens().len());
    }

    #[test]
    fn append_and_get() {
        let store = SourceStore::new();
        assert!(store.is_empty());

        let id_a = store.append(from_text("a.erl", "a"));
        let id_b = store.append(from_text("b.erl", "bb"));
        assert_ne!(id_a, id_b);
        assert_eq!(store.len(), 2);

        assert_eq!(store.get(id_a).text(), "a");
        assert_eq!(store.get(id_b).text(), "bb");
        assert_eq!(store.get(id_a).display_name(), "a.erl");
    }

    #[test]
    fn append_pseudo_scans_text() {
        let store = SourceStore::new();
        let id = store
            .append_pseudo("<synth:?FILE at main.erl:3>", "\"main.erl\"")
            .expect("valid tokens");
        assert_eq!(store.get(id).text(), "\"main.erl\"");
        assert_eq!(store.get(id).display_name(), "<synth:?FILE at main.erl:3>");
        assert!(!store.get(id).tokens().is_empty());
    }

    #[test]
    fn append_pseudo_lex_error_leaves_store_unchanged() {
        let store = SourceStore::new();
        let err = store
            .append_pseudo("<synth>", "\"oops")
            .expect_err("lex error expected");
        assert!(store.is_empty());
        assert_eq!(&*err.display_name, "<synth>");
    }

    #[test]
    fn stable_addresses_after_further_append() {
        let store = SourceStore::new();
        let id = store.append(from_text("a.erl", "hello"));
        let first = store.get(id);
        let first_ptr = first.text().as_ptr();

        for i in 0..64 {
            store.append(from_text(&format!("f{i}.erl"), &format!("body {i}")));
        }

        let after = store.get(id);
        assert_eq!(after.text(), "hello");
        assert_eq!(after.text().as_ptr(), first_ptr);
        assert!(Arc::ptr_eq(&first, &after));
    }

    #[test]
    fn fork_semantics_shared_store() {
        let store = Arc::new(SourceStore::new());
        let fork = Arc::clone(&store);

        let id = store.append(from_text("main.erl", "-module(m)."));
        assert_eq!(fork.get(id).text(), "-module(m).");

        let fork_id = fork.append(from_text("inc.hrl", "-define(X, 1)."));
        assert_eq!(store.get(fork_id).text(), "-define(X, 1).");
        assert_eq!(store.len(), 2);
        assert_eq!(fork.len(), 2);
    }
}
