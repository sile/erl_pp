//! Macro definition storage and lookup.
//!
//! The state machine holds a [`MacroTable`] that maps a [`MacroKey`]
//! (name plus optional arity) to a [`MacroDefinition`]. Constant-like
//! (`-define(FOO, ...)`) and function-like (`-define(FOO(A, B), ...)`)
//! macros with the same name coexist because they carry different
//! arities; a bare name (`?FOO`) selects the constant-like key and a
//! call (`?FOO(x, y)`) selects the arity-matching function-like key.
//!
//! This module owns the definition side of macros only; expansion is
//! carried out by later work.
#![expect(
    clippy::result_large_err,
    reason = "PreprocessError deliberately carries structured spans; \
              boxing every Result would add allocation overhead on every define"
)]

use std::collections::HashMap;
use std::sync::Arc;

use erl_tokenize::Token;

use crate::directive::{DefineDirective, Param};
use crate::error::{MacroDefinitionErrorKind, PreprocessError};
use crate::origin::Origin;
use crate::preprocessed_token::PreprocessedToken;
use crate::source::{Source, SourceId, SourceSpan};
use crate::source_string::SourceString;

/// Identifier of a macro entry in a [`MacroTable`].
///
/// A key is the pair (name, arity). `arity` is `None` for
/// constant-like macros (`-define(FOO, 1).`) and `Some(n)` for
/// function-like macros with `n` parameters. Arity-0 function-like
/// macros (`-define(FOO(), 1).`) are `Some(0)` and are distinct from
/// the constant-like `FOO`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MacroKey {
    /// Macro name as written in the directive.
    pub name: String,
    /// `None` for constant-like macros, `Some(n)` for function-like
    /// macros with `n` parameters.
    pub arity: Option<usize>,
}

impl MacroKey {
    /// Creates a constant-like macro key (`-define(NAME, ...)`).
    pub fn constant<N: Into<String>>(name: N) -> Self {
        Self {
            name: name.into(),
            arity: None,
        }
    }

    /// Creates a function-like macro key (`-define(NAME(...), ...)`)
    /// with the given arity.
    pub fn function<N: Into<String>>(name: N, arity: usize) -> Self {
        Self {
            name: name.into(),
            arity: Some(arity),
        }
    }
}

/// A macro definition, ready to be looked up and expanded.
///
/// Built by the preprocessor from a parsed [`DefineDirective`] with
/// the parameter list validated (duplicate parameter names are
/// rejected as [`PreprocessError::MacroDefinition`]).
///
/// The replacement is kept as [`PreprocessedToken`]s so that later
/// expansion can hand a caller tokens whose text, span, and origin are
/// already resolved.
#[derive(Debug, Clone)]
pub struct MacroDefinition {
    /// Table key (name plus optional arity).
    pub key: MacroKey,
    /// Parameter names in declaration order. Empty for constant-like
    /// macros; carries the arity-matching parameters for function-like
    /// macros (empty vector for arity 0).
    pub params: Vec<Param>,
    /// Replacement token list (may include hidden tokens).
    pub replacement: Vec<PreprocessedToken>,
    /// Span covering the whole `-define(...)` directive.
    pub directive_span: SourceSpan,
    /// Span of the macro name token.
    pub name_span: SourceSpan,
    /// Origin the preprocessor assigned to the definition.
    pub origin: Origin,
}

impl MacroDefinition {
    /// Builds a definition from a parsed directive.
    ///
    /// `source` is the [`Source`] the directive was scanned from; it
    /// is used to construct the replacement [`PreprocessedToken`]s.
    /// `origin` is the [`Origin`] assigned to the tokens (typically
    /// [`Origin::Source`] for source-scanned directives, or the
    /// synthesized origin for initial macros registered through the
    /// preprocessor's initialization API).
    ///
    /// Returns [`PreprocessError::MacroDefinition`] when the parameter
    /// list is invalid (duplicate names today; more kinds may be added
    /// later).
    pub(crate) fn from_directive(
        directive: &DefineDirective,
        source: Arc<Source>,
        source_id: SourceId,
        origin: Origin,
    ) -> Result<Self, PreprocessError> {
        let (params, arity) = match &directive.params {
            Some(params) => {
                if let Some(name) = first_duplicate_param(params) {
                    return Err(PreprocessError::MacroDefinition {
                        span: directive.span,
                        kind: MacroDefinitionErrorKind::DuplicateParameter { name },
                    });
                }
                (params.clone(), Some(params.len()))
            }
            None => (Vec::new(), None),
        };
        let key = MacroKey {
            name: directive.name.value.clone(),
            arity,
        };
        let replacement = directive
            .replacement
            .iter()
            .map(|t| build_source_token(*t, &source, source_id, &origin))
            .collect();
        Ok(Self {
            key,
            params,
            replacement,
            directive_span: directive.span,
            name_span: directive.name.span,
            origin,
        })
    }
}

