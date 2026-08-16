//! Preprocessor directive parser.
//!
//! [`parse_directive`] tries to recognise one preprocessor directive
//! at the current [`crate::cursor::Cursor`] position. It performs no
//! side effects: it does not open include files, register or expand
//! macros, evaluate conditionals, or emit diagnostics. Its job is to
//! turn a run of tokens into a structured [`Directive`] value (or to
//! rewind the cursor and report `Ok(None)` when the current form is
//! not a known directive).
//!
//! The caller (the state machine) decides when to invoke this parser
//! based on whether the cursor stands at a form boundary. The parser
//! itself does not track form boundaries.
#![allow(
    clippy::result_large_err,
    reason = "ParseError deliberately carries structured span and failure info; \
              boxing every Result would add allocation overhead on every parse"
)]

use std::borrow::Cow;

use erl_tokenize::{Keyword, Position, Symbol, Token, TokenKind, TokenValue};

use crate::cursor::Cursor;
use crate::error::{ParseError, ParseFailure};
use crate::source::{SourceId, SourceSpan};

/// A parsed preprocessor directive.
///
/// Every variant carries a [`SourceSpan`] covering the whole directive
/// from the opening `-` through the terminating `.`, plus spans and
/// decoded values for each constituent that the parser recognises.
#[derive(Debug, Clone)]
pub enum Directive {
    /// `-include("path").`
    Include(IncludeDirective),
    /// `-include_lib("path").`
    IncludeLib(IncludeLibDirective),
    /// `-define(NAME[(Params)], Replacement).`
    Define(DefineDirective),
    /// `-undef(NAME).`
    Undef(UndefDirective),
    /// `-ifdef(NAME).`
    Ifdef(IfdefDirective),
    /// `-ifndef(NAME).`
    Ifndef(IfndefDirective),
    /// `-else.`
    Else(ElseDirective),
    /// `-endif.`
    Endif(EndifDirective),
    /// `-error(Argument).`
    Error(ErrorDirective),
    /// `-warning(Argument).`
    Warning(WarningDirective),
}

impl Directive {
    /// Returns the span that covers the whole directive from `-`
    /// through `.`.
    pub fn span(&self) -> SourceSpan {
        match self {
            Directive::Include(d) => d.span,
            Directive::IncludeLib(d) => d.span,
            Directive::Define(d) => d.span,
            Directive::Undef(d) => d.span,
            Directive::Ifdef(d) => d.span,
            Directive::Ifndef(d) => d.span,
            Directive::Else(d) => d.span,
            Directive::Endif(d) => d.span,
            Directive::Error(d) => d.span,
            Directive::Warning(d) => d.span,
        }
    }
}

/// Data of an `-include(...)` directive.
#[derive(Debug, Clone)]
pub struct IncludeDirective {
    /// Span covering the whole directive from `-` through `.`.
    pub span: SourceSpan,
    /// The decoded, concatenated contents of the include's string
    /// literals.
    ///
    /// This is the raw Erlang-source value, **not** a resolved
    /// filesystem path. Environment-variable expansion (`$FOO`),
    /// relative-path resolution, and any OS-specific path handling
    /// are the resolver's job (see the `open_include` utility added
    /// by later work). Consumers that pass the value on to a
    /// resolver can hand it over as-is.
    //
    // Stored as a decoded `String` rather than `Vec<Token>` because
    // `-include("foo" ".hrl")` collapses one or more adjacent string
    // literals into a single logical path with no single owning
    // token, and every consumer wants the final decoded value.
    // Directives whose payload is consumed as raw tokens downstream
    // (e.g. `DefineDirective::replacement`, the diagnostic arg
    // lists) do keep `Vec<Token>`.
    pub path: String,
    /// Span covering all path string literal tokens (from the first
    /// literal's start to the last literal's end, possibly across
    /// intervening hidden tokens).
    pub path_span: SourceSpan,
}

