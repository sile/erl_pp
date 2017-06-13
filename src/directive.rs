use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::mem;
use std::path::{Path, PathBuf};
use erl_tokenize::{LexicalToken, Position, PositionRange};
use erl_tokenize::tokens::{AtomToken, VariableToken, SymbolToken, StringToken};
use glob::glob;
use trackable::error::ErrorKindExt;

use {Result, ErrorKind};

#[derive(Debug, Clone)]
pub enum Directive {
    Include(Include),
    IncludeLib(IncludeLib),
    Define(Define),
    Undef(Undef),
    Ifdef(Ifdef),
    Ifndef(Ifndef),
    Else(Else),
    Endif(Endif),
    Error(Error),
    Warning(Warning),
}

fn substitute_path_variables<P: AsRef<Path>>(path: P) -> Result<PathBuf> {
    let mut new = PathBuf::new();
    for c in path.as_ref().components() {
        if let Some(s) = c.as_os_str().to_str() {
            if s.as_bytes().get(0) == Some(&b'$') {
                let c = track_try!(env::var(s.split_at(1).1)
                                       .map_err(|e| ErrorKind::InvalidInput.cause(e)));
                new.push(c);
                continue;
            }
        }
        new.push(c.as_os_str());
    }
    Ok(new)
}

#[derive(Debug, Clone)]
pub struct Include {
    pub _hyphen: SymbolToken,
    pub _include: AtomToken,
    pub _open_paren: SymbolToken,
    pub path: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl Include {
    pub fn include(&self) -> Result<(PathBuf, String)> {
        let path = track_try!(substitute_path_variables(self.path.value()));
        let mut text = String::new();
        let mut file = track_try!(File::open(&path).map_err(|e| ErrorKind::InvalidInput.cause(e)),
                                  "path={:?}",
                                  path);
        track_try!(file.read_to_string(&mut text)
                       .map_err(|e| ErrorKind::InvalidInput.cause(e)));
        Ok((path, text))
    }
}
impl PositionRange for Include {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct IncludeLib {
    pub _hyphen: SymbolToken,
    pub _include_lib: AtomToken,
    pub _open_paren: SymbolToken,
    pub path: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl IncludeLib {
    pub fn include_lib(&self, code_paths: &[PathBuf]) -> Result<(PathBuf, String)> {
        use std::path::Component;
        let mut path = track_try!(substitute_path_variables(self.path.value()));
        let temp_path = path.clone();
        let mut components = temp_path.components();
        if let Some(Component::Normal(app_name)) = components.next() {
            let app_name = track_try!(app_name.to_str().ok_or(ErrorKind::InvalidInput));
            let pattern = format!("{}-*", app_name);
            'root: for root in code_paths.iter() {
                for entry in track_try!(glob(root.join(&pattern).to_str().expect("TODO"))
                                            .map_err(|e| ErrorKind::InvalidInput.cause(e))) {
                    path = track_try!(entry.map_err(|e| ErrorKind::InvalidInput.cause(e)));
                    for c in components {
                        path.push(c.as_os_str());
                    }
                    break 'root;
                }
            }
        }

        let mut text = String::new();
        let mut file = track_try!(File::open(&path).map_err(|e| ErrorKind::InvalidInput.cause(e)),
                                  "path={:?}",
                                  path);
        track_try!(file.read_to_string(&mut text)
                       .map_err(|e| ErrorKind::InvalidInput.cause(e)));
        Ok((path, text))
    }
}
impl PositionRange for IncludeLib {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Define {
    pub _hyphen: SymbolToken,
    pub _define: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub variables: Option<MacroVariables>,
    pub _comma: SymbolToken,
    pub replacement: Vec<LexicalToken>,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl Define {
    pub fn expand(&self, args: Vec<&[LexicalToken]>) -> Result<Vec<LexicalToken>> {
        assert!(self.variables.is_some());
        let vars = self.variables.as_ref().unwrap();
        let binds: HashMap<_, _> = vars.iter().map(|v| v.value()).zip(args.iter()).collect();

        let mut tokens = Vec::new();
        let mut template = self.replacement.iter();
        while let Some(t) = template.next() {
            use erl_tokenize::values::Symbol;

            if let Some(val) = binds.get(t.text()) {
                tokens.extend(val.iter().cloned());
            } else if t.as_symbol_token().map(|t| t.value()) == Some(Symbol::DoubleQuestion) {
                let var = track_try!(template.next().ok_or(ErrorKind::InvalidInput));
                let val = track_try!(binds.get(var.text()).ok_or(ErrorKind::InvalidInput));
                let text = val.iter().map(|t| t.text()).collect::<String>();
                tokens.push(track_try!(StringToken::from_text(&format!("{:?}", text),
                                                              val.first()
                                                                  .unwrap()
                                                                  .start_position()))
                                .into());
            } else {
                tokens.push(t.clone());
            }
        }
        Ok(tokens)
    }
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
pub struct Undef {
    pub _hyphen: SymbolToken,
    pub _undef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Undef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Ifdef {
    pub _hyphen: SymbolToken,
    pub _ifdef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Ifdef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Ifndef {
    pub _hyphen: SymbolToken,
    pub _ifndef: AtomToken,
    pub _open_paren: SymbolToken,
    pub name: MacroName,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Ifndef {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Endif {
    pub _hyphen: SymbolToken,
    pub _endif: AtomToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Endif {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Else {
    pub _hyphen: SymbolToken,
    pub _else: AtomToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Else {
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
impl<T> List<T> {
    pub fn iter(&self) -> ListIter<T> {
        ListIter(ListIterInner::List(self))
    }
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
        self.list.iter()
    }
    pub fn len(&self) -> usize {
        self.list.iter().count()
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
    pub _hyphen: SymbolToken,
    pub _error: AtomToken,
    pub _open_paren: SymbolToken,
    pub message: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Error {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub _hyphen: SymbolToken,
    pub _warning: AtomToken,
    pub _open_paren: SymbolToken,
    pub message: StringToken,
    pub _close_paren: SymbolToken,
    pub _dot: SymbolToken,
}
impl PositionRange for Warning {
    fn start_position(&self) -> Position {
        self._hyphen.start_position()
    }
    fn end_position(&self) -> Position {
        self._dot.end_position()
    }
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
