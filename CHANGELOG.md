# Changelog

## 0.3.1

### Fixes

* Forbid patterns no longer match lifetimes. A lifetime (`'static`, `'a`,
  a label `'outer`) is its own token: the pattern `static` catches `static`
  items (`static X`, `static mut`, `pub static`, inside `thread_local!` and
  other macro bodies, in `verus!`) but not `&'static str`, `T: 'static` or
  `impl Trait + 'static`. A pattern word written with a leading `'`
  (`'static`) matches the lifetime and nothing else.
* Raw identifiers no longer bypass rules. Every name is compared with its
  `r#` prefix removed: forbid patterns (`r#include!("x")` matches
  `include`, `r#Justifies` matches `Justifies`), forbid attributes, module
  rules (`crate::r#sibling::f` and `use crate::{r#sibling}` are the sibling;
  `mod r#type;` is module `type`), crate names in paths for `public_modules`
  and `deny_reexport`, authority type and call names, method names,
  `#[r#cfg(test)]` skipping, `#[r#path]` module mapping and `fn r#main`.
  Names in `strata.toml` (patterns, attributes, module paths) may be written
  either way. Keywords are still matched only unprefixed (`r#unsafe { .. }`
  is not an unsafe block).

### Testing

* `tests/raw_identifiers.rs`: one case per rule kind with raw spellings.
* `tests/lifetimes.rs`: `static` items against `'static` lifetimes, lifetime
  pattern words, end to end.

## 0.3.0

### Rules

* `attributes` on `[[forbid]]`: attribute names (`path`, `no_mangle`,
  `verifier::external_body`) allowed only in the `allow` files, with the same
  `crates`, `dirs`, `require_comment` and exemptions as `patterns`. Matches
  outer and inner attributes anywhere, inside `cfg_attr(..)` at any depth,
  behind `unsafe(..)`, in macro bodies and in `verus!`; names in arguments,
  `cfg_attr` predicates, comments and strings do not match. Replaces banning
  the identifier `path` as a stand-in for `#[path = ..]`.
* `[[forbid.exempt]]` takes `attribute = ".."` as an alternative to
  `pattern = ".."`.

### Breaking

* `patterns` on `[[forbid]]` is optional; a forbid with neither `patterns`
  nor `attributes` is a configuration error naming its line.
* A `[[forbid.exempt]]` must set exactly one of `pattern` and `attribute`,
  and it must name one of its forbid's own `patterns` or `attributes`; before,
  an exemption of an unlisted pattern was reported as stale (exit 1), now it
  is a configuration error naming its line (exit 2). Migration: delete the
  exemption, or add the pattern to the forbid.
* An `attributes` entry that is not a path (`"#[path]"`, `"link = .."`) is a
  configuration error naming its line.

### API

* `facts::attribute_hits` and `facts::lexical_hits` (patterns and attributes
  in one pass). `Forbid` gains `attributes`, `FileInfo` gains `attr_hits`,
  and `ForbidExempt::pattern` becomes `Option<String>` beside a new
  `attribute` (code that builds these structs literally must add the fields).

### Testing

* `tests/prop_attributes.rs`: generated attributes in every position against
  generated truth, and forbid verdicts against an oracle. Mutating the
  `cfg_attr` predicate skip, `unsafe(..)` transparency, raw-identifier
  handling, inner attributes or the exemption bookkeeping fails it.

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
