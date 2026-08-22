//! OTP predefined macro expansion and `-if` / `-elif` condition evaluation
//! for the `check_otp` example.
//!
//! `erl_pp` expands `?FILE` / `?LINE` itself. Everything else
//! (`?MODULE`, `?OTP_RELEASE`, …) arrives as `AwaitingMacroExpansion`.

use std::collections::HashSet;

/// OTP 29 `erl_features:all/0` names that `?FEATURE_AVAILABLE` reports.
const AVAILABLE_FEATURES: &[&str] = &["maybe_expr", "compr_assign"];

/// Feature names enabled by default for OTP 29 (`erl_features:approved/0`).
const DEFAULT_ENABLED_FEATURES: &[&str] = &["maybe_expr"];

/// Lexical environment used to expand OTP predefined macros.
pub struct PredefContext {
    otp_release: Option<u32>,
    module: Option<String>,
    function_name: Option<String>,
    function_arity: Option<usize>,
    enabled_features: HashSet<String>,
    scan: FormScan,
}

#[derive(Default)]
struct FormScan {
    nest: usize,
    at_form_start: bool,
    hash: HashState,
    after_fun: bool,
    after_fun_name: bool,
    attr: AttrScan,
    fun: FunScan,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum HashState {
    #[default]
    Off,
    SawHash,
    SawHashName,
    SawWildcard,
}

#[derive(Default)]
enum AttrScan {
    #[default]
    Off,
    AfterHyphen,
    ModuleWaitParen,
    ModuleWaitName,
    FeatureWaitParen,
    FeatureWaitName,
    FeatureWaitComma {
        name: String,
    },
    FeatureWaitAction {
        name: String,
    },
    Other,
}

#[derive(Default)]
enum FunScan {
    #[default]
    Off,
    WaitParen {
        name: String,
    },
    InArity {
        name: String,
        depth: usize,
        items: usize,
        nonempty: bool,
    },
    Body {
        name: String,
        arity: usize,
    },
}

impl PredefContext {
    pub fn new(otp_release: Option<u32>) -> Self {
        Self {
            otp_release,
            module: None,
            function_name: None,
            function_arity: None,
            enabled_features: DEFAULT_ENABLED_FEATURES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            scan: FormScan {
                at_form_start: true,
                ..FormScan::default()
            },
        }
    }

    pub fn on_token(&mut self, token: &erl_pp::SourceToken) {
        self.scan
            .feed(token, &mut self.module, &mut self.enabled_features);
        match &self.scan.fun {
            FunScan::WaitParen { name } | FunScan::InArity { name, .. } => {
                self.function_name = Some(name.clone());
                self.function_arity = None;
            }
            FunScan::Body { name, arity } => {
                self.function_name = Some(name.clone());
                self.function_arity = Some(*arity);
            }
            FunScan::Off => {
                self.function_name = None;
                self.function_arity = None;
            }
        }
    }

    pub fn ifdef_defined(&self, name: &str) -> Option<bool> {
        match name {
            "FILE" | "LINE" | "MACHINE" | "OTP_RELEASE" | "FEATURE_AVAILABLE"
            | "FEATURE_ENABLED" => Some(true),
            "MODULE" | "MODULE_STRING" => Some(self.module.is_some()),
            "FUNCTION_NAME" | "FUNCTION_ARITY" => Some(self.function_name.is_some()),
            _ => None,
        }
    }

    pub fn if_branch(
        &self,
        tokens: &[erl_pp::SourceToken],
        macros: &erl_pp::MacroTable,
    ) -> Option<erl_pp::Branch> {
        evaluate_condition(tokens, macros).map(|v| {
            if v {
                erl_pp::Branch::Then
            } else {
                erl_pp::Branch::Else
            }
        })
    }

