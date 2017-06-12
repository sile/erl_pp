use std::hash::{Hash, Hasher};
use std::mem;
use erl_tokenize::{Token, Position, PositionRange};
use erl_tokenize::tokens::{AtomToken, VariableToken, SymbolToken};

#[derive(Debug, Clone)]
pub enum Directive {
    Include,
    IncludeLib,
    Define(MacroDef),
    Undef(Undef),
    Ifdef,
    Ifndef,
    Else,
    Endif,
    Error(Error),
    Warning(Warning),
}

#[derive(Debug, Clone)]
pub enum Directive2 {
    Include,
    IncludeLib,
    Define(Define),
    Undef(Undef),
    Ifdef,
    Ifndef,
    Else,
    Endif,
    Error(Error),
    Warning(Warning),
}

#[derive(Debug, Clone)]
pub struct Define {
    pub _hyphen: SymbolToken,
    pub _define: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub variables: Option<MacroVariables>,
    pub _comma: SymbolToken,
    pub replacement: Vec<Token>,
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

#[derive(Debug, Clone)]
pub enum List<T> {
    Null,
    Cons { head: T, tail: Tail<T> },
}

#[derive(Debug, Clone)]
pub enum Tail<T> {
    Null,
    Cons {
        _comma: SymbolToken,
        head: T,
        tail: Box<Tail<T>>,
    },
}

#[derive(Debug)]
pub struct ListIter<'a, T: 'a>(ListIterInner<'a, T>);
impl<'a, T: 'a> Iterator for ListIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

#[derive(Debug)]
enum ListIterInner<'a, T: 'a> {
    List(&'a List<T>),
    Tail(&'a Tail<T>),
    End,
}
impl<'a, T: 'a> Iterator for ListIterInner<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        match mem::replace(self, ListIterInner::End) {
            ListIterInner::List(&List::Null) => None,
            ListIterInner::List(&List::Cons { ref head, ref tail }) => {
                *self = ListIterInner::Tail(tail);
                Some(head)
            }
            ListIterInner::Tail(&Tail::Null) => None,
            ListIterInner::Tail(&Tail::Cons { ref head, ref tail, .. }) => {
                *self = ListIterInner::Tail(tail);
                Some(head)
            }
            ListIterInner::End => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MacroVariables {
    pub _open_paren: SymbolToken,
    pub list: List<VariableToken>,
    pub _close_paren: SymbolToken,
}
impl MacroVariables {
    pub fn iter(&self) -> ListIter<VariableToken> {
        ListIter(ListIterInner::List(&self.list))
    }
}
impl PositionRange for MacroVariables {
    fn start_position(&self) -> Position {
        self._open_paren.start_position()
    }
    fn end_position(&self) -> Position {
        self._close_paren.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Error {
    pub message_start: Position,
    pub message_end: Position,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub message_start: Position,
    pub message_end: Position,
    pub tokens: Vec<Token>,
}

#[derive(Debug, Clone)]
pub struct Undef {
    pub name: MacroName,
    pub tokens: Vec<Token>,
}

// TODO: rename
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
    pub fn value(&self) -> &str {
        match *self {
            MacroName::Atom(ref t) => t.value(),
            MacroName::Variable(ref t) => t.value(),
        }
    }
    pub fn text(&self) -> &str {
        match *self {
            MacroName::Atom(ref t) => t.text(),
            MacroName::Variable(ref t) => t.text(),
        }
    }
}
impl PartialEq for MacroName {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}
impl Eq for MacroName {}
impl Hash for MacroName {
    fn hash<H: Hasher>(&self, hasher: &mut H) {
        self.value().hash(hasher);
    }
}
impl PositionRange for MacroName {
    fn start_position(&self) -> Position {
        match *self {
            MacroName::Atom(ref t) => t.start_position(),
            MacroName::Variable(ref t) => t.start_position(),
        }
    }
    fn end_position(&self) -> Position {
        match *self {
            MacroName::Atom(ref t) => t.end_position(),
            MacroName::Variable(ref t) => t.end_position(),
        }
    }
}
