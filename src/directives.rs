//! Macro directives.
use erl_tokenize::tokens::{AtomToken, StringToken, SymbolToken};
use erl_tokenize::values::Symbol;
use erl_tokenize::{LexicalToken, Position, PositionRange};
use glob::glob;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Component, PathBuf};

use crate::token_reader::{ReadFrom, TokenReader};
use crate::types::{MacroName, MacroVariables};
use crate::util;
use crate::Result;

/// `include` directive.
///
/// See [9.1 File Inclusion](http://erlang.org/doc/reference_manual/macros.html#id85412)
/// for detailed information.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Include {
    pub _hyphen: SymbolToken,
    pub _include: AtomToken,
    pub _open_paren: SymbolToken,
    pub path: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl Include {
    /// Executes file inclusion.
    pub fn include(&self) -> Result<(PathBuf, String)> {
        let path = util::substitute_path_variables(self.path.value());
        let text = util::read_file(&path)
            .map_err(|e| crate::Error::include_file_error(e, self, path.clone()))?;
        Ok((path, text))
    }
}
impl PositionRange for Include {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Include {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-include({}).", self.path.text())
    }
}
impl ReadFrom for Include {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Include {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _include: reader.read_expected("include")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            path: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `include_lib` directive.
///
/// See [9.1 File Inclusion](http://erlang.org/doc/reference_manual/macros.html#id85412)
/// for detailed information.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct IncludeLib {
    pub _hyphen: SymbolToken,
    pub _include_lib: AtomToken,
    pub _open_paren: SymbolToken,
    pub path: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl IncludeLib {
    /// Executes file inclusion.
    pub fn include_lib(&self, code_paths: &VecDeque<PathBuf>) -> Result<(PathBuf, String)> {
        let mut path = util::substitute_path_variables(self.path.value());

        let temp_path = path.clone();
        let mut components = temp_path.components();
        if let Some(Component::Normal(app_name)) = components.next() {
            let app_name = app_name
                .to_str()
                .ok_or_else(|| crate::Error::non_utf8_path(&app_name))?;
            let pattern = format!("{}-*", app_name);
            'root: for root in code_paths.iter() {
                let pattern = root.join(&pattern);
                let pattern = pattern
                    .to_str()
                    .ok_or_else(|| crate::Error::non_utf8_path(&pattern))?;
                if let Some(entry) = glob(pattern)?.next() {
                    path = entry?;
                    for c in components {
                        path.push(c.as_os_str());
                    }
                    break 'root;
                }
            }
        }

        let text = util::read_file(&path)
            .map_err(|e| crate::Error::include_file_error(e, self, path.clone()))?;
        Ok((path, text))
    }
}
impl PositionRange for IncludeLib {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for IncludeLib {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-include_lib({}).", self.path.text())
    }
}
impl ReadFrom for IncludeLib {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(IncludeLib {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _include_lib: reader.read_expected("include_lib")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            path: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `error` directive.
///
/// See [9.6 -error() and -warning() directives][error_and_warning]
/// for detailed information.
///
/// [error_and_warning]: http://erlang.org/doc/reference_manual/macros.html#id85997
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Error {
    pub _hyphen: SymbolToken,
    pub _error: AtomToken,
    pub _open_paren: SymbolToken,
    pub message: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Error {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-error({}).", self.message.text())
    }
}
impl ReadFrom for Error {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Error {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _error: reader.read_expected("error")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            message: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `warning` directive.
///
/// See [9.6 -error() and -warning() directives][error_and_warning]
/// for detailed information.
///
/// [error_and_warning]: http://erlang.org/doc/reference_manual/macros.html#id85997
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Warning {
    pub _hyphen: SymbolToken,
    pub _warning: AtomToken,
    pub _open_paren: SymbolToken,
    pub message: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Warning {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-warning({}).", self.message.text())
    }
}
impl ReadFrom for Warning {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Warning {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _warning: reader.read_expected("warning")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            message: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `endif` directive.
///
/// See [9.5 Flow Control in Macros][flow_control] for detailed information.
///
/// [flow_control]: http://erlang.org/doc/reference_manual/macros.html#id85859
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Endif {
    pub _hyphen: SymbolToken,
    pub _endif: AtomToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Endif {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Endif {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-endif.")
    }
}
impl ReadFrom for Endif {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Endif {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _endif: reader.read_expected("endif")?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `else` directive.
///
/// See [9.5 Flow Control in Macros][flow_control] for detailed information.
///
/// [flow_control]: http://erlang.org/doc/reference_manual/macros.html#id85859
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Else {
    pub _hyphen: SymbolToken,
    pub _else: AtomToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Else {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Else {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-else.")
    }
}
impl ReadFrom for Else {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Else {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _else: reader.read_expected("else")?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `undef` directive.
///
/// See [9.5 Flow Control in Macros][flow_control] for detailed information.
///
/// [flow_control]: http://erlang.org/doc/reference_manual/macros.html#id85859
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Undef {
    pub _hyphen: SymbolToken,
    pub _undef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Undef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Undef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-undef({}).", self.name.text())
    }
}
impl ReadFrom for Undef {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Undef {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _undef: reader.read_expected("undef")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            name: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `ifdef` directive.
///
/// See [9.5 Flow Control in Macros][flow_control] for detailed information.
///
/// [flow_control]: http://erlang.org/doc/reference_manual/macros.html#id85859
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Ifdef {
    pub _hyphen: SymbolToken,
    pub _ifdef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Ifdef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Ifdef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-ifdef({}).", self.name.text())
    }
}
impl ReadFrom for Ifdef {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Ifdef {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _ifdef: reader.read_expected("ifdef")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            name: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `ifndef` directive.
///
/// See [9.5 Flow Control in Macros][flow_control] for detailed information.
///
/// [flow_control]: http://erlang.org/doc/reference_manual/macros.html#id85859
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Ifndef {
    pub _hyphen: SymbolToken,
    pub _ifndef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Ifndef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Ifndef {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "-ifndef({}).", self.name.text())
    }
}
impl ReadFrom for Ifndef {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Ok(Ifndef {
            _hyphen: reader.read_expected(&Symbol::Hyphen)?,
            _ifndef: reader.read_expected("ifndef")?,
            _open_paren: reader.read_expected(&Symbol::OpenParen)?,
            name: reader.read()?,
            _close_paren: reader.read_expected(&Symbol::CloseParen)?,
            _dot: reader.read_expected(&Symbol::Dot)?,
        })
    }
}

/// `define` directive.
///
/// See [9.2 Defining and Using Macros][define_and_use] for detailed information.
///
/// [define_and_use]: http://erlang.org/doc/reference_manual/macros.html#id85572
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub struct Define {
    pub _hyphen: SymbolToken,
    pub _define: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub variables: Option<MacroVariables>,
    pub _comma: SymbolToken,
    pub replacement: Vec<LexicalToken>,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Define {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}
impl fmt::Display for Define {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "-define({}{}, {}).",
            self.name,
            self.variables
                .as_ref()
                .map_or("".to_string(), ToString::to_string),
            self.replacement
                .iter()
                .map(LexicalToken::text)
                .collect::<String>()
        )
    }
}
impl ReadFrom for Define {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let _hyphen = reader.read_expected(&Symbol::Hyphen)?;
        let _define = reader.read_expected("define")?;
        let _open_paren = reader.read_expected(&Symbol::OpenParen)?;
        let name = reader.read()?;
        let variables =
            if let Some(token) = reader.try_read_expected::<SymbolToken>(&Symbol::OpenParen)? {
                reader.unread_token(token.into());
                Some(reader.read()?)
            } else {
                None
            };
        let _comma = reader.read_expected(&Symbol::Comma)?;

        let mut replacement = Vec::new();
        loop {
            if let Some(_close_paren) = reader.try_read_expected(&Symbol::CloseParen)? {
                if let Some(_dot) = reader.try_read_expected(&Symbol::Dot)? {
                    return Ok(Define {
                        _hyphen,
                        _define,
                        _open_paren,
                        name,
                        variables,
                        _comma,
                        replacement,
                        _close_paren,
                        _dot,
                    });
                }
                replacement.push(_close_paren.into());
            } else {
                let token = reader.read_token()?;
                if token
                    .as_symbol_token()
                    .map_or(false, |s| s.value() == Symbol::Dot)
                {
                    return Err(crate::Error::unexpected_dot_in_macro_def(&token));
                }
                replacement.push(token);
            }
        }
    }
}