    pub fn expansion_text(&self, call: &erl_pp::MacroCall) -> Result<String, String> {
        let name = call.name.as_str();
        match (name, call.arity) {
            ("MODULE", None) => self
                .module
                .as_deref()
                .map(emit_atom)
                .ok_or_else(|| "?MODULE used before -module".to_string()),
            ("MODULE_STRING", None) => self
                .module
                .as_deref()
                .map(emit_string)
                .ok_or_else(|| "?MODULE_STRING used before -module".to_string()),
            ("MACHINE", None) => Ok("BEAM".to_string()),
            ("OTP_RELEASE", None) => self
                .otp_release
                .map(|n| n.to_string())
                .ok_or_else(|| "?OTP_RELEASE needs OTP_TAG".to_string()),
            ("FUNCTION_NAME", None) => self
                .function_name
                .as_deref()
                .map(emit_atom)
                .ok_or_else(|| "?FUNCTION_NAME used outside a function".to_string()),
            ("FUNCTION_ARITY", None) => self
                .function_arity
                .map(|n| n.to_string())
                .ok_or_else(|| "?FUNCTION_ARITY used outside a function".to_string()),
            ("FEATURE_AVAILABLE", Some(1)) => {
                let feat = feature_arg(call)?;
                Ok(emit_bool(AVAILABLE_FEATURES.contains(&feat.as_str())))
            }
            ("FEATURE_ENABLED", Some(1)) => {
                let feat = feature_arg(call)?;
                Ok(emit_bool(self.enabled_features.contains(&feat)))
            }
            _ => Err(format!(
                "unknown macro ?{name}{}",
                match call.arity {
                    None => String::new(),
                    Some(n) => format!("/{n}"),
                }
            )),
        }
    }
}

fn feature_arg(call: &erl_pp::MacroCall) -> Result<String, String> {
    let tokens = call
        .arguments
        .first()
        .ok_or_else(|| "feature macro is missing its argument".to_string())?;
    for t in tokens {
        if !t.token().kind().is_lexical() {
            continue;
        }
        return match t.value() {
            erl_tokenize::TokenValue::Atom(a) => Ok(a.into_owned()),
            _ => Err("feature macro argument is not an atom".to_string()),
        };
    }
    Err("feature macro argument is empty".to_string())
}

impl FormScan {
    fn feed(
        &mut self,
        token: &erl_pp::SourceToken,
        module: &mut Option<String>,
        enabled: &mut HashSet<String>,
    ) {
        let kind = token.token().kind();
        if self.after_fun {
            self.after_fun = false;
            match kind {
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenParen) => {
                    self.open_group();
                    return;
                }
                erl_tokenize::TokenKind::Atom | erl_tokenize::TokenKind::Variable => {
                    self.after_fun_name = true;
                    return;
                }
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Colon) => {
                    self.after_fun_name = true;
                    return;
                }
                _ => {}
            }
        }
        if self.after_fun_name {
            self.after_fun_name = false;
            match kind {
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Slash) => return,
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenParen) => {
                    self.open_group();
                    return;
                }
                erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Colon) => {
                    self.after_fun_name = true;
                    return;
                }
                erl_tokenize::TokenKind::Atom | erl_tokenize::TokenKind::Variable => {
                    self.after_fun_name = true;
                    return;
                }
                _ => {}
            }
        }

        match kind {
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenParen)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenSquare)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenBrace)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::DoubleLeftAngle) => {
                let opens_fun_args =
                    matches!(&self.fun, FunScan::WaitParen { .. }) && self.nest == 0;
                self.open_group();
                if !opens_fun_args {
                    self.on_arity_token(true);
                }
                self.hash = HashState::Off;
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::CloseParen)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::CloseSquare)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::CloseBrace)
            | erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::DoubleRightAngle) => {
                self.close_group();
                self.finish_arity_if_closed();
                self.hash = HashState::Off;
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Comma) => {
                self.on_arity_comma();
                self.hash = HashState::Off;
                self.advance_attr_comma();
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Hyphen)
                if self.at_form_start && self.nest == 0 =>
            {
                self.at_form_start = false;
                self.attr = AttrScan::AfterHyphen;
                self.hash = HashState::Off;
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Sharp) => {
                self.hash = HashState::SawHash;
                self.on_arity_token(true);
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::WildcardRecord) => {
                self.hash = HashState::SawWildcard;
                self.on_arity_token(true);
            }
            erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Dot) => {
                if self.is_record_field_dot() {
                    self.hash = HashState::Off;
                    self.on_arity_token(true);
                } else if self.nest == 0 {
                    self.end_form();
                } else {
                    self.hash = HashState::Off;
                }
            }
            erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Begin)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Case)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::If)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Receive)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Try)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Maybe)
            | erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Cond) => {
                self.nest += 1;
                self.hash = HashState::Off;
                self.on_arity_token(true);
            }
            erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Fun) => {
                self.after_fun = true;
                self.hash = HashState::Off;
                self.on_arity_token(true);
            }
            erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::End) => {
                self.nest = self.nest.saturating_sub(1);
                self.hash = HashState::Off;
            }
            erl_tokenize::TokenKind::Atom
                if let erl_tokenize::TokenValue::Atom(a) = token.value() =>
            {
                self.on_atom(a.as_ref(), module, enabled);
            }
            erl_tokenize::TokenKind::Variable => {
                if self.hash == HashState::SawHash {
                    self.hash = HashState::SawHashName;
                } else {
                    self.hash = HashState::Off;
                }
                self.on_arity_token(true);
            }
            _ => {
                self.hash = HashState::Off;
                self.on_arity_token(true);
            }
        }
    }

    fn is_record_field_dot(&self) -> bool {
        matches!(self.hash, HashState::SawHashName | HashState::SawWildcard)
    }

    fn open_group(&mut self) {
        match &mut self.fun {
            FunScan::WaitParen { name } if self.nest == 0 => {
                let name = std::mem::take(name);
                self.fun = FunScan::InArity {
                    name,
                    depth: self.nest + 1,
                    items: 0,
                    nonempty: false,
                };
            }
            _ => {}
        }
        match &mut self.attr {
            AttrScan::ModuleWaitParen => self.attr = AttrScan::ModuleWaitName,
            AttrScan::FeatureWaitParen => self.attr = AttrScan::FeatureWaitName,
            _ => {}
        }
        self.nest += 1;
        if matches!(
            self.hash,
            HashState::SawHash | HashState::SawHashName | HashState::SawWildcard
        ) {
            self.hash = HashState::Off;
        }
    }

    fn close_group(&mut self) {
        self.nest = self.nest.saturating_sub(1);
    }

    fn on_arity_token(&mut self, nonempty: bool) {
        if let FunScan::InArity { nonempty: flag, .. } = &mut self.fun
            && nonempty
        {
            *flag = true;
        }
    }

    fn on_arity_comma(&mut self) {
        if let FunScan::InArity {
            depth,
            items,
            nonempty,
            ..
        } = &mut self.fun
            && self.nest == *depth
        {
            if *nonempty {
                *items += 1;
            }
            *nonempty = false;
        }
    }

    fn finish_arity_if_closed(&mut self) {
        let FunScan::InArity {
            name,
            depth,
            items,
            nonempty,
        } = &self.fun
        else {
            return;
        };
        if self.nest != *depth - 1 {
            return;
        }
        let arity = if *nonempty { *items + 1 } else { *items };
        self.fun = FunScan::Body {
            name: name.clone(),
            arity,
        };
    }

    fn on_atom(&mut self, atom: &str, module: &mut Option<String>, enabled: &mut HashSet<String>) {
        if self.hash == HashState::SawHash {
            self.hash = HashState::SawHashName;
        } else {
            self.hash = HashState::Off;
        }

        if self.at_form_start && self.nest == 0 && matches!(self.attr, AttrScan::Off) {
            self.at_form_start = false;
            self.fun = FunScan::WaitParen {
                name: atom.to_string(),
            };
            return;
        }

        match &self.attr {
            AttrScan::AfterHyphen => {
                self.attr = match atom {
                    "module" => AttrScan::ModuleWaitParen,
                    "feature" => AttrScan::FeatureWaitParen,
                    _ => AttrScan::Other,
                };
                return;
            }
            AttrScan::ModuleWaitName => {
                *module = Some(atom.to_string());
                self.attr = AttrScan::Other;
                return;
            }
            AttrScan::FeatureWaitName => {
                self.attr = AttrScan::FeatureWaitComma {
                    name: atom.to_string(),
                };
                return;
            }
            AttrScan::FeatureWaitAction { name } => {
                match atom {
                    "enable" => {
                        enabled.insert(name.clone());
                    }
                    "disable" => {
                        enabled.remove(name);
                    }
                    _ => {}
                }
                self.attr = AttrScan::Other;
                return;
            }
            _ => {}
        }

        self.on_arity_token(true);
    }

    fn advance_attr_comma(&mut self) {
        if let AttrScan::FeatureWaitComma { name } = &self.attr {
            self.attr = AttrScan::FeatureWaitAction { name: name.clone() };
        }
    }

    fn end_form(&mut self) {
        self.nest = 0;
        self.at_form_start = true;
        self.hash = HashState::Off;
        self.after_fun = false;
        self.after_fun_name = false;
        self.attr = AttrScan::Off;
        self.fun = FunScan::Off;
    }
}

