# OTP epp compatibility notes

erl_pp aims for **practical** compatibility with OTP `epp`, not
bit-for-bit identity. Real Erlang tooling that goes through erl_pp
should see the same tokens for the same source in the overwhelming
majority of cases, but a handful of intentional design differences
and a few small corner cases exist. This document names them so
callers can plan around them without having to read the
preprocessor's implementation.

The reference for OTP behaviour throughout is
`lib/stdlib/src/epp.erl` in the Erlang/OTP source tree (OTP 29 was
used while implementing this crate).

## Design choices that intentionally differ

### Hidden tokens stay in `Source`, not in `Event::Token`

OTP's epp receives a token stream from `erl_scan` that has already
dropped whitespace and comments. erl_pp's caller tokenizes first and
hands the full stream to `Source`, so the preprocessor can walk
hidden tokens for recognition without re-emitting them. `Event::Token`
is lexical only, which matches the stream OTP tools see.

Consequences:

- **`?FOO(   )` is arity 0**, the same as `?FOO()` and the same as
  OTP. Hidden tokens between `(` and `)` are not an argument.
- **`??Param` still ignores hidden.** The stringification step
  filters to lexical tokens before emitting, so the OTP-style output
  is preserved.
- **Hidden tokens between `?` and the macro name are absorbed by
  the call.** Both scan-time and rescan recognition skip hidden
  tokens to find the name, matching OTP.
- Argument payloads (`MacroCall::arguments`, diagnostic arguments,
  `-define` replacement bodies) still carry hidden tokens for
  substitution and span reconstruction. They do not appear as
  `Event::Token`.

### Argument-parsing delimiter set

The delimiter stack tracks:

- `( )`, `[ ]`, `{ }`, `<< >>`
- The `end`-terminated keyword blocks: `begin`, `if`, `case`,
  `maybe`, `receive`, `try`, `cond`
- A `fun_end` sentinel for `fun` — promoted to `end` on `->` or
  `when`, drained on outer `,` / `)`

This is enough for essentially every macro argument seen in real
Erlang source, including fun types (`fun((atom()) -> ok)`) and named
funs (`fun F(A) -> A end`). Deeply nested `fun bar/1` arity forms
that mix with a top-level `,` inside a macro argument can drift from
OTP's exact `epp_dodger`-style heuristics; that case is rare and no
test in the wild has been observed to exercise it.

### Predefined macros are a strict two-macro set

