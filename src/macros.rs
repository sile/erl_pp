use erl_tokenize::tokens::{SymbolToken, VariableToken};
use erl_tokenize::values::Symbol;
use erl_tokenize::{LexicalToken, Position, PositionRange};
use std::fmt;

use crate::directives::Define;
use crate::token_reader::{ReadFrom, TokenReader};
use crate::types::{MacroArgs, MacroName};
use crate::Result;

/// Macro definition.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
#[allow(clippy::large_enum_variant)]
pub enum MacroDef {
    Static(Define),
    Dynamic(Vec<LexicalToken>),
}
impl MacroDef {
    /// Returns `true` if this macro has variables, otherwise `false`.
    pub fn has_variables(&self) -> bool {
        match *self {
            MacroDef::Static(ref d) => d.variables.is_some(),
            MacroDef::Dynamic(_) => false,
        }
    }
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
            .map(PositionRange::end_position)
            .unwrap_or_else(|| self.name.end_position())
    }
}
impl fmt::Display for MacroCall {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "?{}{}",
            self.name.text(),
            self.args
                .as_ref()
                .map_or("".to_string(), ToString::to_string)
        )
    }
}
impl ReadFrom for MacroCall {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(MacroCall {
            _question: reader.read_expected(&Symbol::Question)?,
            name: reader.read()?,
            args: reader.try_read()?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct NoArgsMacroCall {
    pub _question: SymbolToken,
    pub name: MacroName,
}
impl ReadFrom for NoArgsMacroCall {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(NoArgsMacroCall {
            _question: reader.read_expected(&Symbol::Question)?,
            name: reader.read()?,
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
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Stringify {
            _double_question: reader.read_expected(&Symbol::DoubleQuestion)?,
            name: reader.read()?,
        })
    }
}
