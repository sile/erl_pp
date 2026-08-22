//! Bundle of a decoded string with the source span it was scanned
//! from.
//!
//! Directive payloads and per-token diagnostics use this shape
//! repeatedly (macro name, parameter name, include path, and so on).
//! The bundle exists so those cases share a single type instead of
//! carrying parallel `name` + `name_span` fields.

use std::fmt;

use crate::source::SourceSpan;

/// A decoded string paired with the [`SourceSpan`] of the token (or
/// contiguous tokens) it was scanned from.
///
/// This is not a general "string + span" wrapper; it is used only for
/// values that come from decoding a source token so callers can
/// inspect both the decoded text and the original span in one place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceString {
    /// Decoded string value.
    pub value: String,
    /// Source span the value was scanned from.
    pub span: SourceSpan,
}

impl SourceString {
    /// Builds a bundle from a decoded value and its source span.
    pub fn new<V>(value: V, span: SourceSpan) -> Self
    where
        V: Into<String>,
    {
        Self {
            value: value.into(),
            span,
        }
    }

    /// Returns the decoded string as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for SourceString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Source, SourceStore};

    fn dummy_span() -> SourceSpan {
        let store = SourceStore::new();
        let id = store.append(Source::new(
            "m.erl",
            "foo",
            erl_tokenize::scan_tokens("foo").expect("test input must scan without lex errors"),
        ));
        SourceSpan::new(
            id,
            erl_tokenize::Position::new(),
            erl_tokenize::Position::new(),
        )
    }

    #[test]
    fn new_and_accessors() {
        let span = dummy_span();
        let s = SourceString::new("foo", span);
        assert_eq!(s.value, "foo");
        assert_eq!(s.as_str(), "foo");
        assert_eq!(s.span, span);
    }

    #[test]
    fn new_accepts_owned_string() {
        let span = dummy_span();
        let s = SourceString::new(String::from("bar"), span);
        assert_eq!(s.value, "bar");
    }

    #[test]
    fn display_prints_the_value() {
        let s = SourceString::new("baz", dummy_span());
        assert_eq!(format!("{s}"), "baz");
    }

    #[test]
    fn eq_compares_both_fields() {
        let span = dummy_span();
        let a = SourceString::new("x", span);
        let b = SourceString::new("x", span);
        assert_eq!(a, b);
    }
}