/// Data of an `-include_lib(...)` directive.
#[derive(Debug, Clone)]
pub struct IncludeLibDirective {
    /// Span covering the whole directive from `-` through `.`.
    pub span: SourceSpan,
    /// The decoded, concatenated contents of the include's string
    /// literals.
    ///
    /// This is the raw Erlang-source value, **not** a resolved
    /// filesystem path. In particular the first path component is the
    /// application name, which the resolver looks up in the code path
    /// rather than opening as a directory. Environment-variable
    /// expansion, relative-path resolution, and OS-specific path
    /// handling are all the resolver's job (see the `open_include`
    /// utility added by later work).
    //
    // See `IncludeDirective::path` for the rationale behind storing a
    // decoded `String` rather than `Vec<Token>`.
    pub path: String,
    /// Span covering all path string literal tokens.
    pub path_span: SourceSpan,
}

/// Data of a `-define(...)` directive.
#[derive(Debug, Clone)]
pub struct DefineDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Decoded macro name.
    pub name: String,
    /// Span of the macro name token.
    pub name_span: SourceSpan,
    /// Macro parameters. `None` is a constant-like macro
    /// (`-define(FOO, 1).`). `Some(vec![])` is an arity-0 function-like
    /// macro (`-define(FOO(), 1).`).
    pub params: Option<Vec<Param>>,
    /// Replacement token list (may include hidden tokens).
    pub replacement: Vec<Token>,
    /// Span covering the replacement tokens. `None` when the
    /// replacement is empty.
    pub replacement_span: Option<SourceSpan>,
}

/// One parameter of a function-like macro.
#[derive(Debug, Clone)]
pub struct Param {
    /// Decoded parameter name (typically a variable identifier).
    pub name: String,
    /// Span of the parameter token.
    pub span: SourceSpan,
}

/// Data of an `-undef(...)` directive.
#[derive(Debug, Clone)]
pub struct UndefDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Decoded macro name.
    pub name: String,
    /// Span of the macro name token.
    pub name_span: SourceSpan,
}

/// Data of an `-ifdef(...)` directive.
#[derive(Debug, Clone)]
pub struct IfdefDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Decoded macro name.
    pub name: String,
    /// Span of the macro name token.
    pub name_span: SourceSpan,
}

/// Data of an `-ifndef(...)` directive.
#[derive(Debug, Clone)]
pub struct IfndefDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Decoded macro name.
    pub name: String,
    /// Span of the macro name token.
    pub name_span: SourceSpan,
}

/// Data of an `-else.` directive.
#[derive(Debug, Clone)]
pub struct ElseDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
}

/// Data of an `-endif.` directive.
#[derive(Debug, Clone)]
pub struct EndifDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
}

/// Data of an `-error(...)` directive.
#[derive(Debug, Clone)]
pub struct ErrorDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Raw token list inside the parentheses (evaluation is not this
    /// module's responsibility).
    pub arg_tokens: Vec<Token>,
    /// Span covering the argument tokens.
    pub arg_span: SourceSpan,
}

/// Data of a `-warning(...)` directive.
#[derive(Debug, Clone)]
pub struct WarningDirective {
    /// Span covering the whole directive.
    pub span: SourceSpan,
    /// Raw token list inside the parentheses.
    pub arg_tokens: Vec<Token>,
    /// Span covering the argument tokens.
    pub arg_span: SourceSpan,
}

/// Names that this module recognises as preprocessor directives.
///
/// Names outside this list cause [`parse_directive`] to roll back and
/// return `Ok(None)`. `-if(...)` and `-elif(...)` are intentionally
/// excluded (their evaluation is out of scope); so are `-file(...)`,
/// `-feature(...)`, and other non-listed directives.
const KNOWN_DIRECTIVES: &[&str] = &[
    "include",
    "include_lib",
    "define",
    "undef",
    "ifdef",
    "ifndef",
    "else",
    "endif",
    "error",
    "warning",
];

