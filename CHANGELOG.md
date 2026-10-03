# Changelog

## 0.1.0

First release.

### Rules

* Crate rules from `cargo metadata`: layers and groups (ordered, or an exact
  `may_depend_on` matrix), per-crate `allow`, `allow_dev`, `deny`,
  `deny_reach`, external dependency allow and deny lists, placement
  (`dir`), directory reach (`deny_reach_dir`, `deny_source_dir`), and
  grandfathered exemptions that report themselves when stale.
* Module rules (`depends_on`, `deny`) from `use` trees and `crate::`,
  `self::`, `super::` paths, with line numbers.
* Source lints: construction authority for a type, re-export limits, `unsafe`
  ratchet or `forbid`, `fn main` call lists, token patterns confined to files,
  `verus_only` crates, `#[path]` and `include!` reach.
* Name resolution inside a crate (`resolve`, on by default): re-exports,
  globs, renames, type aliases and `Self` are followed before module rules,
  authority and re-export lints.

### Verus

* The optional `verus` feature parses `verus! { ... }` bodies with
  `verus_syn`, so module rules and lints see inside them. Without the feature
  `verus_syn` is not compiled.

### Tooling

* `cargo strata init` writes a config that describes the workspace as it is:
  an exact `allow` and `allow_dev` per crate and a `depends_on` per module.
* Configuration errors name the config line: unknown crate, layer, group,
  module or dependency names, overlapping layers or groups.
  `allow_unknown_names` relaxes this for configs shared by checkouts whose
  members differ.
* Exit codes: 0 clean, 1 violations, 2 configuration, metadata or parse errors.

### Testing

* Property tests with brute-force oracles for crate rules, module extraction
  and rules, source lints, name resolution, and directory rules, with and
  without the `verus` feature.
* Fuzz targets (`scan`, `config`, `facts`, `resolve`) under `fuzz/`.
* A corpus of eight public workspaces (verus, rust-analyzer, tokio, serde,
  ripgrep, cargo, clap, wasmtime) pinned by commit: a generated config must
  check clean, and each injected crate or module edge must be reported at the
  right file and line. Wall time is about 22 microseconds per source line
  (wasmtime, 799k lines: 17.8 s on a 64-core host).
* Differential runs against an existing regex-based checker on a large Verus
  workspace agreed on every injected case.

### Known limits

* Nightly-only syntax that `syn` rejects (`gen` blocks, default field
  values) is a parse error for that file.
* Modules declared inside macro invocations (for example tokio's `cfg_*!`)
  are invisible.
* `cfg` is not evaluated except `#[cfg(test)]`.

See the README for the full list.
