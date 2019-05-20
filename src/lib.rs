//! Erlang source code preprocessor.
//!
//! # Examples
//!
//! ```
//! # extern crate erl_pp;
//! # extern crate erl_tokenize;
//! use erl_pp::Preprocessor;
//! use erl_tokenize::Lexer;
//!
//! # fn main() {
//! let src = r#"-define(FOO(A), {A, ?LINE}). io:format("Hello: ~p", [?FOO(bar)])."#;
//! let pp = Preprocessor::new(Lexer::new(src));
//! let tokens = pp.collect::<Result<Vec<_>, _>>().unwrap();
//!
//! assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
//!            ["io", ":", "format", "(", r#""Hello: ~p""#, ",",
//!             "[", "{", "bar", ",", "1", "}", "]", ")", "."]);
//! # }
//! ```
//!
//! # References
//!
//! - [Erlang Reference Manual -- Preprocessor](http://erlang.org/doc/reference_manual/macros.html)
//!
#![warn(missing_docs)]
#[macro_use]
extern crate trackable;

pub use crate::directive::Directive;
pub use crate::error::{Error, ErrorKind};
pub use crate::macros::{MacroCall, MacroDef};
pub use crate::preprocessor::Preprocessor;

pub mod directives;
pub mod types;

mod directive;
mod error;
mod macros;
mod preprocessor;
mod token_reader;
mod util;

/// This crate specific `Result` type.
pub type Result<T> = ::std::result::Result<T, Error>;
