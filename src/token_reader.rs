use erl_tokenize::{Token, Tokenizer};
use erl_tokenize::tokens::{AtomToken, SymbolToken};
use erl_tokenize::values::Symbol;

use Result;

#[derive(Debug)]
pub struct TokenReader<'a> {
    tokens: Tokenizer<'a>,
    unread: Vec<Token>,
    transaction: Option<Vec<Token>>,
}
impl<'a> TokenReader<'a> {
    pub fn new(tokens: Tokenizer<'a>) -> Self {
        TokenReader {
            tokens,
            unread: Vec::new(),
            transaction: None,
        }
    }
    pub fn start_transaction(&mut self) {
        assert!(self.transaction.is_none());
        self.transaction = Some(Vec::new());
    }
    pub fn commit_transaction(&mut self) -> Vec<Token> {
        self.transaction.take().expect("No ongoing transaction")
    }
    pub fn abort_transaction(&mut self) {
        let mut tokens = self.transaction.take().expect("No ongoing transaction");
        tokens.reverse();
        self.unread.extend(tokens);
    }

    pub fn read(&mut self) -> Result<Option<Token>> {
        if let Some(token) = self.unread.pop() {
            if let Some(transaction) = self.transaction.as_mut() {
                transaction.push(token.clone());
            }
            Ok(Some(token))
        } else {
            match self.tokens.next() {
                None => Ok(None),
                Some(Err(e)) => Err(e),
                Some(Ok(t)) => Ok(Some(t)),
            }
        }
    }
    pub fn unread(&mut self, token: Token) {
        if let Some(transaction) = self.transaction.as_mut() {
            transaction.pop();
        }
        self.unread.push(token);
    }
    pub fn skip_whitespace_or_comment(&mut self) -> Result<()> {
        while let Some(token) = track_try!(self.read()) {
            match token {
                Token::Whitespace(_) |
                Token::Comment(_) => {}
                _ => {
                    self.unread(token);
                    break;
                }
            }
        }
        Ok(())
    }
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
}
