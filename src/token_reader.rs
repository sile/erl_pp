use erl_tokenize::tokens::{AtomToken, StringToken, SymbolToken, VariableToken};
use erl_tokenize::values::Symbol;
use erl_tokenize::{Lexer, LexicalToken};
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::path::Path;

use crate::macros::NoArgsMacroCall;
use crate::{Error, MacroCall, MacroDef, Result};

#[derive(Debug)]
pub struct TokenReader<T> {
    tokens: T,
    included_tokens: Vec<Lexer<String>>,
    unread: VecDeque<LexicalToken>,
}
impl<T> TokenReader<T>
where
    T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
{
    pub fn new(tokens: T) -> Self {
        TokenReader {
            tokens,
            included_tokens: Vec::new(),
            unread: VecDeque::new(),
        }
    }

    pub fn add_included_text<P: AsRef<Path>>(&mut self, path: P, text: String) {
        let mut lexer = Lexer::new(text);
        lexer.set_filepath(path);
        self.included_tokens.push(lexer);
    }

    pub fn read<V>(&mut self) -> Result<V>
    where
        V: ReadFrom,
    {
        V::read_from(self)
    }
    pub fn try_read<V>(&mut self) -> Result<Option<V>>
    where
        V: ReadFrom,
    {
        V::try_read_from(self)
    }
    pub fn try_read_macro_call(
        &mut self,
        macros: &HashMap<String, MacroDef>,
    ) -> Result<Option<MacroCall>> {
        if let Some(call) = self.try_read::<NoArgsMacroCall>()? {
            let mut call = MacroCall {
                _question: call._question,
                name: call.name,
                args: None,
            };
            if macros
                .get(call.name.value())
                .map_or(false, MacroDef::has_variables)
            {
                call.args = Some(self.read()?);
            }
            Ok(Some(call))
        } else {
            Ok(None)
        }
    }
    pub fn read_expected<V>(&mut self, expected: &V::Value) -> Result<V>
    where
        V: ReadFrom + Expect + Into<LexicalToken>,
    {
        V::read_expected(self, expected)
    }
    pub fn try_read_expected<V>(&mut self, expected: &V::Value) -> Result<Option<V>>
    where
        V: ReadFrom + Expect + Into<LexicalToken>,
    {
        V::try_read_expected(self, expected)
    }
    pub fn try_read_token(&mut self) -> Result<Option<LexicalToken>> {
        if let Some(token) = self.unread.pop_front() {
            Ok(Some(token))
        } else if !self.included_tokens.is_empty() {
            match self
                .included_tokens
                .last_mut()
                .expect("unreachable")
                .next()
                .transpose()?
            {
                None => {
                    self.included_tokens.pop();
                    self.try_read_token()
                }
                Some(t) => Ok(Some(t)),
            }
        } else {
            match self.tokens.next().transpose()? {
                None => Ok(None),
                Some(t) => Ok(Some(t)),
            }
        }
    }
    pub fn read_token(&mut self) -> Result<LexicalToken> {
        if let Some(token) = self.try_read_token()? {
            Ok(token)
        } else {
            Err(Error::UnexpectedEof)
        }
    }
    pub fn unread_token(&mut self, token: LexicalToken) {
        self.unread.push_front(token);
    }
}

pub trait ReadFrom: Sized {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>;
    fn try_read_from<T>(reader: &mut TokenReader<T>) -> Result<Option<Self>>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        Self::read_from(reader).map(Some).or_else(|e| {
            if let Error::UnexpectedToken { token, .. } = e {
                reader.unread_token(token);
                return Ok(None);
            }
            if let Error::UnexpectedEof = e {
                return Ok(None);
            }
            Err(e)
        })
    }
    fn read_expected<T>(reader: &mut TokenReader<T>, expected: &Self::Value) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
        Self: Expect + Into<LexicalToken>,
    {
        Self::read_from(reader).and_then(|token| {
            if !token.expect(expected) {
                return Err(Error::unexpected_token(
                    token.into(),
                    &format!("{:?}", expected),
                ));
            }
            Ok(token)
        })
    }
    fn try_read_expected<T>(
        reader: &mut TokenReader<T>,
        expected: &Self::Value,
    ) -> Result<Option<Self>>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
        Self: Expect + Into<LexicalToken>,
    {
        Self::try_read_from(reader).map(|token| {
            token.and_then(|token| {
                if token.expect(expected) {
                    Some(token)
                } else {
                    reader.unread_token(token.into());
                    None
                }
            })
        })
    }
}
impl ReadFrom for AtomToken {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let token = reader.read_token()?;
        token
            .into_atom_token()
            .map_err(|token| Error::unexpected_token(token, "atom"))
    }
}
impl ReadFrom for VariableToken {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let token = reader.read_token()?;
        token
            .into_variable_token()
            .map_err(|token| Error::unexpected_token(token, "variable"))
    }
}
impl ReadFrom for SymbolToken {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let token = reader.read_token()?;
        token
            .into_symbol_token()
            .map_err(|token| Error::unexpected_token(token, "symbol"))
    }
}
impl ReadFrom for StringToken {
    fn read_from<T>(reader: &mut TokenReader<T>) -> Result<Self>
    where
        T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
    {
        let token = reader.read_token()?;
        token
            .into_string_token()
            .map_err(|token| Error::unexpected_token(token, "string"))
    }
}

pub trait Expect {
    type Value: PartialEq + Debug + ?Sized;
    fn expect(&self, expected: &Self::Value) -> bool;
}
impl Expect for AtomToken {
    type Value = str;
    fn expect(&self, expected: &Self::Value) -> bool {
        self.value() == expected
    }
}
impl Expect for SymbolToken {
    type Value = Symbol;
    fn expect(&self, expected: &Self::Value) -> bool {
        self.value() == *expected
    }
}
