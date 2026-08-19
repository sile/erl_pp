//! Origin metadata for preprocessed tokens.
//!
//! An [`Origin`] tells why a particular token appears in the output:
//! whether it came directly from the source text, from an include, from
//! a macro body, from a macro argument, from a stringification (`??Arg`),
//! or from a predefined macro (`?FILE`, `?LINE`, `?MACHINE`).

use std::sync::Arc;

use crate::source::SourceSpan;
use crate::source_string::SourceString;

/// Kind of predefined macro that a synthesized token came from.
///
/// Attached to [`Origin::Predefined`] so callers can distinguish the
/// three built-in predefined macros the preprocessor expands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PredefinedMacroKind {
    /// `?FILE` — the display name of the current source.
    File,
    /// `?LINE` — the line number at the call site.
    Line,
    /// `?MACHINE` — atom `'BEAM'`.
    Machine,
}

/// Provenance of a token emitted by the preprocessor.
///
/// The six variants match the token-provenance categories defined in
/// the parent redesign. Variants that can have a parent origin (every
/// variant except [`Origin::Source`]) hold the parent inside an
/// [`Arc<Origin>`] so that deep chains produced by nested macro
/// expansion or by macros inside include sources are structurally
/// shared and are not deep-copied when the enclosing state is cloned.
#[derive(Debug, Clone)]
pub enum Origin {
    /// Token was written directly in an input [`crate::Source`].
    ///
    /// This variant has no parent because it sits at the root of every
    /// provenance chain.
    Source,

    /// Token was written directly in an included source.
    ///
    /// The parent points at the origin of the `-include` /
    /// `-include_lib` directive that pulled the source in.
    Include(Arc<Origin>),

    /// Token was copied from the replacement body of a user-defined
    /// macro.
    MacroBody {
        /// Parent origin (the origin at the macro call site).
        parent: Arc<Origin>,
        /// Span covering the whole `?NAME(...)` call at the call site.
        call_site: SourceSpan,
        /// Span of the whole `-define(...)` directive the token came
        /// from (matches `MacroDefinition::directive_span`).
        definition_span: SourceSpan,
    },

    /// Token came from a macro argument that was substituted for a
    /// parameter in the replacement body.
    MacroArgument {
        /// Parent origin (the origin at the macro call site).
        parent: Arc<Origin>,
        /// Span covering the whole `?NAME(...)` call at the call site.
        call_site: SourceSpan,
        /// The parameter this token was substituted for.
        parameter: SourceString,
        /// Span of the whole `-define(...)` directive that declared the
        /// parameter (matches `MacroDefinition::directive_span`).
        definition_span: SourceSpan,
    },

    /// Token was synthesized by stringification (`??Arg`).
    Stringification {
        /// Parent origin (the origin at the macro call site).
        parent: Arc<Origin>,
        /// Span covering the whole `?NAME(...)` call at the call site.
        call_site: SourceSpan,
        /// The parameter that was stringified.
        parameter: SourceString,
        /// Span of the whole `-define(...)` directive that declared the
        /// parameter (matches `MacroDefinition::directive_span`).
        definition_span: SourceSpan,
    },

    /// Token was synthesized by a predefined macro
    /// (`?FILE`, `?LINE`, `?MACHINE`).
    Predefined {
        /// Parent origin (the origin at the predefined macro use site).
        parent: Arc<Origin>,
        /// Span covering the `?FILE` / `?LINE` / `?MACHINE` token pair
        /// at the call site.
        call_site: SourceSpan,
        /// Which predefined macro this token came from.
        kind: PredefinedMacroKind,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    use erl_tokenize::Position;

    use crate::source::{Source, SourceStore};

    fn dummy_span() -> SourceSpan {
        let store = SourceStore::new();
        let id = store.append(Source::from_text("m.erl", "x"));
        SourceSpan::new(id, Position::new(), Position::new())
    }

    fn dummy_source_string() -> SourceString {
        SourceString::new("X", dummy_span())
    }

    fn kind_label(o: &Origin) -> &'static str {
        // If a new variant is added and this match stops being
        // exhaustive, this test forces us to update the discriminant
        // coverage.
        match o {
            Origin::Source => "source",
            Origin::Include(_) => "include",
            Origin::MacroBody { .. } => "macro_body",
            Origin::MacroArgument { .. } => "macro_argument",
            Origin::Stringification { .. } => "stringification",
            Origin::Predefined { .. } => "predefined",
        }
    }

    fn dummy_parent() -> Arc<Origin> {
        Arc::new(Origin::Source)
    }

    #[test]
    fn all_variants_constructible_and_exhaustive() {
        let span = dummy_span();
        let param = dummy_source_string();
        let all = [
            Origin::Source,
            Origin::Include(dummy_parent()),
            Origin::MacroBody {
                parent: dummy_parent(),
                call_site: span,
                definition_span: span,
            },
            Origin::MacroArgument {
                parent: dummy_parent(),
                call_site: span,
                parameter: param.clone(),
                definition_span: span,
            },
            Origin::Stringification {
                parent: dummy_parent(),
                call_site: span,
                parameter: param,
                definition_span: span,
            },
            Origin::Predefined {
                parent: dummy_parent(),
                call_site: span,
                kind: PredefinedMacroKind::File,
            },
        ];
        let kinds: Vec<_> = all.iter().map(kind_label).collect();
        assert_eq!(
            kinds,
            [
                "source",
                "include",
                "macro_body",
                "macro_argument",
                "stringification",
                "predefined",
            ]
        );
    }

    #[test]
    fn parent_chain_shared_on_clone() {
        let span = dummy_span();
        let root = Arc::new(Origin::Source);
        let leaf = Origin::MacroBody {
            parent: Arc::clone(&root),
            call_site: span,
            definition_span: span,
        };
        let before = Arc::strong_count(&root);
        let cloned = leaf.clone();
        let after = Arc::strong_count(&root);

        let (Origin::MacroBody { parent: a, .. }, Origin::MacroBody { parent: b, .. }) =
            (&leaf, &cloned)
        else {
            panic!("expected MacroBody variants");
        };
        assert!(Arc::ptr_eq(a, b));
        // Cloning the leaf bumps the strong count by exactly one; it
        // does not allocate a new parent.
        assert_eq!(after, before + 1);
    }

    #[test]
    fn nested_chain_survives_cloning() {
        let span = dummy_span();
        let root = Arc::new(Origin::Source);
        let mid = Arc::new(Origin::Include(Arc::clone(&root)));
        let leaf = Origin::MacroBody {
            parent: Arc::clone(&mid),
            call_site: span,
            definition_span: span,
        };

        let cloned = leaf.clone();
        let Origin::MacroBody {
            parent: cloned_mid, ..
        } = cloned
        else {
            panic!("expected MacroBody");
        };
        let Origin::Include(cloned_root) = cloned_mid.as_ref() else {
            panic!("expected Include as parent");
        };
        assert!(Arc::ptr_eq(cloned_root, &root));
    }
}
