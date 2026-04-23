use crate::directive::Directive;
use crate::macros::{MacroCall, MacroDef};
use erl_tokenize::tokens::SymbolToken;
use erl_tokenize::{LexicalToken, Position, PositionRange};
use std::path::{Path, PathBuf};

/// Possible errors.
#[derive(Debug)]
#[non_exhaustive]
#[allow(missing_docs)]
#[allow(clippy::large_enum_variant)]
pub enum Error {
    /// Unexpected token.
    UnexpectedToken {
        token: LexicalToken,
        expected: String,
    },

    /// Include file error.
    IncludeFileError {
        source: std::io::Error,
        directive_start: Position,
        directive_end: Position,
        target_file_path: PathBuf,
    },

    /// Missing a macro argument.
    MissingMacroArg { position: Position },

    /// Unbalanced parentheses.
    UnbalancedParen {
        open: Option<SymbolToken>,
        close: SymbolToken,
    },

    /// Unexpected EOF.
    UnexpectedEof,

    /// Cannot expand ?FILE macro.
    FileNotSet { macro_call: MacroCall },

    /// Undefined macro.
    UndefinedMacro { macro_call: MacroCall },

    /// Undefined macro variable.
    UndefinedMacroVar { varname: String },

    /// Macro arguments mismatched.
    MacroArgsMismatched {
        macro_call: MacroCall,
        macro_def: MacroDef,
    },

    /// Non UTF-8 path.
    NonUtf8Path { path: PathBuf },

    /// Unexpected '.' in `-define` directive.
    UnexpectedDotInMacroDef { position: Position },

    /// Missing `-ifdef` or `-ifndef`.
    MissingIfDirective { directive: Directive },

    /// Tokenize error.
    TokenizeError(erl_tokenize::Error),
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

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnexpectedToken { token, expected } => {
                write!(f, "expected a {expected:?} token, but found {token:?}")
            }
            Self::IncludeFileError {
                source,
                target_file_path,
                ..
            } => {
                write!(
                    f,
                    "cannot include file: path={target_file_path:?}, reason={source}"
                )
            }
            Self::MissingMacroArg { position } => {
                write!(f, "expected an macro argument before ',' ({position})")
            }
            Self::UnbalancedParen { open, close } => {
                write!(f, "unbalanced parentheses: open={open:?}, close={close:?}")
            }
            Self::UnexpectedEof => write!(f, "unexpected EOF"),
            Self::FileNotSet { macro_call } => {
                write!(f, "cannot expand ?FILE macro ({macro_call:?})")
            }
            Self::UndefinedMacro { macro_call } => write!(f, "undefined macro: {macro_call:?}"),
            Self::UndefinedMacroVar { varname } => {
                write!(f, "no such macro variable: {varname:?}")
            }
            Self::MacroArgsMismatched {
                macro_call,
                macro_def,
            } => write!(
                f,
                "macro arguments mismatched: def={macro_def:?}, call={macro_call:?}"
            ),
            Self::NonUtf8Path { path } => {
                write!(f, "cannot convert a path {path:?} to a UTF-8 string")
            }
            Self::UnexpectedDotInMacroDef { position } => {
                write!(
                    f,
                    "found unexpected '.' in `-define` directive ({position})"
                )
            }
            Self::MissingIfDirective { .. } => {
                write!(f, "missing `-ifdef` or `ifndef` directives")
            }
            Self::TokenizeError(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IncludeFileError { source, .. } => Some(source),
            Self::TokenizeError(e) => Some(e),
            _ => None,
        }
    }
}

impl From<erl_tokenize::Error> for Error {
    fn from(e: erl_tokenize::Error) -> Self {
        Self::TokenizeError(e)
    }
}
