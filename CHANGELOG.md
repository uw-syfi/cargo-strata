# Changelog

## 0.2.0

### Rules

* `public_modules` (with `public_modules_for`) on a `[[crate]]`: other
  workspace crates may name this crate's items only through the crate root or
  the listed modules (`error[surface]`). Replaces ad hoc checks that a facade
  module is the deepest path a dependent may write.
* `exempt` on `[[crate.module]]`: modules a module may reference although
  `deny` or `depends_on` rejects them. An entry that suppresses no reference is
  reported as stale, with its config line.
* `require_layer` and `require_group` accept a list of crate-name globs.
* `[crate.main] items`: a binary root may hold only `fn main`, inner
  attributes and the listed top-level items.

### Scanning

* Item-position macro invocations whose body parses as items
  (`cfg_rt! { pub mod runtime; }`, as in tokio) are read as if the items were
  written in place: `mod` declarations, inline modules, `use` items and paths,
  including `verus!` blocks inside them, nested up to 8 deep. tokio's module
  rules in the corpus go from 129 to 524.
* A file the Rust parser rejects (nightly-only syntax) is skipped with a
  warning naming its line and column instead of ending the run with exit 2.
  `cargo strata check --strict` restores the error. Size and nesting limits and
  read errors are still errors.

### API

* `run_with(manifest, config, strict)`; `modules::scan_crate`; `surface`
  module. `ModuleRule` gains `exempt`, `ModViolation` gains `stale`, `Scan`
  gains `macros`, and `CrateRule` gains `public_modules` and
  `public_modules_for` (code that builds these structs literally must add the
  fields).

### Testing

* `tests/prop_surface.rs` (paths in eight spellings, plain and in `verus!`,
  module reached known by construction) and `tests/prop_exempt.rs` (oracle
  from the definition, violations and stale entries). Mutating the matcher or
  the used-entry bookkeeping fails each.
* `tests/prop_modules.rs` wraps random module declarations in nested item
  macros.

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
