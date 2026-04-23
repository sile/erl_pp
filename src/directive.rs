use erl_tokenize::tokens::SymbolToken;
use erl_tokenize::values::Symbol;
use erl_tokenize::{LexicalToken, Position, PositionRange};
use std::fmt;

use crate::directives;
use crate::token_reader::{ReadFrom, TokenReader};
use crate::{Error, Result};

/// Macro directive.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
#[allow(clippy::large_enum_variant)]
pub enum Directive {
    Include(directives::Include),
    IncludeLib(directives::IncludeLib),
    Define(directives::Define),
    Undef(directives::Undef),
    Ifdef(directives::Ifdef),
    Ifndef(directives::Ifndef),
    Else(directives::Else),
    Endif(directives::Endif),
    Error(directives::Error),
    Warning(directives::Warning),
}
impl PositionRange for Directive {
    fn start_position(&self) -> Position {
        match *self {
            Directive::Include(ref t) => t.start_position(),
            Directive::IncludeLib(ref t) => t.start_position(),
            Directive::Define(ref t) => t.start_position(),
            Directive::Undef(ref t) => t.start_position(),
            Directive::Ifdef(ref t) => t.start_position(),
            Directive::Ifndef(ref t) => t.start_position(),
            Directive::Else(ref t) => t.start_position(),
            Directive::Endif(ref t) => t.start_position(),
            Directive::Error(ref t) => t.start_position(),
            Directive::Warning(ref t) => t.start_position(),
        }
    }
    fn end_position(&self) -> Position {
        match *self {
            Directive::Include(ref t) => t.end_position(),
            Directive::IncludeLib(ref t) => t.end_position(),
            Directive::Define(ref t) => t.end_position(),
            Directive::Undef(ref t) => t.end_position(),
            Directive::Ifdef(ref t) => t.end_position(),
            Directive::Ifndef(ref t) => t.end_position(),
            Directive::Else(ref t) => t.end_position(),
            Directive::Endif(ref t) => t.end_position(),
            Directive::Error(ref t) => t.end_position(),
            Directive::Warning(ref t) => t.end_position(),
        }
    }
}
impl fmt::Display for Directive {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Directive::Include(ref t) => t.fmt(f),
            Directive::IncludeLib(ref t) => t.fmt(f),
            Directive::Define(ref t) => t.fmt(f),
            Directive::Undef(ref t) => t.fmt(f),
            Directive::Ifdef(ref t) => t.fmt(f),
            Directive::Ifndef(ref t) => t.fmt(f),
            Directive::Else(ref t) => t.fmt(f),
            Directive::Endif(ref t) => t.fmt(f),
            Directive::Error(ref t) => t.fmt(f),
            Directive::Warning(ref t) => t.fmt(f),
        }
    }
}
impl ReadFrom for Directive {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let _hyphen: SymbolToken = reader.read_expected(&Symbol::Hyphen)?;
        let name_token: LexicalToken = reader
            .try_read_token()?
            .ok_or_else(|| Error::unexpected_token(_hyphen.clone().into(), "-{DIRECTIVE_NAME}"))?;
        let name_text = match &name_token {
            LexicalToken::Atom(t) => Some(t.value()),
            LexicalToken::Keyword(t) => Some(t.text()),
            _ => None,
        };

        reader.unread_token(name_token.clone());
        reader.unread_token(_hyphen.into());
        match name_text {
            Some("include") => reader.read().map(Directive::Include),
            Some("include_lib") => reader.read().map(Directive::IncludeLib),
            Some("define") => reader.read().map(Directive::Define),
            Some("undef") => reader.read().map(Directive::Undef),
            Some("ifdef") => reader.read().map(Directive::Ifdef),
            Some("ifndef") => reader.read().map(Directive::Ifndef),
            Some("else") => reader.read().map(Directive::Else),
            Some("endif") => reader.read().map(Directive::Endif),
            Some("error") => reader.read().map(Directive::Error),
            Some("warning") => reader.read().map(Directive::Warning),
            _ => {
                let _hyphen: SymbolToken = reader.read_expected(&Symbol::Hyphen)?;
                Err(Error::unexpected_token(_hyphen.into(), "-{DIRECTIVE_NAME}"))
            }
        }
    }
}
