use erl_tokenize::tokens::{AtomToken, IntegerToken, StringToken, VariableToken};
use erl_tokenize::values::Symbol;
use erl_tokenize::{self, LexicalToken, Position, PositionRange};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

use crate::macros::Stringify;
use crate::token_reader::TokenReader;
use crate::types::{MacroArgs, MacroVariables};
use crate::{Directive, Error, MacroCall, MacroDef, Result};

/// Erlang source code [preprocessor][Preprocessor].
///
/// This is an iterator which preprocesses given tokens and iterates on the resulting tokens.
///
/// The resulting tokens contains no macro directives and
/// all macro calls in the input tokens are expanded.
///
/// [Preprocessor]: http://erlang.org/doc/reference_manual/macros.html
///
/// # Examples
///
/// ```
/// # extern crate erl_pp;
/// # extern crate erl_tokenize;
/// use erl_pp::Preprocessor;
/// use erl_tokenize::Lexer;
///
/// # fn main() {
/// let src = r#"-define(FOO(A), [A, A]). -define(BAR, ?LINE). ?FOO(?BAR)."#;
/// let pp = Preprocessor::new(Lexer::new(src));
/// let tokens = pp.collect::<Result<Vec<_>, _>>().unwrap();
///
/// assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
///            ["[", "1", ",", "1", "]", "."]);
/// # }
/// ```
#[derive(Debug)]
pub struct Preprocessor<T> {
    reader: TokenReader<T>,
    can_directive_start: bool,
    directives: BTreeMap<Position, Directive>,
    code_paths: VecDeque<PathBuf>,
    branches: Vec<Branch>,
    macros: HashMap<String, MacroDef>,
    macro_calls: BTreeMap<Position, MacroCall>,
    expanded_tokens: VecDeque<LexicalToken>,
}
impl<T> Preprocessor<T>
where
    T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
{
    /// Makes a new `Preprocessor` instance.
    pub fn new(tokens: T) -> Self {
        Preprocessor {
            reader: TokenReader::new(tokens),
            can_directive_start: true,
            directives: BTreeMap::new(),
            code_paths: VecDeque::new(),
            branches: Vec::new(),
            macros: HashMap::new(),
            macro_calls: BTreeMap::new(),
            expanded_tokens: VecDeque::new(),
        }
    }

    fn ignore(&self) -> bool {
        self.branches.iter().any(|b| !b.entered)
    }
    fn next_token(&mut self) -> Result<Option<LexicalToken>> {
        loop {
            if let Some(token) = self.expanded_tokens.pop_front() {
                return Ok(Some(token));
            }
            if self.can_directive_start {
                if let Some(d) = self.try_read_directive()? {
                    self.directives.insert(d.start_position(), d);
                    continue;
                }
            }
            if !self.ignore() {
                if let Some(m) = self.reader.try_read_macro_call(&self.macros)? {
                    self.macro_calls.insert(m.start_position(), m.clone());
                    self.expanded_tokens = self.expand_macro(m)?;
                    continue;
                }
            }
            if let Some(token) = self.reader.try_read_token()? {
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
        if let Some(expanded) = self.try_expand_predefined_macro(&call)? {
            Ok(vec![expanded].into())
        } else {
            self.expand_userdefined_macro(call)
        }
    }
    fn try_expand_predefined_macro(&self, call: &MacroCall) -> Result<Option<LexicalToken>> {
        let expanded = match call.name.value() {
            "FILE" => {
                let current = call.start_position();
                let file = current
                    .filepath()
                    .and_then(|f| f.to_str())
                    .ok_or_else(|| Error::file_not_set(call.clone()))?;
                StringToken::from_value(file, call.start_position()).into()
            }
            "LINE" => {
                let line = call.start_position().line();
                IntegerToken::from_value(line.into(), call.start_position()).into()
            }
            "MACHINE" => AtomToken::from_value("BEAM", call.start_position()).into(),
            _ => return Ok(None),
        };
        Ok(Some(expanded))
    }
    fn expand_userdefined_macro(&self, call: MacroCall) -> Result<VecDeque<LexicalToken>> {
        let definition = self
            .macros
            .get(call.name.value())
            .ok_or_else(|| Error::undefined_macro(call.clone()))?;
        match *definition {
            MacroDef::Dynamic(ref replacement) => Ok(replacement.clone().into()),
            MacroDef::Static(ref definition) => {
                if call.args.as_ref().map(MacroArgs::len)
                    != definition.variables.as_ref().map(MacroVariables::len)
                {
                    return Err(Error::macro_args_mismatched(
                        call.clone(),
                        MacroDef::Static(definition.clone()),
                    ));
                }
                let bindings = definition
                    .variables
                    .as_ref()
                    .iter()
                    .flat_map(|i| i.iter().map(VariableToken::value))
                    .zip(
                        call.args
                            .iter()
                            .flat_map(|i| i.iter().map(|a| &a.tokens[..])),
                    )
                    .collect::<HashMap<_, _>>();
                let expanded = self.expand_replacement(bindings, &definition.replacement)?;
                Ok(expanded)
            }
        }
    }
    fn expand_replacement(
        &self,
        bindings: HashMap<&str, &[LexicalToken]>,
        replacement: &[LexicalToken],
    ) -> Result<VecDeque<LexicalToken>> {
        let mut expanded = VecDeque::new();
        let mut reader: TokenReader<_> =
            TokenReader::new(replacement.iter().map(|t| Ok(t.clone())));
        loop {
            if let Some(call) = reader.try_read_macro_call(&self.macros)? {
                let nested = self.expand_macro(call)?;
                for token in nested.into_iter().rev() {
                    reader.unread_token(token);
                }
            } else if let Some(stringify) = reader.try_read::<Stringify>()? {
                let tokens = bindings
                    .get(stringify.name.value())
                    .ok_or_else(|| Error::undefined_macro_var(stringify.name.value().to_owned()))?;
                let string = tokens.iter().map(LexicalToken::text).collect::<String>();
                let token = StringToken::from_value(&string, tokens[0].start_position());
                expanded.push_back(token.into());
            } else if let Some(token) = reader.try_read_token()? {
                if let Some(value) = token
                    .as_variable_token()
                    .and_then(|v| bindings.get(v.value()))
                {
                    let nested = self.expand_replacement(HashMap::new(), value)?;
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
    fn try_read_directive(&mut self) -> Result<Option<Directive>> {
        let directive: Directive = if let Some(directive) = self.reader.try_read()? {
            directive
        } else {
            return Ok(None);
        };

        let ignore = self.ignore();
        match directive {
            Directive::Include(ref d) if !ignore => {
                let (path, text) = d.include()?;
                self.reader.add_included_text(path, text);
            }
            Directive::IncludeLib(ref d) if !ignore => {
                let (path, text) = d.include_lib(&self.code_paths)?;
                self.reader.add_included_text(path, text);
            }
            Directive::Define(ref d) if !ignore => {
                self.macros
                    .insert(d.name.value().to_string(), MacroDef::Static(d.clone()));
            }
            Directive::Undef(ref d) if !ignore => {
                self.macros.remove(d.name.value());
            }
            Directive::Ifdef(ref d) => {
                let entered = self.macros.contains_key(d.name.value());
                self.branches.push(Branch::new(entered));
            }
            Directive::Ifndef(ref d) => {
                let entered = !self.macros.contains_key(d.name.value());
                self.branches.push(Branch::new(entered));
            }
            Directive::Else(_) => {
                let b = self
                    .branches
                    .last_mut()
                    .ok_or_else(|| Error::missing_if_directive(directive.clone()))?;
                if !b.switch_to_else_branch() {
                    return Err(Error::missing_if_directive(directive));
                }
            }
            Directive::Endif(_) => {
                if self.branches.pop().is_none() {
                    return Err(Error::missing_if_directive(directive));
                }
            }
            _ => {}
        }
        Ok(Some(directive))
    }
}
impl<T> Preprocessor<T> {
    /// Returns a reference to the code path list which
    /// will be used by this preprocessor for handling `include_lib` directive.
    pub fn code_paths(&self) -> &VecDeque<PathBuf> {
        &self.code_paths
    }

    /// Returns a mutable reference to the code path list which
    /// will be used by this preprocessor for handling `include_lib` directive.
    pub fn code_paths_mut(&mut self) -> &mut VecDeque<PathBuf> {
        &mut self.code_paths
    }

    /// Returns a reference to the map containing the macro directives
    /// encountered by this preprocessor so far.
    ///
    /// The keys of this map are starting positions of the corresponding directives.
    pub fn directives(&self) -> &BTreeMap<Position, Directive> {
        &self.directives
    }

    /// Returns a reference to the map containing the macro calls
    /// encountered by this preprocessor so far.
    ///
    /// The keys of this map are starting positions of the corresponding macro calls.
    ///
    /// Note this map only contains top level macro calls.
    /// Macro calls that occurred during expansion of other macros are excluded.
    pub fn macro_calls(&self) -> &BTreeMap<Position, MacroCall> {
        &self.macro_calls
    }

    /// Returns a reference to the map containing the current macro definitions.
    pub fn macros(&self) -> &HashMap<String, MacroDef> {
        &self.macros
    }

    /// Returns a mutable reference to the map containing the current macro definitions.
    pub fn macros_mut(&mut self) -> &mut HashMap<String, MacroDef> {
        &mut self.macros
    }
}
impl<T> Iterator for Preprocessor<T>
where
    T: Iterator<Item = erl_tokenize::Result<LexicalToken>>,
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

#[derive(Debug)]
struct Branch {
    pub then_branch: bool,
    pub entered: bool,
}
impl Branch {
    pub fn new(entered: bool) -> Self {
        Branch {
            then_branch: true,
            entered,
        }
    }
    pub fn switch_to_else_branch(&mut self) -> bool {
        if self.then_branch {
            self.then_branch = false;
            self.entered = !self.entered;
            true
        } else {
            false
        }
    }
}
