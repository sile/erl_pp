extern crate erl_tokenize;
#[macro_use]
extern crate trackable;

pub use erl_tokenize::{Result, Error, ErrorKind};

pub use directive::{Directive, Directive2};
pub use preprocessor::{Preprocessor, Preprocessor2};

mod directive;
mod preprocessor;
mod token_reader;
