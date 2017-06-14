use std::collections::{HashMap, BTreeMap, VecDeque};
use std::path::PathBuf;
use erl_tokenize::{self, LexicalToken, Position, PositionRange};
use erl_tokenize::tokens::StringToken;
use erl_tokenize::values::Symbol;

use {Result, Error, Directive, ErrorKind, MacroCall, PredefinedMacros};
use directives::Define;
use macros::Stringify;
use token_reader::TokenReader;
use types::MacroName;

#[derive(Debug)]
pub struct Preprocessor<T, E = erl_tokenize::Error> {
    reader: TokenReader<T, E>,
    can_directive_start: bool,
    directives: BTreeMap<Position, Directive>,
    code_paths: Vec<PathBuf>,
    branches: Vec<bool>,
    predefined_macros: PredefinedMacros,
    macros: HashMap<MacroName, Define>,
    macro_calls: BTreeMap<Position, MacroCall>,
    expanded_tokens: VecDeque<LexicalToken>,
}
impl<T, E> Preprocessor<T, E>
    where T: Iterator<Item = ::std::result::Result<LexicalToken, E>>,
          E: Into<Error>
{
    pub fn new(tokens: T) -> Self {
        Preprocessor {
            reader: TokenReader::new(tokens),
            can_directive_start: true,
            directives: BTreeMap::new(),
            code_paths: Vec::new(),
            branches: Vec::new(),
            predefined_macros: PredefinedMacros::new(),
            macros: HashMap::new(),
            macro_calls: BTreeMap::new(),
            expanded_tokens: VecDeque::new(),
        }
    }
    pub fn predefined_macros(&self) -> &PredefinedMacros {
        &self.predefined_macros
    }
    pub fn predefined_macros_mut(&mut self) -> &mut PredefinedMacros {
        &mut self.predefined_macros
    }
    pub fn directives(&self) -> &BTreeMap<Position, Directive> {
        &self.directives
    }
    pub fn macro_calls(&self) -> &BTreeMap<Position, MacroCall> {
        &self.macro_calls
    }

    fn read(&mut self) -> Result<Option<LexicalToken>> {
        track!(self.reader.try_read_token())
    }
    fn ignore(&self) -> bool {
        self.branches.iter().find(|b| **b == false).is_some()
    }
    fn next_token(&mut self) -> Result<Option<LexicalToken>> {
        loop {
            if let Some(token) = self.expanded_tokens.pop_front() {
                return Ok(Some(token));
            }
            if self.can_directive_start {
                if let Some(d) = track!(self.try_read_directive())? {
                    self.directives.insert(d.start_position(), d);
                    continue;
                }
            }
            if !self.ignore() {
                if let Some(m) = track!(self.try_read_macro_call())? {
                    self.macro_calls.insert(m.start_position(), m.clone());
                    self.expanded_tokens = track!(self.expand_macro(m))?;
                    continue;
                }
            }
            if let Some(token) = track!(self.read())? {
                if self.ignore() {
                    continue;
                }
                self.can_directive_start = token
                    .as_symbol_token()
                    .map_or(false, |s| s.value() == Symbol::Dot);
                return Ok(Some(token));
            } else {
                break;
            }
        }
        Ok(None)
    }
    fn expand_macro(&self, call: MacroCall) -> Result<VecDeque<LexicalToken>> {
        if let Some(expanded) = track!(self.predefined_macros.try_expand(&call))? {
            Ok(vec![expanded].into())
        } else {
            track!(self.expand_userdefined_macro(call))
        }
    }
    fn expand_userdefined_macro(&self, call: MacroCall) -> Result<VecDeque<LexicalToken>> {
        let definition = track!(self.macros.get(&call.name).ok_or(Error::invalid_input()))?;
        track_assert_eq!(call.args.as_ref().map(|a| a.len()),
                         definition.variables.as_ref().map(|v| v.len()),
                         ErrorKind::InvalidInput);
        let bindings = definition
            .variables
            .as_ref()
            .iter()
            .flat_map(|i| i.iter().map(|v| v.value()))
            .zip(call.args
                     .iter()
                     .flat_map(|i| i.iter().map(|a| &a.tokens[..])))
            .collect::<HashMap<_, _>>();
        let expanded = track!(self.expand_replacement(bindings, &definition.replacement))?;
        Ok(expanded)
    }
    fn expand_replacement(&self,
                          bindings: HashMap<&str, &[LexicalToken]>,
                          replacement: &[LexicalToken])
                          -> Result<VecDeque<LexicalToken>> {
        let mut expanded = VecDeque::new();
        let mut reader: TokenReader<_, Error> =
            TokenReader::new(replacement.iter().map(|t| Ok(t.clone())));
        loop {
            if let Some(call) = track!(reader.try_read())? {
                let nested = track!(self.expand_macro(call))?;
                for token in nested.into_iter().rev() {
                    reader.unread_token(token);
                }
            } else if let Some(stringify) = track!(reader.try_read::<Stringify>())? {
                let tokens = track!(bindings
                                        .get(stringify.name.value())
                                        .ok_or(Error::invalid_input()))?;
                let string = tokens.iter().map(|t| t.text()).collect::<String>();
                let token = StringToken::from_value(&string, tokens[0].start_position());
                expanded.push_back(token.into());
            } else if let Some(token) = track!(reader.try_read_token())? {
                if let Some(value) = token
                       .as_variable_token()
                       .and_then(|v| bindings.get(v.value())) {
                    let nested = track!(self.expand_replacement(HashMap::new(), value))?;
                    expanded.extend(nested);
                } else {
                    expanded.push_back(token);
                }
            } else {
                break;
            }
        }
        Ok(expanded)
    }
    fn try_read_macro_call(&mut self) -> Result<Option<MacroCall>> {
        track!(self.reader.try_read())
    }
    fn try_read_directive(&mut self) -> Result<Option<Directive>> {
        let directive: Directive = if let Some(directive) = track!(self.reader.try_read())? {
            directive
        } else {
            return Ok(None);
        };

        let ignore = self.ignore();
        match directive {
            Directive::Include(ref d) if !ignore => {
                let (path, text) = track!(d.include())?;
                self.reader.push_text(path, text);
            }
            Directive::IncludeLib(ref d) if !ignore => {
                let (path, text) = track!(d.include_lib(&self.code_paths))?;
                self.reader.push_text(path, text);
            }
            Directive::Define(ref d) if !ignore => {
                self.macros.insert(d.name.clone(), d.clone());
            }
            Directive::Undef(ref d) if !ignore => {
                self.macros.remove(&d.name);
            }
            Directive::Ifdef(ref d) => {
                let entered = self.macros.contains_key(&d.name);
                self.branches.push(entered);
            }
            Directive::Ifndef(ref d) => {
                let entered = !self.macros.contains_key(&d.name);
                self.branches.push(entered);
            }
            Directive::Else(_) => {
                // TODO: 連続elseチェック
                let b = track!(self.branches.last_mut().ok_or(::Error::invalid_input()))?;
                *b = !*b;
            }
            Directive::Endif(_) => {
                track_assert!(self.branches.pop().is_some(), ErrorKind::InvalidInput);
            }
            _ => {}
        }
        Ok(Some(directive))
    }
}
impl<T, E> Iterator for Preprocessor<T, E>
    where T: Iterator<Item = ::std::result::Result<LexicalToken, E>>,
          E: Into<Error>
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
