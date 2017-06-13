use std::collections::{HashMap, VecDeque};
use std::path::Path;
use erl_tokenize::{Token, Tokenizer, Position, PositionRange};
use erl_tokenize::tokens::{AtomToken, SymbolToken, VariableToken, StringToken, IntegerToken};
use erl_tokenize::values::Symbol;

use {Result, ErrorKind};
use directive::{MacroName, Define, List, Tail};

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct MacroArgs {
    pub _open_paren: SymbolToken,
    pub list: List<MacroArg>,
    pub _close_paren: SymbolToken,
}
impl MacroArgs {
    pub fn len(&self) -> usize {
        self.list.iter().count()
    }
}
impl PositionRange for MacroArgs {
    fn start_position(&self) -> Position {
        self._open_paren.start_position()
    }
    fn end_position(&self) -> Position {
        self._close_paren.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct MacroArg {
    tokens: Vec<Token>,
}
impl PositionRange for MacroArg {
    fn start_position(&self) -> Position {
        self.tokens.first().as_ref().unwrap().start_position()
    }
    fn end_position(&self) -> Position {
        self.tokens.last().as_ref().unwrap().end_position()
    }
}

#[derive(Debug)]
pub struct TokenReader<T> {
    tokens: T,
    included_tokens: Vec<Tokenizer<String>>,
    unread: VecDeque<Token>,
    pub macros: HashMap<MacroName, Define>,
    pub macro_calls: Vec<MacroCall>,

    pub module_name: Option<String>,
    pub function_name: Option<String>,
    pub function_arity: Option<usize>,
}
impl<T> TokenReader<T>
    where T: Iterator<Item = Result<Token>>
{
    pub fn new(tokens: T) -> Self {
        TokenReader {
            tokens,
            included_tokens: Vec::new(),
            unread: VecDeque::new(),
            macros: HashMap::new(),
            macro_calls: Vec::new(),
            module_name: None,
            function_name: None,
            function_arity: None,
        }
    }
    pub fn read_macro_name(&mut self) -> Result<MacroName> {
        if let Some(atom) = track_try!(self.read_atom()) {
            Ok(MacroName::Atom(atom))
        } else if let Some(var) = track_try!(self.read_variable()) {
            Ok(MacroName::Variable(var))
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Invalid macro name: {:?}",
                         self.read());
        }
    }
    fn read_macro_arg(&mut self) -> Result<Option<MacroArg>> {
        let mut stack = Vec::new();
        let mut arg = Vec::new();
        while let Some(token) = track_try!(self.read()) {
            if let Token::Symbol(ref s) = token {
                match s.value() {
                    Symbol::CloseParen if stack.is_empty() => {
                        self.unread(s.clone().into());
                        return if arg.is_empty() {
                            Ok(None)
                        } else {
                            Ok(Some(MacroArg { tokens: arg }))
                        };
                    }
                    Symbol::Comma if stack.is_empty() => {
                        track_assert_ne!(arg.len(), 0, ErrorKind::InvalidInput);
                        self.unread(s.clone().into());
                        return Ok(Some(MacroArg { tokens: arg }));
                    }
                    Symbol::OpenParen | Symbol::OpenBrace | Symbol::OpenSquare |
                    Symbol::DoubleLeftAngle => {
                        stack.push(s.clone());
                    }
                    Symbol::CloseParen | Symbol::CloseBrace | Symbol::CloseSquare |
                    Symbol::DoubleRightAngle => {
                        let last = track_try!(stack.pop().ok_or(ErrorKind::InvalidInput));
                        let expected = match last.value() {
                            Symbol::OpenParen => Symbol::CloseParen,
                            Symbol::OpenBrace => Symbol::CloseBrace,
                            Symbol::OpenSquare => Symbol::CloseSquare,
                            Symbol::DoubleLeftAngle => Symbol::DoubleRightAngle,
                            _ => unreachable!(),
                        };
                        track_assert_eq!(s.value(), expected, ErrorKind::InvalidInput);
                    }
                    _ => {}
                }
            }
            arg.push(token);
        }
        track_panic!(ErrorKind::UnexpectedEos);
    }
    fn read_macro_args_tail(&mut self) -> Result<Tail<MacroArg>> {
        if let Some(comma) = track_try!(self.read_symbol_if(Symbol::Comma)) {
            let arg = track_try!(track_try!(self.read_macro_arg()).ok_or(ErrorKind::InvalidInput));
            Ok(Tail::Cons {
                   _comma: comma,
                   head: arg,
                   tail: Box::new(track_try!(self.read_macro_args_tail())),
               })
        } else {
            Ok(Tail::Null)
        }
    }
    fn read_macro_args(&mut self) -> Result<(List<MacroArg>, SymbolToken)> {
        let mut list = List::Null;
        if let Some(arg) = track_try!(self.read_macro_arg()) {
            list = List::Cons {
                head: arg,
                tail: track_try!(self.read_macro_args_tail()),
            };
        }

        let _close_paren = track_try!(self.read_expected_symbol_or_error(Symbol::CloseParen));
        Ok((list, _close_paren))
    }
    fn read_macro_call(&mut self, _question: SymbolToken) -> Result<MacroCall> {
        let name = track_try!(self.read_macro_name());

        // TODO: refactor
        let pos = _question.start_position();
        let replacement: Option<Token> = match name.value() {
            "MODULE" => {
                let module = track_try!(self.module_name.as_ref().ok_or(ErrorKind::InvalidInput));
                let t = track_try!(AtomToken::from_text(&format!("'{}'", module), pos.clone()))
                    .into();
                Some(t)
            }
            "MODULE_STRING" => {
                let module = track_try!(self.module_name.as_ref().ok_or(ErrorKind::InvalidInput));
                let t = track_try!(StringToken::from_text(&format!("{:?}", module), pos.clone()))
                    .into();
                Some(t)
            }
            "FILE" => {
                let file = track_try!(pos.filepath().ok_or(ErrorKind::InvalidInput));
                let t = track_try!(StringToken::from_text(&format!("{:?}", file), pos.clone()))
                    .into();
                Some(t)
            }
            "LINE" => Some(IntegerToken::from_value(pos.line().into(), pos).into()),
            "MACHINE" => Some(track_try!(AtomToken::from_text("'BEAM'", pos)).into()),
            "FUNCTION_NAME" => {
                let name = track_try!(self.function_name.as_ref().ok_or(ErrorKind::InvalidInput));
                let t = track_try!(AtomToken::from_text(&format!("'{}'", name), pos.clone()))
                    .into();
                Some(t)
            }
            "FUNCTION_ARITY" => {
                let arity = track_try!(self.function_arity.ok_or(ErrorKind::InvalidInput));
                let t = IntegerToken::from_value(arity.into(), pos.clone()).into();
                Some(t)
            }
            _ => None,
        };
        if let Some(replacement) = replacement {
            use std::iter;
            self.unread.extend(iter::once(replacement));
            return Ok(MacroCall {
                          _question,
                          name,
                          args: None,
                      });
        }

        let definition = track_try!(self.macros.get(&name).ok_or(ErrorKind::InvalidInput),
                                    "Undefined macro: {:?}",
                                    name)
            .clone();
        let call = if let Some(ref vars) = definition.variables {
            let _open_paren = track_try!(self.read_expected_symbol_or_error(Symbol::OpenParen));
            let (args, _close_paren) = track_try!(self.read_macro_args());
            let args = MacroArgs {
                _open_paren,
                list: args,
                _close_paren,
            };
            track_assert_eq!(args.len(), vars.len(), ErrorKind::InvalidInput);
            let replacement =
                track_try!(definition.expand(args.list.iter().map(|a| &a.tokens[..]).collect()));
            self.unread.extend(replacement);
            MacroCall {
                _question,
                name,
                args: Some(args),
            }
        } else {
            self.unread.extend(definition.replacement.clone());
            MacroCall {
                _question,
                name,
                args: None,
            }
        };
        Ok(call)
    }
    pub fn push_text<P: AsRef<Path>>(&mut self, path: P, text: String) {
        let mut tokenizer = Tokenizer::new(text);
        tokenizer.set_filepath(path);
        self.included_tokens.push(tokenizer);
    }
    pub fn read(&mut self) -> Result<Option<Token>> {
        while let Some(token) = track_try!(self.read_impl()) {
            match token {
                Token::Symbol(ref t) if t.value() == Symbol::Question => {
                    let call = track_try!(self.read_macro_call(t.clone()));
                    self.macro_calls.push(call);
                }
                Token::Whitespace(_) |
                Token::Comment(_) => {}
                _ => return Ok(Some(token)),
            }
        }
        Ok(None)
    }
    fn read_impl(&mut self) -> Result<Option<Token>> {
        if let Some(token) = self.unread.pop_front() {
            Ok(Some(token))
        } else if !self.included_tokens.is_empty() {
            match self.included_tokens.last_mut().expect("Never fails").next() {
                None => {
                    self.included_tokens.pop();
                    self.read()
                }
                Some(Err(e)) => Err(e),
                Some(Ok(t)) => Ok(Some(t)),
            }
        } else {
            match self.tokens.next() {
                None => Ok(None),
                Some(Err(e)) => Err(e),
                Some(Ok(t)) => Ok(Some(t)),
            }
        }
    }
    // pub fn read_or_error(&mut self) -> Result<Token> {
    //     if let Some(token) = track_try!(self.read()) {
    //         Ok(token)
    //     } else {
    //         track_panic!(ErrorKind::UnexpectedEos)
    //     }
    // }

    pub fn unread(&mut self, token: Token) {
        self.unread.push_front(token);
    }
    // pub fn skip_whitespace_or_comment(&mut self) -> Result<()> {
    //     while let Some(token) = track_try!(self.read()) {
    //         match token {
    //             Token::Whitespace(_) |
    //             Token::Comment(_) => {}
    //             _ => {
    //                 self.unread(token);
    //                 break;
    //             }
    //         }
    //     }
    //     Ok(())
    // }
    pub fn read_atom(&mut self) -> Result<Option<AtomToken>> {
        if let Some(token) = track_try!(self.read()) {
            if let Token::Atom(t) = token {
                Ok(Some(t))
            } else {
                self.unread(token);
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
    // pub fn read_symbol(&mut self) -> Result<Option<SymbolToken>> {
    //     if let Some(token) = track_try!(self.read()) {
    //         if let Token::Symbol(t) = token {
    //             Ok(Some(t))
    //         } else {
    //             self.unread(token);
    //             Ok(None)
    //         }
    //     } else {
    //         Ok(None)
    //     }
    // }
    pub fn read_variable(&mut self) -> Result<Option<VariableToken>> {
        if let Some(token) = track_try!(self.read()) {
            if let Token::Variable(t) = token {
                Ok(Some(t))
            } else {
                self.unread(token);
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
    pub fn read_string(&mut self) -> Result<Option<StringToken>> {
        if let Some(token) = track_try!(self.read()) {
            if let Token::String(t) = token {
                Ok(Some(t))
            } else {
                self.unread(token);
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
    pub fn read_variable_or_error(&mut self) -> Result<VariableToken> {
        if let Some(t) = track_try!(self.read_variable()) {
            Ok(t)
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Unexpected token: actual={:?}, expected=VariableToken",
                         self.read());
        }
    }
    // pub fn read_symbol_or_error(&mut self) -> Result<SymbolToken> {
    //     if let Some(t) = track_try!(self.read_symbol()) {
    //         Ok(t)
    //     } else {
    //         track_panic!(ErrorKind::InvalidInput,
    //                      "Unexpected token: actual={:?}, expected=SymbolToken",
    //                      self.read());
    //     }
    // }
    pub fn read_string_or_error(&mut self) -> Result<StringToken> {
        if let Some(t) = track_try!(self.read_string()) {
            Ok(t)
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Unexpected token: actual={:?}, expected=StringToken",
                         self.read());
        }
    }
    pub fn read_symbol_if(&mut self, expected: Symbol) -> Result<Option<SymbolToken>> {
        if let Some(token) = track_try!(self.read()) {
            if let Token::Symbol(t) = token {
                if t.value() == expected {
                    Ok(Some(t))
                } else {
                    self.unread(t.into());
                    Ok(None)
                }
            } else {
                self.unread(token);
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
    pub fn read_expected_symbol_or_error(&mut self, expected: Symbol) -> Result<SymbolToken> {
        if let Some(s) = track_try!(self.read_symbol_if(expected)) {
            Ok(s)
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Unexpected token: actual={:?}, expected={:?}",
                         self.read(),
                         expected);
        }
    }
}
