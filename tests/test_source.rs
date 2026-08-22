//! Integration tests for building [`erl_pp::Source`] from scanned tokens.

use std::assert_matches;

#[test]
fn scan_tokens_returns_lexical_error() {
    let err = erl_tokenize::scan_tokens("\"unclosed").expect_err("unclosed string must fail");
    assert_matches!(err, erl_tokenize::Error { .. });
}

#[test]
fn new_with_scan_tokens_round_trips() {
    let text = "atom, foo, bar.";
    let tokens = erl_tokenize::scan_tokens(text).expect("valid input");
    let source = erl_pp::Source::new("example.erl", text, tokens.clone());
    assert_eq!(source.text(), text);
    assert_eq!(source.display_name(), "example.erl");
    assert_eq!(source.tokens(), tokens.as_slice());
}
