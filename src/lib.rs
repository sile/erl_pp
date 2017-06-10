extern crate erl_tokenize;
#[macro_use]
extern crate trackable;

pub use erl_tokenize::{Result, Error, ErrorKind};

pub use directive::Directive;
pub use preprocessor::Preprocessor;

mod directive;
mod preprocessor;
mod token_reader;