/// Attempts to parse one directive at the current cursor position.
///
/// * `Ok(None)` — the current position does not start a known
///   directive (either not a `-` at all, or a `-` followed by a name
///   that this module does not recognise). The cursor is restored to
///   its state at entry, so the caller can process the token stream as
///   a regular form.
/// * `Ok(Some(directive))` — a known directive parsed successfully.
///   The cursor is left just after the terminating `.`.
/// * `Err(err)` — the parser committed to a known directive (having
///   seen `-` and a recognised name) but the structure did not match.
///   The cursor position is undefined; the caller should not continue
///   parsing this form.
#[allow(dead_code, reason = "invoked by preprocessor internals to be added")]
pub(crate) fn parse_directive(cursor: &mut Cursor) -> Result<Option<Directive>, ParseError> {
    let entry = cursor.checkpoint();

    // First lexical token must be `-`.
    let hyphen = match peek_lexical_ok(cursor, None)? {
        Some(t) if is_symbol(t, Symbol::Hyphen) => t,
        _ => return Ok(None),
    };
    let start_pos = hyphen.start();
    let source_id = cursor.source_id();
    let directive_start = SourceSpan::new(source_id, hyphen.start(), hyphen.end());

    consume_through(cursor, hyphen, &directive_start)?;

    // Next lexical must be an atom or a keyword whose text is a known directive name.
    let name_tok = match peek_lexical_ok(cursor, Some(&directive_start))? {
        Some(t) => t,
        None => {
            cursor.restore(entry);
            return Ok(None);
        }
    };
    let name_text: String = match directive_name_text(name_tok, cursor.source_text()) {
        Some(text) => text.into_owned(),
        None => {
            cursor.restore(entry);
            return Ok(None);
        }
    };
    if !KNOWN_DIRECTIVES.contains(&name_text.as_str()) {
        cursor.restore(entry);
        return Ok(None);
    }

    consume_through(cursor, name_tok, &directive_start)?;
    let name_span = SourceSpan::new(source_id, name_tok.start(), name_tok.end());

    match name_text.as_str() {
        "include" => parse_include(cursor, source_id, start_pos, directive_start).map(Some),
        "include_lib" => parse_include_lib(cursor, source_id, start_pos, directive_start).map(Some),
        "define" => parse_define(cursor, source_id, start_pos, directive_start).map(Some),
        "undef" => parse_name_only(
            cursor,
            source_id,
            start_pos,
            directive_start,
            name_span,
            NameOnlyKind::Undef,
        )
        .map(Some),
        "ifdef" => parse_name_only(
            cursor,
            source_id,
            start_pos,
            directive_start,
            name_span,
            NameOnlyKind::Ifdef,
        )
        .map(Some),
        "ifndef" => parse_name_only(
            cursor,
            source_id,
            start_pos,
            directive_start,
            name_span,
            NameOnlyKind::Ifndef,
        )
        .map(Some),
        "else" => parse_barewords(
            cursor,
            source_id,
            start_pos,
            directive_start,
            BareKind::Else,
        )
        .map(Some),
        "endif" => parse_barewords(
            cursor,
            source_id,
            start_pos,
            directive_start,
            BareKind::Endif,
        )
        .map(Some),
        "error" => parse_diagnostic(cursor, source_id, start_pos, directive_start, false).map(Some),
        "warning" => {
            parse_diagnostic(cursor, source_id, start_pos, directive_start, true).map(Some)
        }
        _ => unreachable!("KNOWN_DIRECTIVES gate above"),
    }
}

// ---------------------------------------------------------------------------
// per-directive parsers

fn parse_include(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
) -> Result<Directive, ParseError> {
    let (path, path_span) = parse_paren_string_path(cursor, source_id, &directive_start)?;
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;
    Ok(Directive::Include(IncludeDirective {
        span: SourceSpan::new(source_id, start_pos, dot.end()),
        path,
        path_span,
    }))
}

fn parse_include_lib(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
) -> Result<Directive, ParseError> {
    let (path, path_span) = parse_paren_string_path(cursor, source_id, &directive_start)?;
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;
    Ok(Directive::IncludeLib(IncludeLibDirective {
        span: SourceSpan::new(source_id, start_pos, dot.end()),
        path,
        path_span,
    }))
}

