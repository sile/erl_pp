use std;
use erl_tokenize::{self, LexicalToken};
use glob;
use trackable::error::TrackableError;
use trackable::error::{ErrorKind as TrackableErrorKind, ErrorKindExt};

/// This crate specific error type.
#[derive(Debug, Clone)]
pub struct Error(TrackableError<ErrorKind>);
impl Error {
    /// Makes a new `Error` instance of kind `ErrorKind::InvalidInput`.
    pub fn invalid_input() -> Self {
        ErrorKind::InvalidInput.into()
    }

    /// Makes a new `Error` instance of kind `ErrorKind::UnexpectedEos`.
    pub fn unexpected_eos() -> Self {
        ErrorKind::UnexpectedEos.into()
    }

    pub fn unexpected_token(token: LexicalToken) -> Self {
        ErrorKind::UnexpectedToken(token).into()
    }
}
derive_traits_for_trackable_error_newtype!(Error, ErrorKind);
impl From<erl_tokenize::Error> for Error {
    fn from(e: erl_tokenize::Error) -> Self {
        let kind = match *e.kind() {
            erl_tokenize::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
            erl_tokenize::ErrorKind::UnexpectedEos => ErrorKind::UnexpectedEos,
        };
        kind.takes_over(e).into()
    }
}
impl From<std::env::VarError> for Error {
    fn from(f: std::env::VarError) -> Self {
        ErrorKind::InvalidInput.cause(f).into()
    }
}
impl From<std::io::Error> for Error {
    fn from(f: std::io::Error) -> Self {
        ErrorKind::InvalidInput.cause(f).into()
    }
}
impl From<glob::PatternError> for Error {
    fn from(f: glob::PatternError) -> Self {
        ErrorKind::InvalidInput.cause(f).into()
    }
}
impl From<glob::GlobError> for Error {
    fn from(f: glob::GlobError) -> Self {
        ErrorKind::InvalidInput.cause(f).into()
    }
}

/// The list of the possible error kinds
#[derive(Debug, Clone)]
pub enum ErrorKind {
    /// Input text is invalid.
    // TODO: Add Position
    InvalidInput,

    UnexpectedToken(LexicalToken),

    /// Unexpected End-Of-String.
    UnexpectedEos,
}
impl TrackableErrorKind for ErrorKind {}