fn build_source_token(
    token: Token,
    source: &Arc<Source>,
    source_id: SourceId,
    origin: &Origin,
) -> PreprocessedToken {
    PreprocessedToken::new(token, Arc::clone(source), source_id, origin.clone())
}

fn first_duplicate_param(params: &[Param]) -> Option<SourceString> {
    let mut seen = HashMap::<&str, ()>::new();
    for p in params {
        if seen.insert(p.name.as_str(), ()).is_some() {
            return Some(p.name.clone());
        }
    }
    None
}

/// Read-only view of the preprocessor's macro table.
///
/// Exposed by [`crate::Preprocessor::macros`]. Modifications happen
/// through directive processing and through the preprocessor's
/// initialization API; the caller has no direct mutator.
#[derive(Debug, Clone, Default)]
pub struct MacroTable {
    entries: HashMap<MacroKey, MacroDefinition>,
}

impl MacroTable {
    /// Creates an empty macro table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the number of macro definitions stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when no definition is stored.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the definition matching the exact key, if any.
    pub fn get(&self, key: &MacroKey) -> Option<&MacroDefinition> {
        self.entries.get(key)
    }

    /// Returns the constant-like definition for `name`, if any.
    ///
    /// Shorthand for `self.get(&MacroKey::constant(name))`.
    pub fn get_constant(&self, name: &str) -> Option<&MacroDefinition> {
        self.get(&MacroKey {
            name: name.to_owned(),
            arity: None,
        })
    }

    /// Returns the function-like definition for `name` with the given
    /// arity, if any.
    ///
    /// Shorthand for `self.get(&MacroKey::function(name, arity))`.
    pub fn get_function(&self, name: &str, arity: usize) -> Option<&MacroDefinition> {
        self.get(&MacroKey {
            name: name.to_owned(),
            arity: Some(arity),
        })
    }

    /// Returns `true` when any definition (constant-like or
    /// function-like of any arity) exists for `name`.
    ///
    /// This is the definition used by conditional directives
    /// (`ifdef` / `ifndef`) in OTP, which check by name and ignore
    /// arity overloads.
    pub fn is_defined(&self, name: &str) -> bool {
        self.entries.keys().any(|k| k.name == name)
    }

    /// Inserts `def`, returning the previous entry for the same key
    /// if one was replaced.
    pub(crate) fn insert(&mut self, def: MacroDefinition) -> Option<MacroDefinition> {
        self.entries.insert(def.key.clone(), def)
    }

    /// Removes every entry whose key matches `name` (regardless of
    /// arity) and returns the number of entries removed.
    ///
    /// This is the semantics of `-undef(NAME).`: OTP removes both the
    /// constant-like `NAME` and every function-like arity of `NAME`.
    pub(crate) fn remove_all_by_name(&mut self, name: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|k, _| k.name != name);
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::directive::{Directive, parse_directive};
    use crate::error::PreprocessError;
    use crate::source::SourceStore;

    fn source(text: &str) -> (Arc<Source>, SourceId, Arc<SourceStore>) {
        let store = Arc::new(SourceStore::new());
        let source_id = store.append(Source::from_text("m.erl", text));
        let source = store.get(source_id);
        (source, source_id, store)
    }

    fn definition(text: &str) -> MacroDefinition {
        let (source, source_id, _store) = source(text);
        let mut cursor = crate::cursor::Cursor::new(source_id, source.clone());
        let dir = parse_directive(&mut cursor)
            .expect("parse ok")
            .expect("recognised");
        let define = match dir {
            Directive::Define(d) => d,
            other => panic!("expected Define, got {other:?}"),
        };
        MacroDefinition::from_directive(&define, source, source_id, Origin::Source)
            .expect("valid definition")
    }

    #[test]
    fn constant_like_key_has_no_arity() {
        let def = definition("-define(FOO, 1).");
        assert_eq!(def.key, MacroKey::constant("FOO"));
        assert!(def.params.is_empty());
        // "1" plus surrounding hidden tokens depending on parser; at
        // minimum the atom-like token exists.
        assert!(!def.replacement.is_empty());
    }

