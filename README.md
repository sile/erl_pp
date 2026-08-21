erl_pp
======

[![erl_pp](https://img.shields.io/crates/v/erl_pp.svg)](https://crates.io/crates/erl_pp)
[![Documentation](https://docs.rs/erl_pp/badge.svg)](https://docs.rs/erl_pp)
[![Actions Status](https://github.com/sile/erl_pp/workflows/CI/badge.svg)](https://github.com/sile/erl_pp/actions)
![License](https://img.shields.io/crates/l/erl_pp)

Erlang source code preprocessor. A Sans-I/O state machine for language
tools: the caller tokenizes (`erl_tokenize::scan_token`), performs I/O,
include search, `-if` / `-elif` evaluation, and unknown-macro meaning.
This crate advances directives, the macro table, and the condition stack.

Examples
--------

Tokenize, wrap in `Source`, loop on `Preprocessor::step`.
`Event::Token` is lexical only; whitespace and comments stay on `Source`.

```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let text = "atom, foo, bar.";
    let mut tokens = Vec::new();
    let mut position = erl_tokenize::Position::new();
    while let Some(t) = erl_tokenize::scan_token(text, position)? {
        position = t.end();
        tokens.push(t);
    }
    let source = erl_pp::Source::new("example.erl", text, tokens);
    let mut pp = erl_pp::Preprocessor::new([source]);

    let mut lexical = Vec::<String>::new();
    loop {
        match pp.step()? {
            erl_pp::Event::Token(t) => lexical.push(t.text().to_owned()),
            erl_pp::Event::Complete => break,
            other => unreachable!("unexpected event: {other:?}"),
        }
    }
    assert_eq!(lexical, ["atom", ",", "foo", ",", "bar", "."]);
    Ok(())
}
```

`Preprocessor` does not search the filesystem. `open_include` is optional
OTP 29.0 path resolution and returns a path, not a file handle.

Each `Event` variant documents its contract. The [crate rustdoc](https://docs.rs/erl_pp)
covers the `step` driver, typical compiler vs formatter / linter
policies, and how to continue after input failure.

`examples/pp.rs` prints lexical tokens from a file or stdin.
`examples/check_otp.rs` walks an OTP tree; it is not an introduction.

References
----------

- [Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html)