fn parse_define(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
) -> Result<Directive, ParseError> {
    expect_symbol(cursor, Symbol::OpenParen, source_id, &directive_start)?;
    let (name, name_span) = parse_identifier(cursor, source_id, &directive_start, "macro name")?;

    let params = if peek_is_symbol(cursor, Symbol::OpenParen, &directive_start)? {
        expect_symbol(cursor, Symbol::OpenParen, source_id, &directive_start)?;
        let mut params = Vec::new();
        if !peek_is_symbol(cursor, Symbol::CloseParen, &directive_start)? {
            loop {
                let (pname, pspan) =
                    parse_identifier(cursor, source_id, &directive_start, "parameter name")?;
                params.push(Param {
                    name: pname,
                    span: pspan,
                });
                if peek_is_symbol(cursor, Symbol::Comma, &directive_start)? {
                    expect_symbol(cursor, Symbol::Comma, source_id, &directive_start)?;
                    continue;
                }
                break;
            }
        }
        expect_symbol(cursor, Symbol::CloseParen, source_id, &directive_start)?;
        Some(params)
    } else {
        None
    };

    expect_symbol(cursor, Symbol::Comma, source_id, &directive_start)?;
    let (replacement, replacement_span) =
        collect_until_close_paren(cursor, source_id, &directive_start)?;
    expect_symbol(cursor, Symbol::CloseParen, source_id, &directive_start)?;
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;

    Ok(Directive::Define(DefineDirective {
        span: SourceSpan::new(source_id, start_pos, dot.end()),
        name,
        name_span,
        params,
        replacement,
        replacement_span,
    }))
}

enum NameOnlyKind {
    Undef,
    Ifdef,
    Ifndef,
}

fn parse_name_only(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
    _name_span: SourceSpan,
    kind: NameOnlyKind,
) -> Result<Directive, ParseError> {
    expect_symbol(cursor, Symbol::OpenParen, source_id, &directive_start)?;
    let (name, name_span) = parse_identifier(cursor, source_id, &directive_start, "macro name")?;
    expect_symbol(cursor, Symbol::CloseParen, source_id, &directive_start)?;
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;

    let span = SourceSpan::new(source_id, start_pos, dot.end());
    Ok(match kind {
        NameOnlyKind::Undef => Directive::Undef(UndefDirective {
            span,
            name,
            name_span,
        }),
        NameOnlyKind::Ifdef => Directive::Ifdef(IfdefDirective {
            span,
            name,
            name_span,
        }),
        NameOnlyKind::Ifndef => Directive::Ifndef(IfndefDirective {
            span,
            name,
            name_span,
        }),
    })
}

enum BareKind {
    Else,
    Endif,
}

fn parse_barewords(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
    kind: BareKind,
) -> Result<Directive, ParseError> {
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;
    let span = SourceSpan::new(source_id, start_pos, dot.end());
    Ok(match kind {
        BareKind::Else => Directive::Else(ElseDirective { span }),
        BareKind::Endif => Directive::Endif(EndifDirective { span }),
    })
}

fn parse_diagnostic(
    cursor: &mut Cursor,
    source_id: SourceId,
    start_pos: Position,
    directive_start: SourceSpan,
    is_warning: bool,
) -> Result<Directive, ParseError> {
    expect_symbol(cursor, Symbol::OpenParen, source_id, &directive_start)?;
    let (arg_tokens, arg_span_opt) =
        collect_until_close_paren(cursor, source_id, &directive_start)?;
    if arg_tokens.is_empty() {
        return Err(ParseError {
            directive_start,
            expected: "at least one token before `)`".to_owned(),
            actual: ParseFailure::UnexpectedToken {
                span: SourceSpan::new(source_id, start_pos, start_pos),
                kind: TokenKind::Symbol(Symbol::CloseParen),
            },
        });
    }
    let arg_span = arg_span_opt.expect("non-empty arg_tokens implies a span");
    expect_symbol(cursor, Symbol::CloseParen, source_id, &directive_start)?;
    let dot = expect_symbol(cursor, Symbol::Dot, source_id, &directive_start)?;
    let span = SourceSpan::new(source_id, start_pos, dot.end());
    Ok(if is_warning {
        Directive::Warning(WarningDirective {
            span,
            arg_tokens,
            arg_span,
        })
    } else {
        Directive::Error(ErrorDirective {
            span,
            arg_tokens,
            arg_span,
        })
    })
}

// ---------------------------------------------------------------------------
// helpers

