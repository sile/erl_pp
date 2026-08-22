//! Integration tests for the public [`erl_pp::Source::from_text`] API.

use core::assert_matches;

#[test]
fn from_text_returns_lexical_error() {
    let err = erl_pp::Source::from_text("bad.erl", "\"unclosed")
        .expect_err("unclosed string must fail tokenization");
    assert_matches!(err, erl_tokenize::Error { .. });
}

#[test]
fn from_text_matches_manual_scan_and_new() {
    let text = "atom, foo, bar.";
    let from_text = erl_pp::Source::from_text("example.erl", text).expect("valid input");
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(token) = erl_tokenize::scan_token(text, position).expect("valid input") {
        position = token.end();
        tokens.push(token);
    }
    let by_new = erl_pp::Source::new("example.erl", text, tokens);
    assert_eq!(from_text.text(), by_new.text());
    assert_eq!(from_text.display_name(), by_new.display_name());
    assert_eq!(from_text.tokens().len(), by_new.tokens().len());
    for (a, b) in from_text.tokens().iter().zip(by_new.tokens()) {
        assert_eq!(a.kind(), b.kind());
        assert_eq!(a.start(), b.start());
        assert_eq!(a.end(), b.end());
        assert_eq!(a.text(text), b.text(text));
    }
}
