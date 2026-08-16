//! Source text, source store, source identifier, and source span.
//!
//! These types form the storage layer that every token in [`crate::Preprocessed`]
//! refers to. See the crate-level rustdoc for how they compose.

use std::num::NonZeroU32;
use std::sync::{Arc, RwLock};

use erl_tokenize::Position;

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
    #[allow(dead_code, reason = "constructed by SourceStore::append")]
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

/// UTF-8 source text with a display name.
///
/// A `Source` is immutable once constructed; its text is not appended to
/// later. [`Source::clone`] shares the internal buffers without copying
/// the text.
///
/// The display name is an identifier (e.g. a file path, a URL, or an
/// editor buffer name). It is not required to be an existing file path.
/// Synthesized "pseudo" sources used for macro expansion also live here
/// with a display name that marks them as synthesized.
#[derive(Debug, Clone)]
pub struct Source {
    display_name: Arc<str>,
    text: Arc<str>,
}

impl Source {
    /// Creates a source with the given display name and text.
    pub fn new<N, T>(display_name: N, text: T) -> Self
    where
        N: Into<Arc<str>>,
        T: Into<Arc<str>>,
    {
        Self {
            display_name: display_name.into(),
            text: text.into(),
        }
    }

    /// Returns the display name.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// Returns the source text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Append-only store of [`Source`]s shared between a preprocessor and
/// its output [`crate::Preprocessed`].
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
    #[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
    pub(crate) fn append(&self, source: Source) -> SourceId {
        let mut sources = self.sources.write().expect("SourceStore lock was poisoned");
        let index = sources.len();
        sources.push(Arc::new(source));
        SourceId::from_index(index)
    }

    /// Appends a synthesized pseudo source and returns its identifier.
    ///
    /// The display name should mark the source as synthesized so that
    /// callers can distinguish it from real files
    /// (e.g. `"<synth:?FILE at main.erl:3>"`).
    #[allow(dead_code, reason = "invoked by macro expansion internals to be added")]
    pub(crate) fn append_pseudo<N, T>(&self, display_name: N, text: T) -> SourceId
    where
        N: Into<Arc<str>>,
        T: Into<Arc<str>>,
    {
        self.append(Source::new(display_name, text))
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
/// This is the common shape used by [`crate::Preprocessed`] helpers and,
/// where needed, by later error, include, conditional, and diagnostic
/// APIs so that ad hoc types do not proliferate.
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

    #[test]
    fn clone_shares_text() {
        let src = Source::new("main.erl", "foo");
        let cloned = src.clone();
        assert!(std::ptr::eq(src.text(), cloned.text()));
        assert!(std::ptr::eq(src.display_name(), cloned.display_name()));
    }

    #[test]
    fn append_and_get() {
        let store = SourceStore::new();
        assert!(store.is_empty());

        let id_a = store.append(Source::new("a.erl", "a"));
        let id_b = store.append(Source::new("b.erl", "bb"));
        assert_ne!(id_a, id_b);
        assert_eq!(store.len(), 2);

        assert_eq!(store.get(id_a).text(), "a");
        assert_eq!(store.get(id_b).text(), "bb");
        assert_eq!(store.get(id_a).display_name(), "a.erl");
    }

    #[test]
    fn append_pseudo() {
        let store = SourceStore::new();
        let id = store.append_pseudo("<synth:?FILE at main.erl:3>", "\"main.erl\"");
        assert_eq!(store.get(id).text(), "\"main.erl\"");
        assert_eq!(store.get(id).display_name(), "<synth:?FILE at main.erl:3>");
    }

    #[test]
    fn stable_addresses_after_further_append() {
        let store = SourceStore::new();
        let id = store.append(Source::new("a.erl", "hello"));
        let first = store.get(id);
        let first_ptr = first.text().as_ptr();

        for i in 0..64 {
            store.append(Source::new(format!("f{i}.erl"), format!("body {i}")));
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

        let id = store.append(Source::new("main.erl", "-module(m)."));
        assert_eq!(fork.get(id).text(), "-module(m).");

        let fork_id = fork.append(Source::new("inc.hrl", "-define(X, 1)."));
        assert_eq!(store.get(fork_id).text(), "-define(X, 1).");
        assert_eq!(store.len(), 2);
        assert_eq!(fork.len(), 2);
    }
}
