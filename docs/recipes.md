# Recipes

Short task-oriented recipes for building an OTP-style preprocessor
driver on top of erl_pp. Each recipe follows the same five-part shape:

- **Goal.** What the driver is trying to accomplish.
- **Uses.** The public API called by the recipe.
- **Code.** A minimal, self-contained example. Every code block is
  either a doctest verified by `cargo test --doc` or a link to a
  runnable example in `examples/`.
- **Notes.** Pitfalls, trade-offs, and things worth knowing before
  applying the recipe to a real driver.
- **See also.** The API rustdoc entries and companion docs that go
  deeper.

The skeleton driver loop, compiler vs formatter policies, and
responsibility split live in the crate-level rustdoc. These recipes
are copy-paste work units, not a tutorial.

## Contents

- [Seed environment macros with a leading `-define` source](#seed-environment-macros-with-a-leading-define-source)

## Seed environment macros with a leading `-define` source

**Goal.** Put compile-time constants such as `?MACHINE` and
`?OTP_RELEASE` into the macro table before the main source is scanned,
so they behave like OTP predefined macros in the caller's environment
instead of firing [`Event::AwaitingMacroExpansion`](crate::Event::AwaitingMacroExpansion)
on every use.

**Uses.** [`Source::from_text`](crate::Source::from_text),
[`Preprocessor::new`](crate::Preprocessor::new),
[`Event::MacroDefined`](crate::Event::MacroDefined),
[`Event::AwaitingConditional`](crate::Event::AwaitingConditional),
[`Preprocessor::resume_conditional`](crate::Preprocessor::resume_conditional).

```rust
# fn main() -> Result<(), Box<dyn std::error::Error>> {
let seed_text = concat!(
    "-define(MACHINE, \"beam\").\n",
    "-define(OTP_RELEASE, \"29\").\n",
);
let seed = erl_pp::Source::from_text("<seed>", seed_text)?;
let main = erl_pp::Source::from_text(
    "module.erl",
    "-ifdef(MACHINE).\n?MACHINE.\n-endif.\n",
)?;
let mut pp = erl_pp::Preprocessor::new([seed, main]);

let mut defines = 0usize;
let mut saw_machine = false;
loop {
    match pp.step()? {
        erl_pp::Event::MacroDefined(_) => defines += 1,
        erl_pp::Event::AwaitingConditional(cond) => {
            let branch = match cond {
                erl_pp::Conditional::Ifdef(d) => {
                    assert_eq!(d.recommended, erl_pp::Branch::Then);
                    d.recommended
                }
                other => unreachable!("unexpected conditional: {other:?}"),
            };
            pp.resume_conditional(branch)?;
        }
        erl_pp::Event::Token(t) => {
            if t.text() == "\"beam\"" {
                saw_machine = true;
            }
        }
        erl_pp::Event::BranchBoundary(_) => {}
        erl_pp::Event::Complete => break,
        other => unreachable!("unexpected event: {other:?}"),
    }
}
assert_eq!(defines, 2);
assert!(saw_machine);
# Ok(())
# }
```

**Notes.**

- [`Preprocessor::new`](crate::Preprocessor::new) scans its `Source`
  sequence from front to back. A leading seed `Source` whose only job
  is `-define(...)` forms is scanned like any other input: each
  directive becomes [`Event::MacroDefined`](crate::Event::MacroDefined)
  and updates the shared macro table before the main file starts.
- Once a name is in the table, uses such as `?MACHINE` expand from the
  stored replacement and do not reach
  [`Event::AwaitingMacroExpansion`](crate::Event::AwaitingMacroExpansion).
  `-ifdef(MACHINE)` / `-ifndef(MACHINE)` consult the table only;
  [`DefinedConditional::recommended`](crate::DefinedConditional::recommended)
  matches that lookup.
- **`?MODULE`**, **`?FUNCTION_NAME`**, **`?FUNCTION_ARITY`**, and
  **`?FEATURE_*`** are poor fits for a seed source: their values change
  with the module or call site (or need arguments erl_pp does not
  synthesize). Leave them on the
  [`Event::AwaitingMacroExpansion`](crate::Event::AwaitingMacroExpansion)
  path and implement them in the driver when needed.
- Do **not** seed **`?FILE`** or **`?LINE`**. A matching `-define`
  shadows erl_pp's internal evaluation for those names. `-ifdef(FILE)`
  still looks at the macro table only, not at the internal predefined,
  so a seed `-define(FILE, ...)` can desynchronise conditional
  recommendations from the file-name expansion callers expect elsewhere.

**See also.**

[`Preprocessor::new`](crate::Preprocessor::new),
[`docs::otp_differences`](crate::docs::otp_differences) (predefined
macro policy),
[Erlang Reference Manual -- Preprocessor](https://www.erlang.org/doc/system/macros.html).
