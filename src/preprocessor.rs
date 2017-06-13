use std::path::PathBuf;
use erl_tokenize::LexicalToken;
use erl_tokenize::tokens::{VariableToken, AtomToken, SymbolToken};
use erl_tokenize::values::Symbol;

use {Result, Directive, ErrorKind};
use directive;
use token_reader::TokenReader;

#[derive(Debug)]
pub struct Preprocessor<T> {
    reader: TokenReader<T>,
    can_directive_start: bool,
    directives: Vec<Directive>,
    code_paths: Vec<PathBuf>,
    branches: Vec<bool>,
}
impl<T> Preprocessor<T>
    where T: Iterator<Item = Result<LexicalToken>>
{
    pub fn new(tokens: T) -> Self {
        Preprocessor {
            reader: TokenReader::new(tokens),
            can_directive_start: true,
            directives: Vec::new(),
            code_paths: Vec::new(),
            branches: Vec::new(),
        }
    }
    fn read(&mut self) -> Result<Option<LexicalToken>> {
        track!(self.reader.read())
    }
    fn ignore(&self) -> bool {
        self.branches.iter().find(|b| **b == false).is_some()
    }
    fn next_token(&mut self) -> Result<Option<LexicalToken>> {
        if self.can_directive_start {
            if let Some(d) = track_try!(self.try_read_directive()) {
                self.directives.push(d);
            }
        }

        if let Some(token) = track_try!(self.read()) {
            if self.ignore() {
                return self.next_token(); // TODO: loop
            }
            match token {
                LexicalToken::Symbol(ref s) => {
                    self.can_directive_start = s.value() == Symbol::Dot;
                }
                _ => self.can_directive_start = false,
            }
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }
    fn try_read_directive(&mut self) -> Result<Option<Directive>> {
        let hyphen = if let Some(token) = track_try!(self.reader.read_symbol_if(Symbol::Hyphen)) {
            token
        } else {
            return Ok(None);
        };

        let ignore = self.ignore();
        if let Some(atom) = track_try!(self.reader.read_atom()) {
            match atom.value() {
                "include" if !ignore => {
                    let d = track_try!(self.read_include_directive(hyphen, atom));
                    let (path, text) = track_try!(d.include());
                    self.reader.push_text(path, text);
                    return Ok(Some(Directive::Include(d)));
                }
                "include_lib" if !ignore => {
                    let d = track_try!(self.read_include_lib_directive(hyphen, atom));
                    let (path, text) = track_try!(d.include_lib(&self.code_paths));
                    self.reader.push_text(path, text);
                    return Ok(Some(Directive::IncludeLib(d)));
                }
                "define" if !ignore => {
                    let d = track_try!(self.read_define_directive(hyphen, atom));
                    self.reader.macros.insert(d.name.clone(), d.clone());
                    return Ok(Some(Directive::Define(d)));
                }
                "undef" if !ignore => {
                    let d = track_try!(self.read_undef_directive(hyphen, atom));
                    self.reader.macros.remove(&d.name);
                    return Ok(Some(Directive::Undef(d)));
                }
                "ifdef" => {
                    let d = track_try!(self.read_ifdef_directive(hyphen, atom));
                    let entered = self.reader.macros.contains_key(&d.name);
                    self.branches.push(entered);
                    return Ok(Some(Directive::Ifdef(d)));
                }
                "ifndef" => {
                    let d = track_try!(self.read_ifndef_directive(hyphen, atom));
                    let entered = !self.reader.macros.contains_key(&d.name);
                    self.branches.push(entered);
                    return Ok(Some(Directive::Ifndef(d)));
                }
                "else" => {
                    let d = track_try!(self.read_else_directive(hyphen, atom));
                    let b = track_try!(self.branches.last_mut().ok_or(ErrorKind::InvalidInput));
                    *b = !*b;
                    return Ok(Some(Directive::Else(d)));
                }
                "endif" => {
                    let d = track_try!(self.read_endif_directive(hyphen, atom));
                    track_assert!(self.branches.pop().is_some(), ErrorKind::InvalidInput);
                    return Ok(Some(Directive::Endif(d)));
                }
                "error" if !ignore => {
                    let d = track_try!(self.read_error_directive(hyphen, atom));
                    return Ok(Some(Directive::Error(d)));
                }
                "warning" if !ignore => {
                    let d = track_try!(self.read_warning_directive(hyphen, atom));
                    return Ok(Some(Directive::Warning(d)));
                }
                _ => {
                    self.reader.unread(atom.into());
                    self.reader.unread(hyphen.into());
                }
            }
        }

        Ok(None)
    }
    fn read_define_directive(&mut self,
                             _hyphen: SymbolToken,
                             _define: AtomToken)
                             -> Result<directive::Define> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        let name = track_try!(self.reader.read_macro_name());

        // macro variables
        let variables = if let Some(open) = track_try!(self.reader
                                                           .read_symbol_if(Symbol::OpenParen)) {
            Some(track_try!(self.read_macro_variables(open)))
        } else {
            None
        };

        // ','
        let _comma = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Comma));

        // macro replacement, ')', '.'
        let (replacement, _close_paren, _dot) = track_try!(self.read_macro_replacement());

        Ok(directive::Define {
               _hyphen,
               _define,
               _open_paren,
               name,
               variables,
               _comma,
               replacement,
               _close_paren,
               _dot,
           })
    }
    fn read_undef_directive(&mut self,
                            _hyphen: SymbolToken,
                            _undef: AtomToken)
                            -> Result<directive::Undef> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        let name = track_try!(self.reader.read_macro_name());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Undef {
               _hyphen,
               _undef,
               _open_paren,
               name,
               _close_paren,
               _dot,
           })
    }
    fn read_endif_directive(&mut self,
                            _hyphen: SymbolToken,
                            _endif: AtomToken)
                            -> Result<directive::Endif> {
        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Endif {
               _hyphen,
               _endif,
               _dot,
           })
    }
    fn read_else_directive(&mut self,
                           _hyphen: SymbolToken,
                           _else: AtomToken)
                           -> Result<directive::Else> {
        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Else {
               _hyphen,
               _else,
               _dot,
           })
    }
    fn read_ifdef_directive(&mut self,
                            _hyphen: SymbolToken,
                            _ifdef: AtomToken)
                            -> Result<directive::Ifdef> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        let name = track_try!(self.reader.read_macro_name());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Ifdef {
               _hyphen,
               _ifdef,
               _open_paren,
               name,
               _close_paren,
               _dot,
           })
    }
    fn read_ifndef_directive(&mut self,
                             _hyphen: SymbolToken,
                             _ifndef: AtomToken)
                             -> Result<directive::Ifndef> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        let name = track_try!(self.reader.read_macro_name());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Ifndef {
               _hyphen,
               _ifndef,
               _open_paren,
               name,
               _close_paren,
               _dot,
           })
    }
    fn read_include_directive(&mut self,
                              _hyphen: SymbolToken,
                              _include: AtomToken)
                              -> Result<directive::Include> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // path
        let path = track_try!(self.reader.read_string_or_error());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Include {
               _hyphen,
               _include,
               _open_paren,
               path,
               _close_paren,
               _dot,
           })
    }
    fn read_include_lib_directive(&mut self,
                                  _hyphen: SymbolToken,
                                  _include_lib: AtomToken)
                                  -> Result<directive::IncludeLib> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // path
        let path = track_try!(self.reader.read_string_or_error());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::IncludeLib {
               _hyphen,
               _include_lib,
               _open_paren,
               path,
               _close_paren,
               _dot,
           })
    }
    fn read_error_directive(&mut self,
                            _hyphen: SymbolToken,
                            _error: AtomToken)
                            -> Result<directive::Error> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // message
        let message = track_try!(self.reader.read_string_or_error());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Error {
               _hyphen,
               _error,
               _open_paren,
               message,
               _close_paren,
               _dot,
           })
    }
    fn read_warning_directive(&mut self,
                              _hyphen: SymbolToken,
                              _warning: AtomToken)
                              -> Result<directive::Warning> {
        // '('
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // message
        let message = track_try!(self.reader.read_string_or_error());

        // ')'
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Warning {
               _hyphen,
               _warning,
               _open_paren,
               message,
               _close_paren,
               _dot,
           })
    }
    fn read_macro_replacement(&mut self) -> Result<(Vec<LexicalToken>, SymbolToken, SymbolToken)> {
        let mut tokens = Vec::new();
        while let Some(token) = track_try!(self.reader.read()) {
            if let LexicalToken::Symbol(ref symbol) = token {
                if symbol.value() == Symbol::CloseParen {
                    if let Some(dot) = track_try!(self.reader.read_symbol_if(Symbol::Dot)) {
                        return Ok((tokens, symbol.clone(), dot));
                    }
                }
            }
            tokens.push(token);
        }
        track_panic!(ErrorKind::UnexpectedEos);
    }
    fn read_list_tail(&mut self) -> Result<directive::Tail<VariableToken>> {
        if let Some(comma) = track_try!(self.reader.read_symbol_if(Symbol::Comma)) {
            let var = track_try!(self.reader.read_variable_or_error());
            Ok(directive::Tail::Cons {
                   _comma: comma,
                   head: var,
                   tail: Box::new(track_try!(self.read_list_tail())),
               })
        } else {
            Ok(directive::Tail::Null)
        }
    }
    fn read_macro_variables(&mut self,
                            _open_paren: SymbolToken)
                            -> Result<directive::MacroVariables> {
        let mut list = directive::List::Null;
        if let Some(var) = track_try!(self.reader.read_variable()) {
            list = directive::List::Cons {
                head: var,
                tail: track_try!(self.read_list_tail()),
            };
        }

        //
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));
        Ok(directive::MacroVariables {
               _open_paren,
               list,
               _close_paren,
           })
    }
}
impl<T> Iterator for Preprocessor<T>
    where T: Iterator<Item = Result<LexicalToken>>
{
    type Item = Result<LexicalToken>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_token() {
            Err(e) => Some(Err(e)),
            Ok(None) => None,
            Ok(Some(token)) => Some(Ok(token)),
        }
    }
}
