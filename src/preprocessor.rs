use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use erl_tokenize::{Token, Tokenizer, Position, TokenValue, PositionRange};
use erl_tokenize::tokens::{VariableToken, AtomToken, SymbolToken};
use erl_tokenize::values::Symbol;

use {Result, Directive, ErrorKind};
use directive::{self, MacroDef, MacroName, Undef};
use directive::Directive2;
use token_reader::TokenReader;

#[derive(Debug)]
pub struct Preprocessor2<'a> {
    reader: TokenReader<'a>,
    can_directive_start: bool,
    macros: HashMap<MacroName, usize>,
    directives: Vec<Directive2>,
    code_paths: Vec<PathBuf>,
    buffer: VecDeque<Token>,
}
impl<'a> Preprocessor2<'a> {
    pub fn new(tokens: Tokenizer<'a>) -> Self {
        Preprocessor2 {
            reader: TokenReader::new(tokens),
            can_directive_start: true,
            macros: HashMap::new(),
            directives: Vec::new(),
            code_paths: Vec::new(),
            buffer: VecDeque::new(),
        }
    }
    fn read(&mut self) -> Result<Option<Token>> {
        if let Some(token) = self.buffer.pop_front() {
            Ok(Some(token))
        } else {
            track!(self.reader.read())
        }
    }
    fn next_token(&mut self) -> Result<Option<Token>> {
        if self.can_directive_start && self.buffer.is_empty() {
            if let Some(d) = track_try!(self.try_read_directive()) {
                self.directives.push(d);
            }
        }

        if let Some(token) = track_try!(self.read()) {
            match token {
                Token::Whitespace(_) |
                Token::Comment(_) => {}
                Token::Symbol(ref s) => {
                    self.can_directive_start = s.value() == Symbol::Dot;
                }
                _ => self.can_directive_start = false,
            }
            Ok(Some(token))
        } else {
            Ok(None)
        }
    }
    fn skip_whitespace_or_comment(&mut self) -> Result<()> {
        while let Some(token) = track_try!(self.reader.read_whitespace_or_comment()) {
            self.buffer.push_back(token);
        }
        Ok(())
    }
    fn try_read_directive(&mut self) -> Result<Option<Directive2>> {
        assert!(self.buffer.is_empty());
        let hyphen = if let Some(token) = track_try!(self.reader.read_symbol_if(Symbol::Hyphen)) {
            token
        } else {
            return Ok(None);
        };

        track_try!(self.skip_whitespace_or_comment());
        if let Some(atom) = track_try!(self.reader.read_atom()) {
            match atom.value() {
                "include" => unimplemented!(),
                "include_lib" => unimplemented!(),
                "define" => {
                    let d = track_try!(self.read_define_directive(hyphen, atom));
                    self.macros.insert(d.name.clone(), self.directives.len());
                    return Ok(Some(Directive2::Define(d)));
                }
                "undef" => {
                    let d = track_try!(self.read_undef_directive(hyphen, atom));
                    self.macros.remove(&d.name);
                    return Ok(Some(Directive2::Undef(d)));
                }
                "ifdef" => unimplemented!(),
                "ifndef" => unimplemented!(),
                "else" => unimplemented!(),
                "endif" => unimplemented!(),
                "error" => {
                    // let d = track_try!(self.read_error_directive());
                    // return Ok(Some(Directive::Error(d)));
                    unimplemented!()
                }
                "warning" => {
                    // let d = track_try!(self.read_warning_directive());
                    // return Ok(Some(Directive::Warning(d)));
                    unimplemented!()
                }
                _ => {
                    self.buffer.push_front(hyphen.into());
                    self.buffer.push_back(atom.into());
                }
            }
        }

        Ok(None)
    }
    fn read_macro_name(&mut self) -> Result<MacroName> {
        if let Some(atom) = track_try!(self.reader.read_atom()) {
            Ok(MacroName::Atom(atom))
        } else if let Some(var) = track_try!(self.reader.read_variable()) {
            Ok(MacroName::Variable(var))
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Invalid macro name: {:?}",
                         self.reader.read());
        }
    }
    fn read_define_directive(&mut self,
                             _hyphen: SymbolToken,
                             _define: AtomToken)
                             -> Result<directive::Define> {
        // '('
        track_try!(self.skip_whitespace_or_comment());
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        track_try!(self.skip_whitespace_or_comment());
        let name = track_try!(self.read_macro_name());

        // macro variables
        track_try!(self.skip_whitespace_or_comment());
        let variables = if let Some(open) = track_try!(self.reader
                                                           .read_symbol_if(Symbol::OpenParen)) {
            Some(track_try!(self.read_macro_variables(open)))
        } else {
            None
        };

        // ','
        track_try!(self.skip_whitespace_or_comment());
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
                            -> Result<directive::Undef2> {
        // '('
        track_try!(self.skip_whitespace_or_comment());
        let _open_paren = track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        track_try!(self.skip_whitespace_or_comment());
        let name = track_try!(self.read_macro_name());

        // ')'
        track_try!(self.skip_whitespace_or_comment());
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        track_try!(self.skip_whitespace_or_comment());
        let _dot = track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(directive::Undef2 {
               _hyphen,
               _undef,
               _open_paren,
               name,
               _close_paren,
               _dot,
           })
    }
    fn read_macro_replacement(&mut self) -> Result<(Vec<Token>, SymbolToken, SymbolToken)> {
        let mut tokens = Vec::new();
        while let Some(token) = track_try!(self.reader.read()) {
            if let Token::Symbol(symbol) = token {
                if symbol.value() == Symbol::CloseParen {
                    let mut ignores = Vec::new();
                    while let Some(token) = track_try!(self.reader.read_whitespace_or_comment()) {
                        ignores.push(token);
                    }
                    if let Some(dot) = track_try!(self.reader.read_symbol_if(Symbol::Dot)) {
                        self.buffer.extend(ignores);
                        return Ok((tokens, symbol, dot));
                    }
                    tokens.push(symbol.into());
                    tokens.extend(ignores);
                }
            } else {
                tokens.push(token);
            }
        }
        track_panic!(ErrorKind::UnexpectedEos);
    }
    fn read_list_tail(&mut self) -> Result<directive::Tail<VariableToken>> {
        track_try!(self.skip_whitespace_or_comment());
        if let Some(comma) = track_try!(self.reader.read_symbol_if(Symbol::Comma)) {
            track_try!(self.skip_whitespace_or_comment());
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
        track_try!(self.skip_whitespace_or_comment());
        if let Some(var) = track_try!(self.reader.read_variable()) {
            list = directive::List::Cons {
                head: var,
                tail: track_try!(self.read_list_tail()),
            };
        }

        //
        track_try!(self.skip_whitespace_or_comment());
        let _close_paren = track_try!(self.reader
                                          .read_expected_symbol_or_error(Symbol::CloseParen));
        Ok(directive::MacroVariables {
               _open_paren,
               list,
               _close_paren,
           })
    }
}
impl<'a> Iterator for Preprocessor2<'a> {
    type Item = Result<Token>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_token() {
            Err(e) => Some(Err(e)),
            Ok(None) => None,
            Ok(Some(token)) => Some(Ok(token)),
        }
    }
}