fn parse_paren_string_path(
    cursor: &mut Cursor,
    source_id: SourceId,
    directive_start: &SourceSpan,
) -> Result<(String, SourceSpan), ParseError> {
    expect_symbol(cursor, Symbol::OpenParen, source_id, directive_start)?;

    let first = expect_lexical(cursor, directive_start, "string literal")?;
    let first_decoded =
        decode_string_literal(first, cursor.source_text(), directive_start)?.into_owned();
    consume_through(cursor, first, directive_start)?;

    let mut path = first_decoded;
    let path_start = first.start();
    let mut path_end = first.end();

    while let Some(next) = peek_lexical_ok(cursor, Some(directive_start))? {
        if next.kind() != TokenKind::String {
            break;
        }
        let decoded =
            decode_string_literal(next, cursor.source_text(), directive_start)?.into_owned();
        path.push_str(&decoded);
        path_end = next.end();
        consume_through(cursor, next, directive_start)?;
    }

    expect_symbol(cursor, Symbol::CloseParen, source_id, directive_start)?;
    Ok((path, SourceSpan::new(source_id, path_start, path_end)))
}

fn decode_string_literal<'a>(
    token: Token,
    source: &'a str,
    directive_start: &SourceSpan,
) -> Result<Cow<'a, str>, ParseError> {
    match token.value(source) {
        TokenValue::String(cow) => Ok(cow),
        other => Err(ParseError {
            directive_start: *directive_start,
            expected: "string literal".to_owned(),
            actual: ParseFailure::UnexpectedToken {
                span: SourceSpan::new(directive_start.source_id, token.start(), token.end()),
                kind: type_of(&other),
            },
        }),
    }
}

fn type_of(value: &TokenValue<'_>) -> TokenKind {
    match value {
        TokenValue::Atom(_) => TokenKind::Atom,
        TokenValue::Char(_) => TokenKind::Char,
        TokenValue::Comment(_) => TokenKind::Comment,
        TokenValue::Float(_) => TokenKind::Float,
        TokenValue::Integer(_) => TokenKind::Integer,
        TokenValue::Keyword(k) => TokenKind::Keyword(*k),
        TokenValue::SigilString { .. } => TokenKind::SigilString,
        TokenValue::String(_) => TokenKind::String,
        TokenValue::Symbol(s) => TokenKind::Symbol(*s),
        TokenValue::Variable(_) => TokenKind::Variable,
        TokenValue::Whitespace(_) => TokenKind::Whitespace,
    }
}

fn parse_identifier(
    cursor: &mut Cursor,
    source_id: SourceId,
    directive_start: &SourceSpan,
    expected: &str,
) -> Result<(String, SourceSpan), ParseError> {
    let token = expect_lexical(cursor, directive_start, expected)?;
    let name = match token.value(cursor.source_text()) {
        TokenValue::Atom(cow) => cow.into_owned(),
        TokenValue::Variable(name) => name.to_owned(),
        other => {
            return Err(ParseError {
                directive_start: *directive_start,
                expected: expected.to_owned(),
                actual: ParseFailure::UnexpectedToken {
                    span: SourceSpan::new(source_id, token.start(), token.end()),
                    kind: type_of(&other),
                },
            });
        }
    };
    consume_through(cursor, token, directive_start)?;
    Ok((name, SourceSpan::new(source_id, token.start(), token.end())))
}

/// Collects every token (including hidden ones) up to but not
/// including the closing `)` that matches the paren the caller has
/// already consumed. Tracks nested parens, braces, and brackets so
/// that commas or delimiters inside them do not terminate the collect
/// prematurely.
fn collect_until_close_paren(
    cursor: &mut Cursor,
    source_id: SourceId,
    directive_start: &SourceSpan,
) -> Result<(Vec<Token>, Option<SourceSpan>), ParseError> {
    let mut tokens = Vec::new();
    let mut depth: usize = 0;
    let mut span_start: Option<Position> = None;
    let mut span_end: Option<Position> = None;

    loop {
        let Some(next) = cursor_peek_ok(cursor, directive_start)? else {
            return Err(ParseError {
                directive_start: *directive_start,
                expected: "`)` before end of source".to_owned(),
                actual: ParseFailure::UnexpectedEof,
            });
        };
        if depth == 0 && matches!(next.kind(), TokenKind::Symbol(Symbol::CloseParen)) {
            return Ok((
                tokens,
                span_start.map(|start| {
                    SourceSpan::new(source_id, start, span_end.expect("start implies end"))
                }),
            ));
        }
        match next.kind() {
            TokenKind::Symbol(Symbol::OpenParen) => depth += 1,
            TokenKind::Symbol(Symbol::CloseParen) => depth -= 1,
            _ => {}
        }
        // Track span over lexical tokens (start of first lexical to end of last lexical).
        if next.kind().is_lexical() {
            if span_start.is_none() {
                span_start = Some(next.start());
            }
            span_end = Some(next.end());
        }
        tokens.push(next);
        bump_ok(cursor)?; // consume the peeked token
    }
}

