//! Origin metadata for preprocessed tokens.
//!
//! An [`Origin`] tells why a particular token appears in the output:
//! whether it came directly from the source text, from an include, from
//! a macro body, from a macro argument, from a stringification (`??Arg`),
//! or from a predefined macro (`?FILE`, `?LINE`, `?MACHINE`).
//!
//! Payload details for each variant (call site span, definition site
//! span, parameter name, and so on) are added by later work that
//! actually produces each variant. This module fixes only the variant
//! list and the shared parent-chain shape.

use std::sync::Arc;

/// Provenance of a token emitted by the preprocessor.
///
/// The six variants match the token-provenance categories defined in
/// the parent redesign. Variants that can have a parent origin (every
/// variant except [`Origin::Source`]) hold the parent inside an
/// [`Arc<Origin>`] so that deep chains produced by nested macro
/// expansion or by macros inside include sources are structurally
/// shared and are not deep-copied when the enclosing state is cloned.
///
/// The concrete payloads carried by each variant will be filled in by
/// later work that produces them; this module commits only to the enum
/// shape and the parent-sharing scheme.
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
    ///
    /// The parent points at the origin of the macro call site.
    MacroBody(Arc<Origin>),

    /// Token came from a macro argument that was substituted for a
    /// parameter in the replacement body.
    ///
    /// The parent points at the origin of the macro call site (and
    /// through it at the parameter/definition information that later
    /// work will add).
    MacroArgument(Arc<Origin>),

    /// Token was synthesized by stringification (`??Arg`).
    ///
    /// The parent points at the origin of the macro call site whose
    /// argument was stringified.
    Stringification(Arc<Origin>),

    /// Token was synthesized by a predefined macro
    /// (`?FILE`, `?LINE`, `?MACHINE`).
    ///
    /// The parent points at the origin of the predefined macro use
    /// site.
    Predefined(Arc<Origin>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind_label(o: &Origin) -> &'static str {
        // If a new variant is added and this match stops being
        // exhaustive, this test forces us to update the discriminant
        // coverage.
        match o {
            Origin::Source => "source",
            Origin::Include(_) => "include",
            Origin::MacroBody(_) => "macro_body",
            Origin::MacroArgument(_) => "macro_argument",
            Origin::Stringification(_) => "stringification",
            Origin::Predefined(_) => "predefined",
        }
    }

    fn dummy_parent() -> Arc<Origin> {
        Arc::new(Origin::Source)
    }

    #[test]
    fn all_variants_constructible_and_exhaustive() {
        let all = [
            Origin::Source,
            Origin::Include(dummy_parent()),
            Origin::MacroBody(dummy_parent()),
            Origin::MacroArgument(dummy_parent()),
            Origin::Stringification(dummy_parent()),
            Origin::Predefined(dummy_parent()),
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
        let root = Arc::new(Origin::Source);
        let leaf = Origin::MacroBody(Arc::clone(&root));
        let before = Arc::strong_count(&root);
        let cloned = leaf.clone();
        let after = Arc::strong_count(&root);

        // Both leaf and cloned should point at the same parent Origin.
        let (Origin::MacroBody(a), Origin::MacroBody(b)) = (&leaf, &cloned) else {
            panic!("expected MacroBody variants");
        };
        assert!(Arc::ptr_eq(a, b));
        // Cloning the leaf bumps the strong count by exactly one; it
        // does not allocate a new parent.
        assert_eq!(after, before + 1);
    }

    #[test]
    fn nested_chain_survives_cloning() {
        let root = Arc::new(Origin::Source);
        let mid = Arc::new(Origin::Include(Arc::clone(&root)));
        let leaf = Origin::MacroBody(Arc::clone(&mid));

        let cloned = leaf.clone();
        let Origin::MacroBody(cloned_mid) = cloned else {
            panic!("expected MacroBody");
        };
        let Origin::Include(cloned_root) = cloned_mid.as_ref() else {
            panic!("expected Include as parent");
        };
        assert!(Arc::ptr_eq(cloned_root, &root));
    }
}
