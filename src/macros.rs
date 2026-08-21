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

use erl_tokenize::{Symbol, Token, TokenKind};

use crate::directive::{Directive, Param};
use crate::error::PreprocessError;
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
/// Built by the preprocessor from a parsed [`Directive::Define`] with
/// the parameter list validated (duplicate parameter names are
/// rejected as [`PreprocessError::DuplicateParameter`]).
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
    /// Returns [`PreprocessError::DuplicateParameter`] when the parameter
    /// list is invalid (duplicate names today; more kinds may be added
    /// later).
    pub(crate) fn from_directive(
        directive: &Directive,
        source: Arc<Source>,
        source_id: SourceId,
        origin: Origin,
    ) -> Result<Self, PreprocessError> {
        let Directive::Define {
            span,
            name,
            params,
            replacement,
            ..
        } = directive
        else {
            unreachable!("from_directive requires Directive::Define");
        };
        let (params, arity) = match params {
            Some(params) => {
                if let Some(dup) = first_duplicate_param(params) {
                    return Err(PreprocessError::DuplicateParameter {
                        span: *span,
                        name: dup,
                    });
                }
                (params.clone(), Some(params.len()))
            }
            None => (Vec::new(), None),
        };
        let key = MacroKey {
            name: name.value.clone(),
            arity,
        };
        let replacement = replacement
            .iter()
            .map(|t| build_source_token(*t, &source, source_id, &origin))
            .collect();
        Ok(Self {
            key,
            params,
            replacement,
            directive_span: *span,
            name_span: name.span,
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
///
/// Internally the table maintains a parallel "uses" map that records,
/// for every stored definition, the `(name, arity)` macro references
/// that its replacement body statically calls. This drives the OTP
/// `check_uses/4`-style top-level DFS used for circular expansion
/// detection.
#[derive(Debug, Clone, Default)]
pub struct MacroTable {
    entries: HashMap<MacroKey, MacroDefinition>,
    uses: HashMap<MacroKey, Vec<(String, Option<usize>)>>,
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

    /// Returns the statically collected macro references that the
    /// definition for `key` calls from its replacement body.
    ///
    /// Kept as a lightweight accessor for tests; production callers
    /// consult [`MacroTable::check_circular_uses`] instead.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "runtime cycle detection goes through check_circular_uses"
        )
    )]
    pub(crate) fn uses_of(&self, key: &MacroKey) -> Option<&[(String, Option<usize>)]> {
        self.uses.get(key).map(Vec::as_slice)
    }

    /// Runs an OTP-style top-level DFS over the static uses graph
    /// starting from `key`. Returns the ancestor chain that closes
    /// back on itself when a cycle is found, or `None` when no cycle
    /// is reachable from `key`.
    ///
    /// The returned chain is in call order (outermost first) and ends
    /// with the repeated `(name, arity)` pair.
    pub(crate) fn check_circular_uses(
        &self,
        key: &MacroKey,
    ) -> Option<Vec<(String, Option<usize>)>> {
        let mut ancestors: Vec<(String, Option<usize>)> = Vec::new();
        self.check_uses_dfs(&key.name, key.arity, &mut ancestors)
    }

    fn check_uses_dfs(
        &self,
        name: &str,
        arity: Option<usize>,
        ancestors: &mut Vec<(String, Option<usize>)>,
    ) -> Option<Vec<(String, Option<usize>)>> {
        let node = (name.to_owned(), arity);
        if let Some(existing_idx) = ancestors.iter().position(|k| *k == node) {
            let mut chain: Vec<_> = ancestors[existing_idx..].to_vec();
            chain.push(node);
            return Some(chain);
        }
        ancestors.push(node);
        let child_uses = self
            .uses
            .get(&MacroKey {
                name: name.to_owned(),
                arity,
            })
            .cloned()
            .unwrap_or_default();
        for (child_name, child_arity) in &child_uses {
            if let Some(chain) = self.check_uses_dfs(child_name, *child_arity, ancestors) {
                return Some(chain);
            }
        }
        ancestors.pop();
        None
    }

    /// Inserts `def`, returning the previous entry for the same key
    /// if one was replaced.
    ///
    /// Also refreshes the uses-map entry for the key with the
    /// statically collected references from the new definition's
    /// replacement body.
    pub(crate) fn insert(&mut self, def: MacroDefinition) -> Option<MacroDefinition> {
        let key = def.key.clone();
        let uses = collect_uses(&def.replacement);
        self.uses.insert(key.clone(), uses);
        self.entries.insert(key, def)
    }

    /// Removes every entry whose key matches `name` (regardless of
    /// arity) and returns the number of entries removed.
    ///
    /// This is the semantics of `-undef(NAME).`: OTP removes both the
    /// constant-like `NAME` and every function-like arity of `NAME`.
    /// The uses-map entries for the removed keys are dropped in the
    /// same step.
    pub(crate) fn remove_all_by_name(&mut self, name: &str) -> usize {
        let before = self.entries.len();
        self.uses.retain(|k, _| k.name != name);
        self.entries.retain(|k, _| k.name != name);
        before - self.entries.len()
    }
}