pub fn otp_release_from_tag(tag: &str) -> Option<u32> {
    let rest = tag.strip_prefix("OTP-").unwrap_or(tag);
    rest.split('.').next()?.parse().ok()
}

pub fn otp_release_from_env() -> Option<u32> {
    match std::env::var("OTP_TAG") {
        Ok(tag) if !tag.is_empty() => otp_release_from_tag(&tag),
        _ => None,
    }
}

fn emit_atom(name: &str) -> String {
    if is_bare_atom(name) {
        name.to_string()
    } else {
        let mut out = String::from("'");
        for c in name.chars() {
            match c {
                '\\' | '\'' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => {
                    out.push(c);
                }
            }
        }
        out.push('\'');
        out
    }
}

fn emit_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' | '"' => {
                out.push('\\');
                out.push(c);
            }
            _ => {
                out.push(c);
            }
        }
    }
    out.push('"');
    out
}

fn emit_bool(v: bool) -> String {
    if v { "true" } else { "false" }.to_string()
}

fn evaluate_condition(tokens: &[erl_pp::SourceToken], macros: &erl_pp::MacroTable) -> Option<bool> {
    let lex: Vec<&erl_pp::SourceToken> = tokens
        .iter()
        .filter(|t| t.token().kind().is_lexical())
        .collect();
    match lex.len() {
        1 => atom_truthy(lex[0]),
        3 => compare_three(lex[0], lex[1], lex[2]),
        _ => evaluate_defined_orelse_chain(&lex, macros),
    }
}

