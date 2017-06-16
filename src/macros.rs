use std::fmt;
use erl_tokenize::{Position, PositionRange, LexicalToken};
use erl_tokenize::tokens::{SymbolToken, VariableToken};
use erl_tokenize::values::Symbol;

use Result;
use directives::Define;
use token_reader::{TokenReader, ReadFrom};
use types::{MacroName, MacroArgs};

/// Macro Definition.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum MacroDef {
    Static(Define),
    Dynamic(Vec<LexicalToken>),
}

/// Macro call.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct MacroCall {
    pub _question: SymbolToken,
    pub name: MacroName,
    pub args: Option<MacroArgs>,
}
impl PositionRange for MacroCall {
    fn start_position(&self) -> Position {
        self._question.start_position()
    }
    fn end_position(&self) -> Position {
        self.args
            .as_ref()
            .map(|a| a.end_position())
            .unwrap_or_else(|| self.name.end_position())
    }
}
impl fmt::Display for MacroCall {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "?{}{}",
            self.name.text(),
            self.args.as_ref().map_or("".to_string(), |a| a.to_string())
        )
    }
}
impl ReadFrom for MacroCall {
    fn read_from<T, E>(reader: &mut TokenReader<T, E>) -> Result<Self>
    where
        T: Iterator<Item = ::std::result::Result<LexicalToken, E>>,
        E: Into<::Error>,
    {
        Ok(MacroCall {
            _question: track!(reader.read_expected(&Symbol::Question))?,
            name: track!(reader.read())?,
            args: track!(reader.try_read())?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Stringify {
    pub _double_question: SymbolToken,
    pub name: VariableToken,
}
impl PositionRange for Stringify {
    fn start_position(&self) -> Position {
        self._double_question.start_position()
    }
    fn end_position(&self) -> Position {
        self.name.end_position()
    }
}
impl fmt::Display for Stringify {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "??{}", self.name.text())
    }
}
impl ReadFrom for Stringify {
    fn read_from<T, E>(reader: &mut TokenReader<T, E>) -> Result<Self>
    where
        T: Iterator<Item = ::std::result::Result<LexicalToken, E>>,
        E: Into<::Error>,
    {
        Ok(Stringify {
            _double_question: track!(reader.read_expected(&Symbol::DoubleQuestion))?,
            name: track!(reader.read())?,
        })
    }
}
