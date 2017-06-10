use std::collections::HashMap;
use std::path::PathBuf;
use erl_tokenize::{Token, Tokenizer, Position};
use erl_tokenize::values::Symbol;

use {Result, Directive};
use token_reader::TokenReader;

type MacroName = String;

#[derive(Debug)]
pub struct Preprocessor<'a> {
    reader: TokenReader<'a>,
    can_directive_start: bool,
    macros: HashMap<MacroName, Position>,
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
                panic!();
            }
            self.reader.abort_transaction();
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
                "define" => unimplemented!(),
                "undef" => unimplemented!(),
                "ifdef" => unimplemented!(),
                "ifndef" => unimplemented!(),
                "else" => unimplemented!(),
                "endif" => unimplemented!(),
                "error" => unimplemented!(),
                "warning" => unimplemented!(),
                _ => {}
            }
        }

        Ok(None)
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