fn atom_truthy(token: &erl_pp::SourceToken) -> Option<bool> {
    match token.value() {
        erl_tokenize::TokenValue::Atom(a) => Some(a.as_ref() != "false"),
        erl_tokenize::TokenValue::Integer(Some(0)) => Some(false),
        erl_tokenize::TokenValue::Integer(Some(_)) => Some(true),
        _ => None,
    }
}

fn compare_three(
    left: &erl_pp::SourceToken,
    op: &erl_pp::SourceToken,
    right: &erl_pp::SourceToken,
) -> Option<bool> {
    let left = cond_value(left)?;
    let right = cond_value(right)?;
    let cmp = compare_op(op)?;
    compare_values(&left, &right, cmp)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompareOp {
    Eq,
    NotEq,
    ExactEq,
    ExactNotEq,
    Less,
    Greater,
    LessEq,
    GreaterEq,
}

fn compare_op(token: &erl_pp::SourceToken) -> Option<CompareOp> {
    match token.token().kind() {
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Eq) => Some(CompareOp::Eq),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::NotEq) => Some(CompareOp::NotEq),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::ExactEq) => Some(CompareOp::ExactEq),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::ExactNotEq) => {
            Some(CompareOp::ExactNotEq)
        }
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Less) => Some(CompareOp::Less),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::Greater) => Some(CompareOp::Greater),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::LessEq) => Some(CompareOp::LessEq),
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::GreaterEq) => {
            Some(CompareOp::GreaterEq)
        }
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum CondValue {
    Name(String),
    Integer(u64),
}

