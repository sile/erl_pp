use std::hash::{Hash, Hasher};
use erl_tokenize::{Token, Position};
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
    pub name: MacroName,
    pub vars: Option<Vec<VariableToken>>,
    pub replacement_start: Position,
    pub replacement_end: Position,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub enum MacroName {
    Atom(AtomToken),
    Variable(VariableToken),
}
impl MacroName {
    pub fn as_str(&self) -> &str {
        match *self {
            MacroName::Atom(ref t) => t.value(),
            MacroName::Variable(ref t) => t.value(),
        }
    }
}
impl PartialEq for MacroName {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for MacroName {}
impl Hash for MacroName {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.as_str().hash(hasher);
    }
}
