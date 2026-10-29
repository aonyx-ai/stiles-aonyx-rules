<!-- markdownlint-disable-file MD024 -->

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2026-10-29

### Changed

- The rules build against stiles 0.1.0 from crates.io. The prebuilt rules need plugin ABI 0.6 ([#106])
- Every rule reports its findings as errors, so a project that adopts this release fails `stiles check` on any finding ([#107])

## [0.1.0] - 2026-10-28

### Changed

- The rules build against stiles, the new name of whisker, and load into stiles only ([#103])

## [0.1.0-rc.4] - 2026-10-28

### Added

- `lint.doc-summary-period` reports a doc summary that ends with a period
- `lint.doc-summary-person` reports a function summary that does not open
  with a third-person verb
- `lint.outer-module-doc` reports a doc comment on a `mod foo;` declaration
- `lint.open-vocabulary` reports a catch-all variant such as `Other(String)`,
  and a variant or public field that holds anything
- `lint.error-enum-shape` reports an error enum that breaks the typed-error
  shape
- `lint.pub-anyhow-result` reports a public function whose error type is
  `anyhow::Error`
- `lint.external-type-in-port` reports a method of a `pub trait` that names a
  type from an external crate
- `lint.multi-param-constructor` reports a constructor that takes two or more
  positional parameters
- `lint.io-in-pure-function` reports I/O inside a function such as `new`,
  `from_str`, or `parse`
- `lint.terminal-output` reports `println!` and its kin, `stdout()`, and
  `std::process::exit` outside `main.rs`, build scripts, and tests
- `lint.unprefixed-submodule-path` reports a path into a submodule without
  `self::`
- `lint.facade-bypass-import` reports an import that goes around a facade
  crate
- `lint.multi-assert-test` reports a test that makes more than one assertion
- `lint.unseparated-test-body` reports a test body of three or more statements
  without a blank line
- `lint.unsorted-test` reports a test function that is out of byte order in
  its module

### Changed

- `lint.matches-macro` asks for a `match` that carries the branch when
  `matches!` is a condition
- The rules load into whisker 0.1.0-rc.5 and no other release, at plugin ABI
  0.5

## [0.1.0-rc.3] - 2026-09-08

### Added

- `lint.repeated-primitive-params` and `lint.bool-param` read the
  `boundary-attributes` option, and both skip a signature that a caller
  outside Rust fixes

### Changed

- `lint.bool-param` skips an `extern` signature, and it reports a `bool` on a
  trait method declaration
- `lint.split-binding` reports a binding only when the second binding
  transforms the first
- The rules load into whisker 0.1.0-rc.4 and no other release, at plugin
  protocol 4

## [0.1.0-rc.2] - 2026-09-04

### Added

- Every rule declares its name, so a project on whisker 0.1.0-rc.3 can run a
  subset of the rules

## [0.1.0-rc.1] - 2026-09-02

### Added

- Each release carries prebuilt archives of the rules for whisker
  0.1.0-rc.1, named for the commit and the ABI tag of whisker

[0.1.0-rc.4]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/v0.1.0-rc.4
[0.1.0-rc.3]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/v0.1.0-rc.3
[0.1.0-rc.2]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/v0.1.0-rc.2
[0.1.0-rc.1]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/v0.1.0-rc.1
[0.1.0]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/stiles-aonyx-rules@0.1.0
[#103]: https://github.com/aonyx-ai/stiles-aonyx-rules/pull/103
[0.2.0]: https://github.com/aonyx-ai/stiles-aonyx-rules/releases/tag/stiles-aonyx-rules@0.2.0
[#106]: https://github.com/aonyx-ai/stiles-aonyx-rules/pull/106
[#107]: https://github.com/aonyx-ai/stiles-aonyx-rules/pull/107