fn cond_value(token: &erl_pp::SourceToken) -> Option<CondValue> {
    match token.value() {
        erl_tokenize::TokenValue::Atom(a) => Some(CondValue::Name(a.into_owned())),
        erl_tokenize::TokenValue::Variable(v) => Some(CondValue::Name(v.to_string())),
        erl_tokenize::TokenValue::Integer(Some(n)) => Some(CondValue::Integer(n)),
        _ => None,
    }
}

fn compare_values(left: &CondValue, right: &CondValue, op: CompareOp) -> Option<bool> {
    match (left, right, op) {
        (CondValue::Name(a), CondValue::Name(b), CompareOp::Eq | CompareOp::ExactEq) => {
            Some(a == b)
        }
        (CondValue::Name(a), CondValue::Name(b), CompareOp::NotEq | CompareOp::ExactNotEq) => {
            Some(a != b)
        }
        (CondValue::Integer(a), CondValue::Integer(b), CompareOp::Eq | CompareOp::ExactEq) => {
            Some(a == b)
        }
        (
            CondValue::Integer(a),
            CondValue::Integer(b),
            CompareOp::NotEq | CompareOp::ExactNotEq,
        ) => Some(a != b),
        (CondValue::Integer(a), CondValue::Integer(b), CompareOp::Less) => Some(a < b),
        (CondValue::Integer(a), CondValue::Integer(b), CompareOp::Greater) => Some(a > b),
        (CondValue::Integer(a), CondValue::Integer(b), CompareOp::LessEq) => Some(a <= b),
        (CondValue::Integer(a), CondValue::Integer(b), CompareOp::GreaterEq) => Some(a >= b),
        _ => None,
    }
}

fn evaluate_defined_orelse_chain(
    lex: &[&erl_pp::SourceToken],
    macros: &erl_pp::MacroTable,
) -> Option<bool> {
    let mut i = 0usize;
    while i < lex.len() {
        if i > 0 {
            match lex[i].token().kind() {
                erl_tokenize::TokenKind::Keyword(erl_tokenize::Keyword::Orelse) => i += 1,
                _ => return None,
            }
        }
        let name = parse_defined_call(lex, &mut i)?;
        if macros.is_defined(&name) {
            return Some(true);
        }
    }
    Some(false)
}

fn parse_defined_call(lex: &[&erl_pp::SourceToken], i: &mut usize) -> Option<String> {
    let atom = token_name(lex.get(*i)?)?;
    if atom != "defined" {
        return None;
    }
    *i += 1;
    match lex.get(*i)?.token().kind() {
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::OpenParen) => *i += 1,
        _ => return None,
    }
    let name = token_name(lex.get(*i)?)?;
    *i += 1;
    match lex.get(*i)?.token().kind() {
        erl_tokenize::TokenKind::Symbol(erl_tokenize::Symbol::CloseParen) => *i += 1,
        _ => return None,
    }
    Some(name)
}

fn token_name(token: &erl_pp::SourceToken) -> Option<String> {
    match token.token().kind() {
        erl_tokenize::TokenKind::Atom | erl_tokenize::TokenKind::Variable => match token.value() {
            erl_tokenize::TokenValue::Atom(a) => Some(a.into_owned()),
            erl_tokenize::TokenValue::Variable(v) => Some(v.to_string()),
            _ => None,
        },
        _ => None,
    }
}

fn is_bare_atom(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && erl_tokenize::Keyword::from_text(name).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn otp_release_from_otp_tag() {
        assert_eq!(otp_release_from_tag("OTP-29.0.5"), Some(29));
        assert_eq!(otp_release_from_tag("29.0.5"), Some(29));
        assert_eq!(otp_release_from_tag(""), None);
    }
}
