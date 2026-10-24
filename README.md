# whisker-aonyx-rules

Aonyx's lint rules for [whisker][whisker]. These are the rules that enforce
the conventions in Aonyx's `CLAUDE.md` files: the ones Clippy does not
cover, like derive ordering, wildcard match arms, and imports written inside
function bodies.

Whisker ships no rules of its own. A project that wants these runs them by
naming this repository and a commit:

```toml
# .config/whisker.toml
[[lints]]
git = "https://github.com/aonyx-ai/whisker-aonyx-rules"
rev = "0123456789abcdef0123456789abcdef01234567"
```

Whisker fetches that commit, builds every rule in it, and loads them. The
revision is written out in full and no branch or tag is accepted, so a check
runs the same rules today and next month.

## The rules

Each directory under `lints/` is one rule, and each documents itself. The
rule ids they report under are all `lint.<name>`.

## Options

A rule that reads options says so in its own documentation. A project sets
them under the rule that reads them:

```toml
[rules.options."lint.repeated-primitive-params"]
boundary-attributes = ["shard", "procedure"]
```

`lint.repeated-primitive-params` and `lint.bool-param` both read
`boundary-attributes`. It names the attribute macros that generate a bridge
for a caller outside Rust, such as topcoat's `#[shard]` and `#[procedure]`.
Such a macro fixes a signature the way `extern` does, so both rules skip the
function rather than ask for types the caller cannot send. A project that
sets one usually sets both.

Both rules already skip an `extern` signature, with no option set. They
answer this question in one place, `crates/boundary`, because a rule that
exempted a function the other reported would read as a bug in whichever one
spoke.

## Pinning

A rule is a `cdylib` that whisker loads into its own process. Rust has no
stable ABI of its own, so whisker lays every type that crosses that boundary
out with stabby and loads a plugin regardless of which rustc built it. It
still refuses a plugin built against another whisker revision. Two things
therefore move together:

- The `rev` that the whisker dependencies in this repository's `Cargo.toml`
  share, which every rule builds against. `just pin` prints it.
- The `rev` that a project names in its own `.config/whisker.toml`, which
  should be a commit of this repository built against the whisker that
  project runs.

`rust-toolchain.toml` is this repository's own choice.

To bump: move every whisker `rev` in `[workspace.dependencies]` to the new
whisker commit, run `cargo update -w`, and let CI's dogfood job confirm
the pair still loads. A mismatch is a refusal with an error naming what to
rebuild, never a silent wrong answer.

## Adding a rule

Copy the smallest existing rule, `lints/todo_comment`, and change the
name in its `Cargo.toml`, its rule id, and its logic. A rule implements the
generated `RustLintPass` trait, returns diagnostics with a stable rule id
and a severity, and hands itself to `whisker_plugin::export_lints!`. Rules that read
semantic decorations **fail open**: when the decoration is missing, the rule
stays silent rather than guessing.

Every rule has tests beside it, and `just test` runs all of them. `just
check-self <path-to-whisker>` checks this repository with the rules in it.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE)
  or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT)
  or <http://opensource.org/licenses/MIT>)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

[whisker]: https://github.com/aonyx-ai/whisker
