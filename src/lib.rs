extern crate erl_tokenize;
extern crate glob;
#[macro_use]
extern crate trackable;

pub use directive::Directive;
pub use macros::{MacroCall, PredefinedMacros};
pub use error::{Error, ErrorKind};
pub use preprocessor::Preprocessor;

pub mod directives;
pub mod types;

mod directive;
mod macros;
mod error;
mod preprocessor;
mod token_reader;
mod util;

pub type Result<T> = ::std::result::Result<T, Error>;
