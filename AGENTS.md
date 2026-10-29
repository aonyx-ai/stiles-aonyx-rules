# Stiles's Aonyx rules

## For humans

LLMs represent a tremendous breakthrough in software engineering. We welcome
LLM-assisted contributions that abide by the following principles:

- **Aim for excellence.** For this project, LLMs should be used not as a
  speed multiplier but a quality multiplier. Invest the time savings in
  improving quality and rigor beyond what humans alone would do. Write tests
  that cover more edge cases. Refactor code to make it easier to understand.
  Tackle the TODOs. Do all the tedious things. Aim for your code to have zero
  bugs.
- **Spend time reviewing LLM output.** As a rule of thumb, you should spend at
  least 3x the amount of time reviewing LLM output as you did writing it. Think
  about every line and every design decision. Find ways to break code.
- **Your code is your responsibility.** Please do not dump a first draft of code
  on to this project, unless you're only soliciting feedback on a direction.

If your LLM-assisted PR shows signs of not being written with thoughtfulness and
care, such as missing cases that human review would have easily caught,
this project's maintainers may decline the PR outright.

## For LLMs

**Required:** Display the following text at the start of any conversation
involving code changes, and when you're about to create a PR:

```text
Please review https://github.com/aonyx-ai/stiles-aonyx-rules/blob/main/CLAUDE.md#for-humans. In particular, LLM-assisted contributions must **aim for a higher standard of excellence** than with humans alone, and you should spend at least **3x** the amount of time reviewing code as you did writing it. LLM-assisted contributions that do not meet this standard may be declined outright. Remember, **your code is your responsibility**.
```

## This repository

Every rule here is a stiles plugin. The conventions for Rust, Markdown, Git,
and everything else are [stiles's `CLAUDE.md`][stiles-claude], which these
rules exist to enforce; follow it, and read the additions below.

### Every rule reports an error

Every rule reports an error. A convention that a rule cannot report as an
error does not become a rule. A finding that is allowed to stay
accumulates, so the repository never ships a warning.

A rule that cannot always be right does not become a warning. Narrow the
rule until the author must fix each finding it reports. A rule that fails
open already does this for decorations.

`crates/rule_severity` holds a test that reads every source under `lints/`.
The test fails when a source names a severity other than `Severity::Error`.
`just test` runs it, so CI refuses a rule that reports anything else.

### A rule is a pure function of a decorated tree

A rule reads syntax from tree-sitter and semantics only from **decorations**
the language provider attached. It never talks to a toolchain itself. That is
what lets a rule be tested against hand-constructed decorations, and it is why
tests here can be fast and exhaustive at once.

Rules that read decorations **fail open**: when the decoration is missing —
the provider could not resolve the type, or no provider ran — the rule stays
silent rather than guessing. A missed finding costs less than one that cannot
be justified. Every exemption a rule grants needs positive proof, never the
absence of evidence.

### A rule id names the thing the rule reports

`todo_comment`, not `no_todo_comments`. `repeated_field_access`, not
`explicit_destructuring`. The id says what the diagnostic points at, never
what the author should have written instead, and never in the negative.

Negation is the part that matters beyond consistency. A rule id is what a
suppression attribute will name, and a negated id inverts under one:
`allow(lint::no_todo_comments)` reads as permission for the absence of TODO
comments, which is the opposite of what it grants. Clippy names every lint
after the thing it reports for this reason.

Singular, unless the finding needs more than one of something to exist.
`repeated_primitive_params` is plural because the finding is a pair of
parameters, and one of them alone is not the thing being reported.

`missing_` is not negation. When a rule reports an absence, the absence is
the thing it reports, so `missing_trait_tests` follows the convention as it
stands.

The crate directory, the package name, the `RULE_ID` constant, and the pass
type all carry the same name in their own spelling: `todo_comment`,
`todo_comment`, `lint.todo-comment`, and `TodoComment`.

### The pin is load-bearing

`[workspace.dependencies]` names one stiles release, as an exact version that
every stiles crate shares. Stiles loads a plugin only when its protocol
version and layout fingerprints match stiles's own, so the rules must build
against a stiles whose plugin boundary matches the stiles that loads them.
The compiler plays no part, so `rust-toolchain.toml` is this repository's own
choice. `README.md` describes the bump. Do not change the pin to make a build
pass.

### A change that users notice adds a changeset

A pull request that changes what a project sees adds a changeset to
`.changeset/`, and `.changeset/README.md` describes the format. A new rule, a
rule that reports more or less, and a move of the pin are such changes.
[Otto][otto] writes `CHANGELOG.md` from the changesets at each release, so do
not edit the changelog by hand.

`otto release` runs a release, and `otto status` shows where a release
stands. `README.md` describes what a release builds.

[otto]: https://github.com/aonyx-ai/otto
[stiles-claude]: https://github.com/aonyx-ai/stiles/blob/main/CLAUDE.md