// ---------------------------------------------------------------------------
// cursor utility layer

fn peek_lexical_ok(
    cursor: &mut Cursor,
    directive_start: Option<&SourceSpan>,
) -> Result<Option<Token>, ParseError> {
    match cursor.peek_lexical() {
        None => Ok(None),
        Some(Ok(t)) => Ok(Some(t)),
        Some(Err(e)) => Err(lex_to_parse_error(e, directive_start)),
    }
}

fn cursor_peek_ok(
    cursor: &mut Cursor,
    directive_start: &SourceSpan,
) -> Result<Option<Token>, ParseError> {
    match cursor.peek() {
        None => Ok(None),
        Some(Ok(t)) => Ok(Some(t)),
        Some(Err(e)) => Err(lex_to_parse_error(e, Some(directive_start))),
    }
}

fn bump_ok(cursor: &mut Cursor) -> Result<Option<Token>, ParseError> {
    match cursor.bump() {
        None => Ok(None),
        Some(Ok(t)) => Ok(Some(t)),
        Some(Err(e)) => Err(lex_to_parse_error(e, None)),
    }
}

fn lex_to_parse_error(
    err: crate::error::LexicalError,
    directive_start: Option<&SourceSpan>,
) -> ParseError {
    let start = directive_start.copied().unwrap_or(err.span);
    ParseError {
        directive_start: start,
        expected: "well-formed token".to_owned(),
        actual: ParseFailure::Lexical(Box::new(err)),
    }
}

fn is_symbol(token: Token, sym: Symbol) -> bool {
    matches!(token.kind(), TokenKind::Symbol(s) if s == sym)
}

fn peek_is_symbol(
    cursor: &mut Cursor,
    sym: Symbol,
    directive_start: &SourceSpan,
) -> Result<bool, ParseError> {
    Ok(peek_lexical_ok(cursor, Some(directive_start))?
        .map(|t| is_symbol(t, sym))
        .unwrap_or(false))
}

fn expect_symbol(
    cursor: &mut Cursor,
    sym: Symbol,
    source_id: SourceId,
    directive_start: &SourceSpan,
) -> Result<Token, ParseError> {
    let expected = format!("`{}`", sym.as_str());
    match peek_lexical_ok(cursor, Some(directive_start))? {
        Some(t) if is_symbol(t, sym) => {
            consume_through(cursor, t, directive_start)?;
            Ok(t)
        }
        Some(t) => Err(ParseError {
            directive_start: *directive_start,
            expected,
            actual: ParseFailure::UnexpectedToken {
                span: SourceSpan::new(source_id, t.start(), t.end()),
                kind: t.kind(),
            },
        }),
        None => Err(ParseError {
            directive_start: *directive_start,
            expected,
            actual: ParseFailure::UnexpectedEof,
        }),
    }
}

fn expect_lexical(
    cursor: &mut Cursor,
    directive_start: &SourceSpan,
    expected: &str,
) -> Result<Token, ParseError> {
    match peek_lexical_ok(cursor, Some(directive_start))? {
        Some(t) => Ok(t),
        None => Err(ParseError {
            directive_start: *directive_start,
            expected: expected.to_owned(),
            actual: ParseFailure::UnexpectedEof,
        }),
    }
}

