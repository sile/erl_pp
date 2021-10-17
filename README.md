erl_pp
======

[![erl_pp](https://img.shields.io/crates/v/erl_pp.svg)](https://crates.io/crates/erl_pp)
[![Documentation](https://docs.rs/erl_pp/badge.svg)](https://docs.rs/erl_pp)
[![Actions Status](https://github.com/sile/erl_pp/workflows/CI/badge.svg)](https://github.com/sile/erl_pp/actions)
[![Coverage Status](https://coveralls.io/repos/github/sile/erl_pp/badge.svg?branch=master)](https://coveralls.io/github/sile/erl_pp?branch=master)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

An Erlang source code preprocessor written in Rust.

[Documentation](https://docs.rs/erl_pp)

References
----------

- [Erlang Reference Manual -- Preprocessor](http://erlang.org/doc/reference_manual/macros.html)

Examples
--------

Preprocesses an Erlang source code snippet.

```rust
use erl_pp::Preprocessor;
use erl_tokenize::Lexer;

let src = r#"-define(FOO(A), {A, ?LINE}). io:format("Hello: ~p", [?FOO(bar)])."#;
let pp = Preprocessor::new(Lexer::new(src));
let tokens = pp.collect::<Result<Vec<_>, _>>().unwrap();

assert_eq!(tokens.iter().map(|t| t.text()).collect::<Vec<_>>(),
           ["io", ":", "format", "(", r#""Hello: ~p""#, ",",
            "[", "{", "bar", ",", "1", "}", "]", ")", "."]);
```

Executes the example `pp` command:

```bash
$ cargo run --example pp -- /dev/stdin <<EOS
-define(FOO, foo).
-define(BAR(A), {bar, A}).

-ifdef(FOO).

foo() ->
  ?FOO + ?BAR(baz).

-endif.
EOS

[Position { filepath: Some("stdin"), offset: 61, line: 6, column: 1 }] "foo"
[Position { filepath: Some("stdin"), offset: 64, line: 6, column: 4 }] "("
[Position { filepath: Some("stdin"), offset: 65, line: 6, column: 5 }] ")"
[Position { filepath: Some("stdin"), offset: 67, line: 6, column: 2 }] "->"
[Position { filepath: Some("stdin"), offset: 13, line: 1, column: 2 }] "foo"
[Position { filepath: Some("stdin"), offset: 77, line: 7, column: 2 }] "+"
[Position { filepath: Some("stdin"), offset: 35, line: 2, column: 2 }] "{"
[Position { filepath: Some("stdin"), offset: 36, line: 2, column: 3 }] "bar"
[Position { filepath: Some("stdin"), offset: 39, line: 2, column: 6 }] ","
[Position { filepath: Some("stdin"), offset: 84, line: 7, column: 7 }] "baz"
[Position { filepath: Some("stdin"), offset: 42, line: 2, column: 3 }] "}"
[Position { filepath: Some("stdin"), offset: 88, line: 7, column: 11 }] "."
TOKEN COUNT: 12
ELAPSED: 0.001244 seconds
```
