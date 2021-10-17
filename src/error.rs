use crate::directive::Directive;
use crate::macros::{MacroCall, MacroDef};
use erl_tokenize::tokens::SymbolToken;
use erl_tokenize::{LexicalToken, Position, PositionRange};
use std::path::{Path, PathBuf};

/// Possible errors.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Error {
    /// Unexpected token.
    #[error("expected a {expected:?} token, but found {token:?}")]
    UnexpectedToken {
        token: LexicalToken,
        expected: String,
    },

    /// Include file error.
    #[error("cannot include file: path={target_file_path:?}, reason={source}")]
    IncludeFileError {
        source: std::io::Error,
        directive_start: Position,
        directive_end: Position,
        target_file_path: PathBuf,
    },

    /// Missing a macro argument.
    #[error("expected an macro argument before ',' ({position})")]
    MissingMacroArg { position: Position },

    /// Unbalanced parentheses.
    #[error("unbalanced parentheses: open={open:?}, close={close:?}")]
    UnbalancedParen {
        open: Option<SymbolToken>,
        close: SymbolToken,
    },

    /// Unexpected EOF.
    #[error("unexpected EOF")]
    UnexpectedEof,

    /// Cannot expand ?FILE macro.
    #[error("cannot expand ?FILE macro ({macro_call:?})")]
    FileNotSet { macro_call: MacroCall },

    /// Undefined macro.
    #[error("undefined macro: {macro_call:?}")]
    UndefinedMacro { macro_call: MacroCall },

    /// Undefined macro variable.
    #[error("no such macro variable: {varname:?}")]
    UndefinedMacroVar { varname: String },

    /// Macro arguments mismatched.
    #[error("macro arguments mismatched: def={macro_def:?}, call={macro_call:?}")]
    MacroArgsMismatched {
        macro_call: MacroCall,
        macro_def: MacroDef,
    },

    /// Non UTF-8 path.
    #[error("cannot convert a path {path:?} to a UTF-8 string")]
    NonUtf8Path { path: PathBuf },

    /// Unexpected '.' in `-define` directive.
    #[error("found unexpected '.' in `-define` directive ({position})")]
    UnexpectedDotInMacroDef { position: Position },

    /// Missing `-ifdef` or `-ifndef`.
    #[error("missing `-ifdef` or `ifndef` directives")]
    MissingIfDirective { directive: Directive },

    /// Tokenize error.
    #[error(transparent)]
    TokenizeError(#[from] erl_tokenize::Error),

    /// Glob pattern error.
    #[error(transparent)]
    GlobPatternError(#[from] glob::PatternError),

    /// Glob error.
    #[error(transparent)]
    GlobError(#[from] glob::GlobError),
}

impl Error {
    pub(crate) fn unexpected_token(token: LexicalToken, expected: &str) -> Self {
        Self::UnexpectedToken {
            token,
            expected: expected.to_owned(),
        }
    }

    pub(crate) fn include_file_error(
        source: std::io::Error,
        directive: &impl PositionRange,
        target_file_path: PathBuf,
    ) -> Self {
        Self::IncludeFileError {
            source,
            directive_start: directive.start_position(),
            directive_end: directive.end_position(),
            target_file_path,
        }
    }

    pub(crate) fn missing_macro_arg(position: Position) -> Self {
        Self::MissingMacroArg { position }
    }

    pub(crate) fn unbalanced_paren(open: Option<SymbolToken>, close: SymbolToken) -> Self {
        Self::UnbalancedParen { open, close }
    }

    pub(crate) fn file_not_set(macro_call: MacroCall) -> Self {
        Self::FileNotSet { macro_call }
    }

    pub(crate) fn undefined_macro(macro_call: MacroCall) -> Self {
        Self::UndefinedMacro { macro_call }
    }

    pub(crate) fn non_utf8_path(path: impl AsRef<Path>) -> Self {
        Self::NonUtf8Path {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub(crate) fn unexpected_dot_in_macro_def(token: &LexicalToken) -> Self {
        Self::UnexpectedDotInMacroDef {
            position: token.start_position(),
        }
    }

    pub(crate) fn macro_args_mismatched(macro_call: MacroCall, macro_def: MacroDef) -> Self {
        Self::MacroArgsMismatched {
            macro_call,
            macro_def,
        }
    }

    pub(crate) fn undefined_macro_var(varname: String) -> Self {
        Self::UndefinedMacroVar { varname }
    }

    pub(crate) fn missing_if_directive(directive: Directive) -> Self {
        Self::MissingIfDirective { directive }
    }
}