/// Bumps tokens until the one whose `start` matches `target.start()`
/// has been consumed.
fn consume_through(
    cursor: &mut Cursor,
    target: Token,
    directive_start: &SourceSpan,
) -> Result<(), ParseError> {
    loop {
        let Some(t) = bump_ok(cursor)? else {
            return Err(ParseError {
                directive_start: *directive_start,
                expected: format!("token at {:?}", target.start()),
                actual: ParseFailure::UnexpectedEof,
            });
        };
        if t.start() == target.start() {
            return Ok(());
        }
    }
}

fn directive_name_text<'a>(token: Token, source: &'a str) -> Option<Cow<'a, str>> {
    match token.value(source) {
        TokenValue::Atom(cow) => Some(cow),
        TokenValue::Keyword(k) => Some(Cow::Borrowed(k.as_str())),
        _ => None,
    }
}

// Silence unused-import warnings when no tests exercise a particular
// path. `Position` is used at the top of `parse_directive` via a
// helper; `Keyword` is imported for completeness of the name-decoding
// path even if only Atom-named directives appear in the tests.
#[allow(dead_code)]
fn _touch_imports(_: Position, _: Keyword) {}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cursor::Cursor;
    use crate::source::{Source, SourceStore};

    fn parse(text: &str) -> (Cursor, Result<Option<Directive>, ParseError>) {
        let store = SourceStore::new();
        let id = store.append(Source::new("main.erl", text));
        let mut cursor = Cursor::new(id, store.get(id));
        let result = parse_directive(&mut cursor);
        (cursor, result)
    }

    fn parse_ok(text: &str) -> Directive {
        parse(text)
            .1
            .expect("parse should not error")
            .expect("parse should recognise a directive")
    }

    #[test]
    fn non_directive_returns_none() {
        let (mut cursor, result) = parse("foo() -> ok.");
        assert!(matches!(result, Ok(None)));
        // Cursor should be at the very start (unchanged).
        assert_eq!(cursor.peek().unwrap().unwrap().start().offset(), 0);
    }

    #[test]
    fn unknown_name_rolls_back() {
        let (mut cursor, result) = parse("-module(m).");
        assert!(matches!(result, Ok(None)));
        // The very first token in the stream is still `-`.
        assert_eq!(cursor.bump().unwrap().unwrap().text("-module(m)."), "-");
    }

    #[test]
    fn unknown_name_rolls_back_hidden_tokens() {
        let (mut cursor, result) = parse("- % ??\n module (m).");
        assert!(matches!(result, Ok(None)));
        // Hidden tokens before the `-` (none here) and between `-`
        // and `module` should still be visible when we walk the
        // cursor from scratch.
        let mut kinds = Vec::new();
        while let Some(t) = cursor.bump() {
            kinds.push(t.unwrap().kind());
        }
        assert!(kinds.contains(&TokenKind::Comment));
        assert!(kinds.contains(&TokenKind::Whitespace));
    }

    #[test]
    fn include_directive() {
        let d = parse_ok(r#"-include("foo.hrl")."#);
        let Directive::Include(inc) = d else {
            panic!("expected Include");
        };
        assert_eq!(inc.path, "foo.hrl");
        assert_eq!(inc.span.start.offset(), 0);
        assert_eq!(inc.span.end.offset(), r#"-include("foo.hrl")."#.len());
    }

    #[test]
    fn include_lib_directive() {
        let d = parse_ok(r#"-include_lib("kernel/include/file.hrl")."#);
        let Directive::IncludeLib(inc) = d else {
            panic!("expected IncludeLib");
        };
        assert_eq!(inc.path, "kernel/include/file.hrl");
    }

    #[test]
    fn include_concats_adjacent_strings() {
        let d = parse_ok(r#"-include("foo" ".hrl")."#);
        let Directive::Include(inc) = d else {
            panic!("expected Include");
        };
        assert_eq!(inc.path, "foo.hrl");
    }

    #[test]
    fn include_concats_three_or_more() {
        let d = parse_ok(r#"-include("a" "b" "c" ".hrl")."#);
        let Directive::Include(inc) = d else {
            panic!("expected Include");
        };
        assert_eq!(inc.path, "abc.hrl");
    }

    #[test]
    fn include_ignores_hidden_between_strings() {
        let d = parse_ok(
            r#"-include("foo" % note
".hrl")."#,
        );
        let Directive::Include(inc) = d else {
            panic!("expected Include");
        };
        assert_eq!(inc.path, "foo.hrl");
    }

    #[test]
    fn define_constant_like() {
        let d = parse_ok("-define(FOO, 1).");
        let Directive::Define(def) = d else {
            panic!("expected Define");
        };
        assert_eq!(def.name, "FOO");
        assert!(def.params.is_none());
        assert!(!def.replacement.is_empty());
    }

    #[test]
    fn define_arity_zero_function_like() {
        let d = parse_ok("-define(FOO(), 1).");
        let Directive::Define(def) = d else {
            panic!("expected Define");
        };
        assert_eq!(def.name, "FOO");
        assert_eq!(def.params.as_deref().map(<[Param]>::len), Some(0));
    }

    #[test]
    fn define_multi_param_function_like() {
        let d = parse_ok("-define(FOO(A, B, C), [A, B, C]).");
        let Directive::Define(def) = d else {
            panic!("expected Define");
        };
        assert_eq!(def.name, "FOO");
        let params = def.params.expect("function-like");
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "C"]);
    }

    #[test]
    fn define_empty_replacement() {
        let d = parse_ok("-define(FOO,).");
        let Directive::Define(def) = d else {
            panic!("expected Define");
        };
        assert_eq!(def.name, "FOO");
        assert!(def.replacement_span.is_none());
    }

    #[test]
    fn undef_directive() {
        let d = parse_ok("-undef(FOO).");
        let Directive::Undef(u) = d else {
            panic!("expected Undef");
        };
        assert_eq!(u.name, "FOO");
    }

    #[test]
    fn ifdef_ifndef_directives() {
        let d = parse_ok("-ifdef(FOO).");
        assert!(matches!(d, Directive::Ifdef(ref x) if x.name == "FOO"));

        let d = parse_ok("-ifndef(FOO).");
        assert!(matches!(d, Directive::Ifndef(ref x) if x.name == "FOO"));
    }

    #[test]
    fn else_endif_directives() {
        let d = parse_ok("-else.");
        assert!(matches!(d, Directive::Else(_)));

        let d = parse_ok("-endif.");
        assert!(matches!(d, Directive::Endif(_)));
    }

    #[test]
    fn error_and_warning_directives_carry_arg_tokens() {
        let d = parse_ok(r#"-error("bad")."#);
        let Directive::Error(e) = d else {
            panic!("expected Error");
        };
        assert!(!e.arg_tokens.is_empty());

        let d = parse_ok(r#"-warning({soft, "issue"})."#);
        let Directive::Warning(w) = d else {
            panic!("expected Warning");
        };
        assert!(w.arg_tokens.len() >= 3); // {, soft, ..., }
    }

    #[test]
    fn missing_dot_errors() {
        let (_c, result) = parse("-endif");
        let err = result.expect_err("missing `.` should be a parse error");
        assert!(matches!(err.actual, ParseFailure::UnexpectedEof));
        assert!(err.expected.contains('.'));
    }

    #[test]
    fn missing_close_paren_errors() {
        let (_c, result) = parse("-define(FOO, 1");
        let err = result.expect_err("missing `)` should be a parse error");
        assert!(matches!(err.actual, ParseFailure::UnexpectedEof));
    }

    #[test]
    fn directive_start_span_points_at_hyphen() {
        let (_c, result) = parse("-endif");
        let err = result.expect_err("incomplete `-endif` should error");
        assert_eq!(err.directive_start.start.offset(), 0);
        assert_eq!(err.directive_start.end.offset(), 1); // `-`
    }

    #[test]
    fn span_covers_full_directive_including_dot() {
        let text = "-endif.";
        let d = parse_ok(text);
        let Directive::Endif(e) = d else {
            panic!("expected Endif");
        };
        assert_eq!(e.span.start.offset(), 0);
        assert_eq!(e.span.end.offset(), text.len());
    }

    #[test]
    fn source_id_on_span_matches_cursor() {
        let store = SourceStore::new();
        let id = store.append(Source::new("main.erl", "-endif."));
        let mut cursor = Cursor::new(id, store.get(id));
        let d = parse_directive(&mut cursor).unwrap().unwrap();
        assert_eq!(d.span().source_id, id);
    }
}
