use erl_tokenize::Token;
use erl_tokenize::tokens::{AtomToken, VariableToken};

#[derive(Debug, Clone)]
pub enum Directive {
    Include,
    IncludeLib,
    Define(MacroDef),
    Undef,
    Ifdef,
    Ifndef,
    Else,
    Endif,
    Error,
    Warning,
}

#[derive(Debug, Clone)]
pub struct MacroDef {
    pub name: AtomToken,
    pub vars: Option<Vec<VariableToken>>,
    pub tokens: Vec<Token>,
}
