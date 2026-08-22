//! Origin metadata for [`SourceToken`](crate::SourceToken)s.
//!
//! An [`Origin`] tells why a particular token appears in the output:
//! whether it came directly from the source text, from an include, from
//! a macro body, from a macro argument, from a stringification (`??Arg`),
//! or from a predefined source-info macro (`?FILE`, `?LINE`). Other
//! OTP predefined macros reach the caller as
//! [`Event::AwaitingMacroExpansion`](crate::Event::AwaitingMacroExpansion).

use std::sync::Arc;

use crate::source::SourceSpan;
use crate::source_string::SourceString;

/// Distinguishes `-include` from `-include_lib` in
/// [`IncludeDirective`](crate::IncludeDirective) and [`Origin::Include`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IncludeKind {
    /// `-include("path").`
    Include,
    /// `-include_lib("app/include/hdr.hrl").`
    IncludeLib,
}

/// Which source-info macro a synthesized token came from.
///
/// Attached to [`Origin::SourceInfo`]. Distinguishes the two
/// predefined macros the preprocessor evaluates from the current
/// cursor state (source display name and line number). Every other
/// predefined-in-OTP macro is out of scope for erl_pp itself and
/// reaches the caller through `Event::AwaitingMacroExpansion`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceInfoMacroKind {
    /// `?FILE` — the display name of the current source.
    File,
    /// `?LINE` — the line number at the call site.
    Line,
}

/// Provenance of a token emitted by the preprocessor.
///
/// The seven variants match the token-provenance categories defined
/// in the parent redesign. Variants that can have a parent origin
/// (every variant except [`Origin::Source`]) hold the parent inside
/// an [`Arc<Origin>`] so that deep chains produced by nested macro
/// expansion or by macros inside include sources are structurally
/// shared and are not deep-copied when the enclosing state is cloned.
#[derive(Debug, Clone)]
pub enum Origin {
    /// Token was written directly in an input [`Source`](crate::Source).
    ///
    /// This variant has no parent because it sits at the root of every
    /// provenance chain.
    Source,

    /// Token was written directly in an included source.
    ///
    /// The parent points at the origin of the `-include` /
    /// `-include_lib` directive that pulled the source in.
    Include {
        /// Parent origin (the origin at the include directive's
        /// call site).
        parent: Arc<Origin>,
        /// Span of the whole `-include` / `-include_lib` directive at
        /// the site that pulled the source in.
        include_site: SourceSpan,
        /// Whether the source was pulled in with `-include` or
        /// `-include_lib`.
        kind: IncludeKind,
    },

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

    /// Token was synthesized from the current source cursor state
    /// (`?FILE`, `?LINE`).
    ///
    /// Every other OTP predefined macro (`?MACHINE`, `?MODULE`,
    /// `?FUNCTION_NAME`, etc.) is out of scope for erl_pp itself and
    /// is delegated to the caller through
    /// `Event::AwaitingMacroExpansion`. Macros that appear in a
    /// scanned `-define` expand through the normal user-macro path
    /// and carry [`Origin::MacroBody`], not this variant.
    SourceInfo {
        /// Parent origin (the origin at the macro use site).
        parent: Arc<Origin>,
        /// Span covering the `?FILE` / `?LINE` token pair at the call
        /// site.
        call_site: SourceSpan,
        /// Which source-info macro this token came from.
        kind: SourceInfoMacroKind,
    },

    /// Token came from a [`Source`](crate::Source) the caller supplied through
    /// `Preprocessor::resume_macro_expansion` after an
    /// `Event::AwaitingMacroExpansion`.
    ///
    /// Distinct from [`Origin::MacroBody`] (which is reserved for the
    /// replacement body of a `-define` directive) because caller-driven
    /// expansions do not have a definition to point at; only the call
    /// site and the requested macro name are meaningful.
    CallerExpansion {
        /// Parent origin (the origin at the macro call site).
        parent: Arc<Origin>,
        /// Span covering the whole `?NAME(...)` call at the call site.
        call_site: SourceSpan,
        /// The name of the macro the caller was asked to expand.
        name: SourceString,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Source, SourceStore};

    fn dummy_span() -> SourceSpan {
        let store = SourceStore::new();
        let id = store.append(
            Source::from_text("m.erl", "x").expect("test input must scan without lex errors"),
        );
        SourceSpan::new(
            id,
            erl_tokenize::Position::new(),
            erl_tokenize::Position::new(),
        )
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
            Origin::Include { .. } => "include",
            Origin::MacroBody { .. } => "macro_body",
            Origin::MacroArgument { .. } => "macro_argument",
            Origin::Stringification { .. } => "stringification",
            Origin::SourceInfo { .. } => "source_info",
            Origin::CallerExpansion { .. } => "caller_expansion",
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
            Origin::Include {
                parent: dummy_parent(),
                include_site: span,
                kind: IncludeKind::Include,
            },
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
            Origin::SourceInfo {
                parent: dummy_parent(),
                call_site: span,
                kind: SourceInfoMacroKind::File,
            },
            Origin::CallerExpansion {
                parent: dummy_parent(),
                call_site: span,
                name: dummy_source_string(),
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
                "source_info",
                "caller_expansion",
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
        let mid = Arc::new(Origin::Include {
            parent: Arc::clone(&root),
            include_site: span,
            kind: IncludeKind::Include,
        });
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
        let Origin::Include {
            parent: cloned_root,
            ..
        } = cloned_mid.as_ref()
        else {
            panic!("expected Include as parent");
        };
        assert!(Arc::ptr_eq(cloned_root, &root));
    }
}