Only `?FILE` and `?LINE` are evaluated internally, because the
preprocessor has enough state to compute them (the current source
name and the outermost caller's line). Every other OTP predefined
macro — `?MACHINE`, `?MODULE`, `?MODULE_STRING`, `?FUNCTION_NAME`,
`?FUNCTION_ARITY`, `?OTP_RELEASE`, `?FEATURE_AVAILABLE` and so on —
reaches the caller through `Event::AwaitingMacroExpansion`. This
avoids drawing an arbitrary line between "erl_pp can" and "erl_pp
cannot" and lets callers implement whichever set is meaningful in
their environment (a formatter typically has none; an Erlang
compiler has all of them).

`?FILE` and `?LINE` are shadowable by a matching `-define`, which
is what OTP does as well.

### Empty resume deletes the call

OTP's epp treats an undefined macro as a preprocessor error. erl_pp
never does: an unknown `?NAME` becomes
`Event::AwaitingMacroExpansion`, and a token-free `Source` passed to
`resume_macro_expansion` removes the call from the stream. The
machine succeeds and emits nothing.

Callers that skip unknown macros this way must record the failure
themselves. A parser downstream of `Event::Token` may otherwise
treat the hole as a grammar error and look like an erl_pp bug.

### `?LINE` returns the outermost caller's line

When `?LINE` is invoked from inside a macro body (or a chain of
nested macros), the value is the line of the outermost user
invocation, not the line where `?LINE` sits in the definition.
This matches OTP's annotation-overwrite behaviour
(`epp.erl` `expand_macro/4`), realised in erl_pp by walking the
Origin chain to the top-level source.

### `??Param` stringification detail

Argument tokens are filtered to lexical (whitespace and comments
dropped, per OTP), then joined with a single space. Per-kind
formatting:

- `Integer` / `Float` — decoded value. `?S(16#FF)` stringifies to
  `"255"`, matching OTP's `io_lib:format("~w", ...)`.
- Everything else — the token's source text. Atoms therefore stay
  in whatever form they were written (`foo` stays unquoted; `'foo
  bar'` stays quoted). OTP's `io_lib:write_atom/1` chooses the
  quote form dynamically; erl_pp will differ from OTP on an
  unquoted atom whose text happens to require quoting when
  re-serialised, which is not something Erlang parsers actually
  produce.

### Origin chain is a superset of OTP's provenance

OTP overwrites token annotations to the outermost call site during
expansion; the earlier layers are gone. erl_pp keeps every
provenance step as an `Origin` chain (`Source` → `Include` →
`MacroBody` / `MacroArgument` / `Stringification` /
`CallerExpansion` / `SourceInfo`), so downstream tools can trace
where a token really came from. This is strictly more information
than OTP, not less.

## Known corner cases

### Function-like rescan across the queue/cursor boundary

Function-like rescan handles `?NAME(...)` when the whole call —
the `?`, name, `(`, arguments, and `)` — sits inside the expansion
queue (the usual case: the call is a literal inside another macro's
body). If the queue holds only `?NAME` and the arguments start in
the source cursor, erl_pp bails out and emits the `?`, name, and
`(` as raw tokens. Real Erlang code rarely writes a naked `?NAME`
at the very tail of a macro body whose caller supplies the
argument list; keeping the boundary case out of scope avoids a
speculative parse across two token producers.

### Cycles that cross a caller-response boundary

Cycle detection has two mechanisms:

- A static uses graph, walked as a DFS from every user-defined
  macro call. Catches direct and indirect recursion among
  user-defined macros.
- A runtime ancestor scan of the `Origin` chain, walking
  `Origin::CallerExpansion` entries. Catches pure caller-driven
  self recursion (`?UNKNOWN` → caller returns `?UNKNOWN` → …).

Not caught: mixed cycles that alternate through a user-defined
macro and a caller response, because `Origin::MacroBody` does not
carry the user macro's name. Extending `Origin::MacroBody` with the
`(name, arity)` pair would close this gap; that refactor is out of
scope for the current work.

Practical impact is small — such a cycle only forms if the caller
answers unknown macros with tokens that call the very user macro
whose body triggered the event, and does so recursively. Normal
Erlang tooling patterns never construct this.

### `fun bar/1` alone inside a macro argument

`fun` pushes the `FunEnd` sentinel; `fun bar/1, X` has the sentinel
draining on the top-level `,`, giving arity 2 with the first
argument as `fun bar/1`. Deeply-nested arity forms may drift from
OTP's exact `epp_dodger` behaviour. Real user code rarely writes an
arity form alone as a macro argument.

## Guarantees

Within the design decisions above, erl_pp guarantees:

- Constant-like and function-like macros expand, including nested
  function-like calls inside another macro's body (queued rescan).
- OTP-style argument parsing including the fun_end sentinel and
  all `end`-terminated keyword blocks.
- Middle empty arguments (`?FOO(A, , B)`) are valid arity-3
  groups; leading (`?FOO(, A)`) and trailing (`?FOO(A, )`) empties
  are `LeadingEmptyArgument` / `TrailingEmptyArgument` errors.
- `?FILE` returns the outermost source's display name as an
  Erlang string literal; `?LINE` returns the outermost line number
  as an integer literal.
- `??Param` returns a single Erlang string literal built from the
  argument's lexical tokens with single-space separators.
- Static and runtime cycle detection reject self-recursion and
  transitive recursion under the limits noted above.
- Every emitted token carries an `Origin` that traces back to
  `Origin::Source` or `Origin::Include`, letting callers reason
  about provenance for diagnostics.

Anything outside these bullets — including the corner cases above —
is either explicitly listed here or is a bug and should be reported.