#[derive(Debug)]
pub struct Preprocessor<'a> {
    reader: TokenReader<'a>,
    can_directive_start: bool,
    macros: HashMap<MacroName, usize>,
    directives: Vec<Directive>,
    code_paths: Vec<PathBuf>,
}
impl<'a> Preprocessor<'a> {
    pub fn new(tokens: Tokenizer<'a>) -> Self {
        Preprocessor {
            reader: TokenReader::new(tokens),
            can_directive_start: true,
            macros: HashMap::new(),
            directives: Vec::new(),
            code_paths: Vec::new(),
        }
    }
    fn next_token(&mut self) -> Result<Option<Token>> {
        if self.can_directive_start {
            self.reader.start_transaction();
            if let Some(d) = track_try!(self.try_read_directive()) {
                self.directives.push(d);
            } else {
                self.reader.abort_transaction();
            }
        }

        if let Some(token) = track_try!(self.reader.read()) {
            match token {
                Token::Whitespace(_) |
                Token::Comment(_) => {}
                Token::Symbol(ref s) => {
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
        if track_try!(self.reader.read_symbol_if(Symbol::Hyphen)).is_none() {
            return Ok(None);
        }
        track_try!(self.reader.skip_whitespace_or_comment());

        if let Some(atom) = track_try!(self.reader.read_atom()) {
            match atom.value() {
                "include" => unimplemented!(),
                "include_lib" => unimplemented!(),
                "define" => {
                    let d = track_try!(self.read_define_directive());
                    self.macros.insert(d.name.clone(), self.directives.len());
                    return Ok(Some(Directive::Define(d)));
                }
                "undef" => {
                    let d = track_try!(self.read_undef_directive());
                    self.macros.remove(&d.name);
                    return Ok(Some(Directive::Undef(d)));
                }
                "ifdef" => unimplemented!(),
                "ifndef" => unimplemented!(),
                "else" => unimplemented!(),
                "endif" => unimplemented!(),
                "error" => {
                    let d = track_try!(self.read_error_directive());
                    return Ok(Some(Directive::Error(d)));
                }
                "warning" => {
                    let d = track_try!(self.read_warning_directive());
                    return Ok(Some(Directive::Warning(d)));
                }
                _ => {}
            }
        }

        Ok(None)
    }
    fn read_error_directive(&mut self) -> Result<directive::Error> {
        // '('
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        let message_start = self.reader.position();
        let message_end = track_try!(self.skip_remaining_directive_tokens());

        Ok(directive::Error {
               message_start,
               message_end,
               tokens: self.reader.commit_transaction(),
           })
    }
    fn read_warning_directive(&mut self) -> Result<directive::Warning> {
        // '('
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        let message_start = self.reader.position();
        let message_end = track_try!(self.skip_remaining_directive_tokens());

        Ok(directive::Warning {
               message_start,
               message_end,
               tokens: self.reader.commit_transaction(),
           })
    }
    fn read_parenthesized_macro_name(&mut self) -> Result<MacroName> {
        // '('
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        track_try!(self.reader.skip_whitespace_or_comment());
        let name = track_try!(self.read_macro_name());

        // ')'
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader
                       .read_expected_symbol_or_error(Symbol::CloseParen));

        // '.'
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader.read_expected_symbol_or_error(Symbol::Dot));

        Ok(name)
    }
    fn read_undef_directive(&mut self) -> Result<Undef> {
        let name = track_try!(self.read_parenthesized_macro_name());
        Ok(Undef {
               name,
               tokens: self.reader.commit_transaction(),
           })
    }
    fn read_define_directive(&mut self) -> Result<MacroDef> {
        // '('
        track_try!(self.reader.skip_whitespace_or_comment());
        track_try!(self.reader.read_expected_symbol_or_error(Symbol::OpenParen));

        // macro name
        track_try!(self.reader.skip_whitespace_or_comment());
        let name = track_try!(self.read_macro_name());

        // macro variables
        track_try!(self.reader.skip_whitespace_or_comment());
        let vars = match track_try!(self.reader.read_symbol_or_error()).value() {
            Symbol::Comma => None,
            Symbol::OpenParen => Some(track_try!(self.read_macro_vars())),
            s => {
                track_panic!(ErrorKind::InvalidInput,
                             "Unexpected symbol: actual={:?}, expected=Comma|OpenParent",
                             s)
            }
        };
        let replacement_start = self.reader.position();

        // macro replacement
        let replacement_end = track_try!(self.read_macro_replacement());

        Ok(MacroDef {
               name,
               vars,
               replacement_start,
               replacement_end,
               tokens: self.reader.commit_transaction(),
           })
    }
    fn read_macro_name(&mut self) -> Result<MacroName> {
        if let Some(atom) = track_try!(self.reader.read_atom()) {
            Ok(MacroName::Atom(atom))
        } else if let Some(var) = track_try!(self.reader.read_variable()) {
            Ok(MacroName::Variable(var))
        } else {
            track_panic!(ErrorKind::InvalidInput,
                         "Invalid macro name: {:?}",
                         self.reader.read());
        }
    }
    fn read_macro_vars(&mut self) -> Result<Vec<VariableToken>> {
        let mut vars = Vec::new();
        loop {
            track_try!(self.reader.skip_whitespace_or_comment());
            let var = track_try!(self.reader.read_variable_or_error());
            vars.push(var);

            track_try!(self.reader.skip_whitespace_or_comment());
            match track_try!(self.reader.read_symbol_or_error()).value() {
                Symbol::Comma => {}
                Symbol::CloseParen => break,
                s => {
                    track_panic!(ErrorKind::InvalidInput,
                                 "Unexpected symbol: actual={:?}, expected=Comma|CloneParent",
                                 s)
                }
            }
        }
        Ok(vars)
    }
    fn skip_remaining_directive_tokens(&mut self) -> Result<Position> {
        loop {
            let token = track_try!(self.reader.read_or_error());
            if token.value() == TokenValue::Symbol(Symbol::CloseParen) {
                let end = token.start_position().clone();
                track_try!(self.reader.skip_whitespace_or_comment());
                if track_try!(self.reader.read_symbol_if(Symbol::Dot)).is_some() {
                    return Ok(end);
                }
            }
        }
    }
    fn read_macro_replacement(&mut self) -> Result<Position> {
        track!(self.skip_remaining_directive_tokens())
    }
}
impl<'a> Iterator for Preprocessor<'a> {
    type Item = Result<Token>;
    fn next(&mut self) -> Option<Self::Item> {
        match self.next_token() {
            Err(e) => Some(Err(e)),
            Ok(None) => None,
            Ok(Some(token)) => Some(Ok(token)),
        }
    }
}