    #[test]
    fn arity_0_function_like_differs_from_constant_like() {
        let constant = definition("-define(FOO, 1).");
        let arity0 = definition("-define(FOO(), 1).");
        assert_eq!(constant.key, MacroKey::constant("FOO"));
        assert_eq!(arity0.key, MacroKey::function("FOO", 0));
        assert_ne!(constant.key, arity0.key);
    }

    #[test]
    fn function_like_key_carries_arity() {
        let def = definition("-define(BAR(A, B, C), A).");
        assert_eq!(def.key, MacroKey::function("BAR", 3));
        assert_eq!(def.params.len(), 3);
        assert_eq!(def.params[0].name.as_str(), "A");
        assert_eq!(def.params[1].name.as_str(), "B");
        assert_eq!(def.params[2].name.as_str(), "C");
    }

    #[test]
    fn duplicate_parameter_is_rejected() {
        let (source, source_id, _store) = source("-define(BAD(A, A), A).");
        let mut cursor = crate::cursor::Cursor::new(source_id, source.clone());
        let dir = parse_directive(&mut cursor)
            .expect("parse ok")
            .expect("recognised");
        let define = match dir {
            Directive::Define(d) => d,
            other => panic!("expected Define, got {other:?}"),
        };
        let err = MacroDefinition::from_directive(&define, source, source_id, Origin::Source)
            .expect_err("duplicate should fail");
        match err {
            PreprocessError::MacroDefinition {
                kind: MacroDefinitionErrorKind::DuplicateParameter { name },
                ..
            } => assert_eq!(name.as_str(), "A"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn replacement_tokens_carry_source_and_origin() {
        let (source, source_id, _store) = source("-define(FOO, bar).");
        let mut cursor = crate::cursor::Cursor::new(source_id, source.clone());
        let dir = parse_directive(&mut cursor)
            .expect("parse ok")
            .expect("recognised");
        let define = match dir {
            Directive::Define(d) => d,
            other => panic!("expected Define, got {other:?}"),
        };
        let def =
            MacroDefinition::from_directive(&define, source.clone(), source_id, Origin::Source)
                .expect("ok");
        assert!(def.replacement.iter().any(|t| t.text() == "bar"));
        for t in &def.replacement {
            assert_eq!(t.source_span().source_id, source_id);
            assert!(matches!(t.origin(), Origin::Source));
            assert!(Arc::ptr_eq(t.source(), &source));
        }
    }

    #[test]
    fn table_distinguishes_arities() {
        let mut table = MacroTable::new();
        table.insert(definition("-define(FOO, 1)."));
        table.insert(definition("-define(FOO(), 2)."));
        table.insert(definition("-define(FOO(A), 3)."));
        assert_eq!(table.len(), 3);
        assert!(table.get_constant("FOO").is_some());
        assert!(table.get_function("FOO", 0).is_some());
        assert!(table.get_function("FOO", 1).is_some());
        assert!(table.get_function("FOO", 2).is_none());
        assert!(table.is_defined("FOO"));
        assert!(!table.is_defined("BAR"));
    }

    #[test]
    fn insert_returns_previous_definition_for_same_key() {
        let mut table = MacroTable::new();
        assert!(table.insert(definition("-define(FOO, 1).")).is_none());
        let prev = table.insert(definition("-define(FOO, 2).")).expect("some");
        assert_eq!(prev.key, MacroKey::constant("FOO"));
    }

    #[test]
    fn undef_removes_all_arities() {
        let mut table = MacroTable::new();
        table.insert(definition("-define(FOO, 1)."));
        table.insert(definition("-define(FOO(), 2)."));
        table.insert(definition("-define(FOO(A), A)."));
        assert_eq!(table.remove_all_by_name("FOO"), 3);
        assert!(!table.is_defined("FOO"));
    }

    #[test]
    fn undef_of_absent_name_is_noop() {
        let mut table = MacroTable::new();
        table.insert(definition("-define(BAR, 1)."));
        assert_eq!(table.remove_all_by_name("FOO"), 0);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn clone_isolates_updates() {
        let mut original = MacroTable::new();
        original.insert(definition("-define(FOO, 1)."));
        let mut clone = original.clone();
        clone.insert(definition("-define(BAR, 2)."));
        clone.remove_all_by_name("FOO");

        assert!(original.get_constant("FOO").is_some());
        assert!(original.get_constant("BAR").is_none());
        assert!(clone.get_constant("BAR").is_some());
        assert!(clone.get_constant("FOO").is_none());
    }
}
