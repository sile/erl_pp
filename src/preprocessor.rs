use erl_tokenize::{Token, Position};

use Result;

#[derive(Debug)]
pub struct Preprocessor<I> {
    tokens: I,
}
impl<I> Preprocessor<I>
    where I: Iterator<Item = Result<(Token, Position)>>
{
    pub fn new(tokens: I) -> Self {
        Preprocessor { tokens }
    }
}
impl<I> Iterator for Preprocessor<I>
    where I: Iterator<Item = Result<(Token, Position)>>
{
    type Item = Result<(Token, Position)>;
    fn next(&mut self) -> Option<Self::Item> {
        unimplemented!()
    }
}
