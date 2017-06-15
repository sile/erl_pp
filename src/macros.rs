use std::fmt;
use erl_tokenize::{Position, PositionRange, LexicalToken};
use erl_tokenize::tokens::{SymbolToken, AtomToken, StringToken, IntegerToken, VariableToken};
use erl_tokenize::values::Symbol;

use {Result, Error, ErrorKind};
use token_reader::{TokenReader, ReadFrom};
use types::{MacroName, MacroArgs};

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

/// Predefined macros.
///
/// See [9.3 Predefined Macros]
/// (http://erlang.org/doc/reference_manual/macros.html#id85705)
/// for detailed information.
#[derive(Debug, Default)]
pub struct PredefinedMacros {
    module_name: Option<String>,
    function_name: Option<String>,
    function_arity: Option<usize>,
}
impl PredefinedMacros {
    /// Makes a new `PredefinedMacros` instance.
    pub fn new() -> Self {
        PredefinedMacros::default()
    }

    /// Sets the value of `MODULE` and `MODULE_NAME` macros.
    pub fn set_module_name(&mut self, name: &str) {
        self.module_name = Some(name.to_string());
    }

    /// Sets the value of `FUNCTION_NAME` macro.
    pub fn set_function_name(&mut self, name: &str) {
        self.function_name = Some(name.to_string());
    }

    /// Sets the value of `FUNCTION_ARITY` macro.
    pub fn set_function_arity(&mut self, arity: usize) {
        self.function_arity = Some(arity);
    }

    /// Clears the value of `MODULE` and `MODULE_NAME` macros.
    pub fn clear_module_name(&mut self) {
        self.module_name = None;
    }

    /// Clears the value of `FUNCTION_NAME` macro.
    pub fn clear_function_name(&mut self) {
        self.function_name = None;
    }

    /// Clears the value of `FUNCTION_ARITY` macro.
    pub fn clear_function_arity(&mut self) {
        self.function_arity = None;
    }

    /// Tries to expand the specified macro call.
    ///
    /// If the `call` is not a predefined macro, this will return `Ok(None)`.
    pub fn try_expand(&self, call: &MacroCall) -> Result<Option<LexicalToken>> {
        let expanded = match call.name.value() {
            "MODULE" => {
                let module = track!(self.module_name.as_ref().ok_or(Error::invalid_input()))?;
                AtomToken::from_value(module, call.start_position()).into()
            }
            "MODULE_STRING" => {
                let module = track!(self.module_name.as_ref().ok_or(Error::invalid_input()))?;
                StringToken::from_value(module, call.start_position()).into()
            }
            "FILE" => {
                let current = call.start_position();
                let file = track!(current.filepath().ok_or(Error::invalid_input()))?;
                let file = track!(file.to_str().ok_or(Error::invalid_input()))?;
                StringToken::from_value(file, call.start_position()).into()
            }
            "LINE" => {
                let line = call.start_position().line();
                IntegerToken::from_value(line.into(), call.start_position()).into()
            }
            "MACHINE" => AtomToken::from_value("BEAM", call.start_position()).into(),
            "FUNCTION_NAME" => {
                let name = track!(self.function_name.as_ref().ok_or(Error::invalid_input()))?;
                AtomToken::from_value(name, call.start_position()).into()
            }
            "FUNCTION_ARITY" => {
                let arity = track!(self.function_arity.ok_or(Error::invalid_input()))?;
                IntegerToken::from_value(arity.into(), call.start_position()).into()
            }
            _ => return Ok(None),
        };
        track_assert!(call.args.is_none(), ErrorKind::InvalidInput);
        Ok(Some(expanded))
    }
}