fn collect_uses(replacement: &[PreprocessedToken]) -> Vec<(String, Option<usize>)> {
    let lex: Vec<&PreprocessedToken> = replacement
        .iter()
        .filter(|pt| pt.token().kind().is_lexical())
        .collect();

    let mut uses = Vec::new();
    let mut i = 0;
    while i < lex.len() {
        if !is_symbol(lex[i].token(), Symbol::Question) {
            i += 1;
            continue;
        }
        // Skip `??` stringification prefix.
        if lex
            .get(i + 1)
            .is_some_and(|t| is_symbol(t.token(), Symbol::Question))
        {
            i += 2;
            continue;
        }
        let Some(name_tok) = lex.get(i + 1) else {
            break;
        };
        if !matches!(
            name_tok.token().kind(),
            TokenKind::Atom | TokenKind::Variable
        ) {
            i += 1;
            continue;
        }
        let name = name_tok.text().to_owned();
        let arity = if lex
            .get(i + 2)
            .is_some_and(|t| is_symbol(t.token(), Symbol::OpenParen))
        {
            let (arity, consumed) = count_call_args(&lex[i + 3..]);
            uses.push((name, Some(arity)));
            i += 3 + consumed;
            continue;
        } else {
            None
        };
        uses.push((name, arity));
        i += 2;
    }
    uses
}

fn is_symbol(token: &Token, sym: Symbol) -> bool {
    matches!(token.kind(), TokenKind::Symbol(s) if s == sym)
}

/// Counts the top-level arguments inside a macro call whose opening
/// `(` has already been consumed.
///
/// Returns `(arity, tokens_consumed)` where `tokens_consumed` covers
/// every token up to and including the matching `)`. `arity` counts
/// top-level `,`-separated groups (an empty balanced `()` is arity 0,
/// a single group is arity 1, etc.).
///
/// This is a lightweight version that tracks paren-family balance
/// (`( )`, `[ ]`, `{ }`, `<< >>`) but not `end`-terminated keyword
/// blocks. It is used only for building the static uses graph; the
/// runtime argument parser at call time is exact.
fn count_call_args(rest: &[&PreprocessedToken]) -> (usize, usize) {
    let mut depth = 0usize;
    let mut has_content = false;
    let mut commas = 0usize;
    for (i, t) in rest.iter().enumerate() {
        let kind = t.token().kind();
        match kind {
            TokenKind::Symbol(Symbol::OpenParen)
            | TokenKind::Symbol(Symbol::OpenSquare)
            | TokenKind::Symbol(Symbol::OpenBrace)
            | TokenKind::Symbol(Symbol::DoubleLeftAngle) => {
                depth += 1;
                has_content = true;
            }
            TokenKind::Symbol(Symbol::CloseSquare)
            | TokenKind::Symbol(Symbol::CloseBrace)
            | TokenKind::Symbol(Symbol::DoubleRightAngle) => {
                depth = depth.saturating_sub(1);
            }
            TokenKind::Symbol(Symbol::CloseParen) => {
                if depth == 0 {
                    let arity = if has_content { commas + 1 } else { 0 };
                    return (arity, i + 1);
                }
                depth -= 1;
            }
            TokenKind::Symbol(Symbol::Comma) if depth == 0 => {
                commas += 1;
                has_content = true;
            }
            _ => {
                has_content = true;
            }
        }
    }
    // Unbalanced — treat as arity 0 (best-effort static analysis).
    let arity = if has_content { commas + 1 } else { 0 };
    (arity, rest.len())
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
        let define = match &dir {
            Directive::Define { .. } => &dir,
            other => panic!("expected Define, got {other:?}"),
        };
        MacroDefinition::from_directive(define, source, source_id, Origin::Source)
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
        let err = MacroDefinition::from_directive(&dir, source, source_id, Origin::Source)
            .expect_err("duplicate should fail");
        match err {
            PreprocessError::DuplicateParameter { name, .. } => {
                assert_eq!(name.as_str(), "A")
            }
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
        let def = MacroDefinition::from_directive(&dir, source.clone(), source_id, Origin::Source)
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
    fn insert_populates_uses_map_for_constant_like_ref() {
        let mut table = MacroTable::new();
        // Body contains ?BAR (constant-like reference).
        table.insert(definition("-define(FOO, ?BAR + 1)."));
        let uses = table
            .uses_of(&MacroKey::constant("FOO"))
            .expect("uses recorded");
        assert_eq!(uses, &[("BAR".to_owned(), None)]);
    }

    #[test]
    fn insert_populates_uses_map_for_function_like_ref() {
        let mut table = MacroTable::new();
        // Body contains ?BAR(A, B) (function-like reference, arity 2).
        table.insert(definition("-define(FOO(A, B), ?BAR(A, B) + 1)."));
        let uses = table
            .uses_of(&MacroKey::function("FOO", 2))
            .expect("uses recorded");
        assert_eq!(uses, &[("BAR".to_owned(), Some(2))]);
    }

    #[test]
    fn stringification_ref_is_skipped_from_uses() {
        let mut table = MacroTable::new();
        // ??A is a stringification, not a macro use.
        table.insert(definition("-define(FOO(A), ??A)."));
        let uses = table
            .uses_of(&MacroKey::function("FOO", 1))
            .expect("uses recorded");
        assert!(uses.is_empty());
    }

    #[test]
    fn undef_drops_uses_entries() {
        let mut table = MacroTable::new();
        table.insert(definition("-define(FOO, ?BAR)."));
        table.insert(definition("-define(FOO(X), ?BAR(X))."));
        assert!(table.uses_of(&MacroKey::constant("FOO")).is_some());
        assert!(table.uses_of(&MacroKey::function("FOO", 1)).is_some());
        table.remove_all_by_name("FOO");
        assert!(table.uses_of(&MacroKey::constant("FOO")).is_none());
        assert!(table.uses_of(&MacroKey::function("FOO", 1)).is_none());
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
