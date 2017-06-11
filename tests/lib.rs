extern crate erl_pp;
extern crate erl_tokenize;

use erl_pp::Preprocessor;
use erl_tokenize::{Tokenizer, TokenKind};

fn pp(text: &str) -> Preprocessor {
    let tokenizer = Tokenizer::new(text);
    Preprocessor::new(tokenizer)
}

#[test]
fn no_directive_works() {
    let src = r#"io:format("Hello")."#;
    let tokens = pp(src).collect::<Result<Vec<_>, _>>().unwrap();

    assert_eq!(tokens.iter().map(|t| t.kind()).collect::<Vec<_>>(),
               [TokenKind::Atom,
                TokenKind::Symbol,
                TokenKind::Atom,
                TokenKind::Symbol,
                TokenKind::String,
                TokenKind::Symbol,
                TokenKind::Symbol]);
}

#[test]
fn define_works() {
    let src = r#"aaa. -define(foo, [bar, baz]). bbb."#;
    let tokens = pp(src).collect::<Result<Vec<_>, _>>().unwrap();

    assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
               ["aaa", ".", " ", " ", "bbb", "."]);

    let src = r#"aaa. -define(Foo (A, B), [bar, A, baz, B]). bbb."#;
    let tokens = pp(src).collect::<Result<Vec<_>, _>>().unwrap();

    assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
               ["aaa", ".", " ", " ", "bbb", "."]);
}

#[test]
fn undef_works() {
    let src = r#"aaa. -undef(foo). bbb."#;
    let tokens = pp(src).collect::<Result<Vec<_>, _>>().unwrap();

    assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
               ["aaa", ".", " ", " ", "bbb", "."]);
}
