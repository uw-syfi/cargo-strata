# cargo-strata

cargo-strata enforces dependency rules in a Cargo workspace: which crates may
depend on which, and which modules inside a crate may reference which. It reads
`cargo metadata` and the source with `syn`, and optionally parses Verus code.

## Getting started

### Install

```sh
cargo install cargo-strata                    # from crates.io
cargo install cargo-strata --features verus   # also parses verus! { } bodies
cargo install --git https://github.com/uw-syfi/cargo-strata   # unreleased
```

### First run

From the workspace root, write a config that describes the workspace as it is
today, then check it:

```sh
cargo strata init --output strata.toml
cargo strata check
```

```
strata: ok
```

Without `--output`, `init` prints the config to stdout. It records, per crate, an exact `allow` list of its workspace dependencies
and, per module, a `depends_on` list of the modules it references. The first
check is therefore clean, and any new edge is reported. Exit code: 0 clean, 1
violations, 2 configuration, metadata or parse errors.

### Tighten a rule

Take a workspace of three crates: `core`, `plugin` and `app`, where `plugin`
and `app` both depend on `core`. Add layers to `strata.toml`:

```toml
[[layer]]
name = "core"
crates = ["core"]
may_depend_on = []

[[layer]]
name = "plugin"
crates = ["plugin"]
may_depend_on = ["core"]

[[layer]]
name = "app"
crates = ["app"]
may_depend_on = ["core"]   # the app loads plugins by name, never links them
```

Now add `plugin = { path = "../plugin" }` under `[dependencies]` in
`app/Cargo.toml`:

```
$ cargo strata check
error[crate]: app/Cargo.toml:8: `app` -> `plugin` (normal dependency): layer `app` may depend on `core` only, not `plugin`
error[crate]: app/Cargo.toml:8: `app` -> `plugin` (normal dependency): not in the `allow` list of `app`
2 violation(s)
```

Each line names the manifest and line of the offending dependency. Remove the
dependency, or change the rule on purpose (the `allow` list and the layer's
`may_depend_on`), and the check passes again.

### Module rules

Rules inside a crate sit under a `[[crate]]` entry. This one says the `memory`
module of `core` must never reference `engine`:

```toml
[[crate]]
name = "core"

[[crate.module]]
path = "memory"
deny = ["engine"]
```

If `core/src/memory.rs` calls `crate::engine::step()`:

```
error[module]: core/src/memory.rs:2: module `memory` references `engine` (via `crate::engine::step`): `memory` denies `engine`
1 violation(s)
```

`depends_on = ["util"]` is the allowlist form: the module may reference only
itself and the listed modules. References are followed through `use` trees,
re-exports, globs and renames.

### CI

```yaml
- uses: dtolnay/rust-toolchain@stable
- run: cargo install cargo-strata --version 0.1.0 --locked
- run: cargo strata check
```

A violation exits 1 and fails the job.

### Verus workspaces

Install with `--features verus` when your crates keep code inside
`verus! { ... }`. Plain `syn` cannot parse those bodies, so without the
feature module rules, the unsafe counts and the source lints do not see
inside them. Set `verus = true` on a `[[crate]]` entry to parse that crate's
`verus!` bodies; without the feature the flag only produces a warning.

### More

The rest of this file is the reference for every rule and config key.

## Rules

Three kinds of rule, all from one `strata.toml` at the workspace root:

1. **Crate rules**: which workspace crates may depend on which (layers,
   groups, allow and deny lists), read from `cargo metadata`. Directory rules
   sit beside them: where crates live, and which directories their
   dependencies may reach.
2. **Module rules**: which modules inside a crate may reference which, read
   from `use` trees and `crate::`/`self::`/`super::` paths in the source,
   with names resolved through re-exports, globs and renames.
3. **Source lints**: who may construct a type, what a crate may re-export,
   unsafe budgets, `fn main` call lists, token patterns confined to files,
   `#[path]`/`include!` reach. Same parser, same `verus!` handling.

```sh
cargo strata check [--manifest-path Cargo.toml] [--config strata.toml]
cargo strata init [--manifest-path ..] [--output strata.toml] [--verus]
```

Other output lines:

```
error[module]: src/memory.rs:4: module `memory` references `engine` (via `crate::engine::step`): `memory` does not list it in depends_on
error[authority]: app/src/lib.rs:10: `Permit { .. }`: only `gate/src/lib.rs` may construct Permit
```

Without the `verus` feature, `verus_syn` is not a dependency: nothing extra is
compiled, and nothing extra runs.

## Config reference

All tables reject unknown keys.

### Top level

| Key | Default | Meaning |
| --- | --- | --- |
| `check_dev` | false | Check dev-dependency edges |
| `check_build` | true | Check build-dependency edges |
| `check_optional` | true | Check optional (feature-gated) dependencies. `cargo metadata`'s resolve graph omits optional deps whose feature is off; strata reads declared dependencies |
| `layer_dev` | false | Apply the layer axis to dev edges |
| `group_dev` | true | Apply the group axis to dev edges (when `check_dev`) |
| `require_layer`, `require_group` | false | `true`: every workspace crate must belong to a layer / group. A list of crate-name globs: every workspace crate matching one of them must (for a workspace that also holds crates the config does not govern) |
| `resolve` | true | Resolve names inside each crate (re-exports, globs, renames, type aliases, `Self`) before module rules and the authority and re-export lints; see [Name resolution](#name-resolution). `false` keeps the path-as-written behavior |
| `allow_unknown_names` | false | Without it, a name that matches nothing is a configuration error with its config line: a `[[crate]]` name, a layer/group `crates` glob, a literal in `allow`/`allow_dev`, a module rule `path`, and `depends_on`/`deny` entries. Also errors: a crate in two layers (or two groups), an unknown layer in `may_depend_on`, a duplicate layer name. Set it for one config shared by checkouts whose members differ |
| `skip_cfg_test` | true | Ignore `#[cfg(test)]` modules, fns and uses in module checks |
| `lint_skip_cfg_test` | false | Source lints skip `#[cfg(test)]` items (by default they see test code) |
| `unsafe_ratchet` | none | JSON file (workspace-relative) `{crate: allowed unsafe sites}` for `unsafe = "ratchet"` |

### `[[layer]]` and `[[group]]`

Two independent axes with the same shape. A group is a second partition (for
example framework/plugin/deploy) checked the same way as layers.

| Key | Meaning |
| --- | --- |
| `name`, `crates` | Layer name; crate-name globs (`*`, `?`, `[..]`) |
| `may_depend_on` | Exact list of layer names this layer may depend on (own layer is not implicit). When absent, layers are ordered lowest first and a crate may depend on its own and lower layers |
| `deny`, `deny_normal` | Members may not depend on crates matching these globs (`deny_normal`: normal edges only) |
| `external_allow` | Normal deps on non-workspace crates must match one of these globs (`[]` forbids all) |
| `external_deny` | Normal deps on non-workspace crates matching these are violations |
| `dir` | Directory (workspace-relative) every member crate of this layer/group must live in or under (`error[placement]`) |

Only edges between two crates that both belong to the axis are compared.
Ordered layers cannot express a matrix (a mediator may use a proposer, a
driver may not use a mediator); use `may_depend_on` for that.

### `[[crate]]`

`name` is a crate-name glob. Several `[[crate]]` entries may match one crate;
their rules combine.

| Key | Meaning |
| --- | --- |
| `allow` | Exact allowlist (globs) of workspace crates reachable through normal and build edges. Unlisted workspace deps are violations. Absent: no list |
| `allow_dev` | Additional workspace crates allowed through dev edges |
| `deny`, `deny_normal` | Dependencies (any crate, workspace or not) that are always violations |
| `deny_reach` | Crates (globs) that must not be reachable through normal edges between workspace crates |
| `layer_exempt`, `group_exempt` | Grandfathered dependencies exempt from that axis. An exemption that suppresses nothing is reported as stale |
| `verus` | Parse `verus!` bodies for this crate's module rules and source lints. Honored only when built with `--features verus`; otherwise one warning is printed |
| `deny_reach_dir` | Directory globs (workspace-relative; a glob covers everything below the directory it matches). No package living there may be reachable from this crate through dependencies (kinds per `check_dev`/`check_build`/`check_optional`), including path dependencies outside the workspace. The report names the chain (`error[reach]`) |
| `deny_source_dir` | Directory globs this crate's sources may not pull in with `#[path = ".."]` or `include!`/`include_str!`/`include_bytes!` (literal or `concat!(env!("CARGO_MANIFEST_DIR"), ..)` arguments; paths are normalized lexically; dynamic arguments are ignored) (`error[source-reach]`) |
| `deny_reexport`, `allow_reexport` | A `pub use` (any `pub` visibility) whose path starts with a crate in `deny_reexport` (`-` and `_` equal) may only name leaves listed in `allow_reexport`; a glob always fails; `allow_reexport = ["*"]` lifts the rule (`error[reexport]`). The leaf is the original name, so `X as Y` is checked as `X`. With `resolve`, a path that reaches a denied crate through a local alias (`use denied as d; pub use d::X`), a module that re-exports it, or a workspace dependency that re-exports it is checked the same way |
| `unsafe` | `"ratchet"`: count `unsafe` blocks, fns, impls, traits and extern blocks in `src/`; it must equal the `unsafe_ratchet` entry (more: justify and raise; fewer: lower it; no entry and stale entries are errors). `"forbid"`: every lib and bin root carries `#![forbid(unsafe_code)]` (`error[unsafe]`) |
| `verus_only` | Every item in `src/` must be a `use`, `mod`, `extern crate` or inside `verus! { }` (`error[verus-only]`) |
| `[crate.main]` | `calls`, `methods`, `entries`: `fn main` in this crate's binaries may call only these paths (segments joined by `::`, bare names allowed), these methods, and the entries; at least one entry call in total, each at most once (`error[main]`). `items` (globs over `kind name`, such as `mod neg`, `fn helper`, `use`, `impl`): when present, each binary root may hold only `fn main`, inner attributes and top-level items matching one of them |
| `public_modules`, `public_modules_for` | Public surface: a path from another workspace crate into this one may reach only the crate root or these modules (exact, not their submodules); anything else is `error[surface]`. `public_modules_for` (crate-name globs) limits it to those dependents, default every dependent. See [Public surface](#public-surface) |
| `[[crate.module]]` | Module rules, below |

### `[[crate.module]]`

| Key | Meaning |
| --- | --- |
| `path` | Module path from the crate root, `a::b` (a leading `crate::` is allowed) |
| `depends_on` | If present, the module (and its submodules, unless a more specific rule exists) may reference only itself and these modules (prefix match) |
| `deny` | Modules it must never reference |
| `exempt` | Modules it may reference although `deny` or `depends_on` rejects them (a grandfathered edge). An entry that suppresses no reference is reported as stale (`error[module]` with the config line); an unknown name is a configuration error |

Resolution: a reference's target is the longest prefix of its path that names
a known module; the source is the longest configured rule that is a prefix of
the file's module path. References into the source rule's own subtree are
always allowed. References to items at the crate root are ignored. A rule that
names no module produces a warning. With `resolve`, a reference is also
attributed to the module that defines what it names (below); `deny` applies to
both the module as written and the defining module, `depends_on` to the module
as written only, so reaching an allowed facade module that re-exports a
disallowed module's items is allowed.

Example (see `tests/fixtures`):

```toml
[[layer]]
name = "base"
crates = ["low"]
[[layer]]
name = "top"
crates = ["high"]

[[crate]]
name = "app"
verus = true

[[crate.module]]
path = "memory"
depends_on = ["util"]
deny = ["engine"]
```

### Public surface

`public_modules` on a crate limits how its dependents name its items. For each
path in a dependent's source that starts with this crate's name (`use` leaves,
globs, expression, type and pattern paths, `verus!` bodies), the module is the
longest prefix of the rest of the path that names a module of this crate. The
path is allowed if that module is the crate root or is listed exactly; items
after the module are not checked.

```toml
[[crate]]
name = "core"
public_modules = ["memory", "memory::api"]   # `core::memory::api::Slot`, not `core::memory::slab::Slot`
public_modules_for = ["app-*"]               # only these dependents
```

```
error[surface]: app/src/lib.rs:7: `app` names `memory::slab` of `core` (via `core::memory::slab::Slot`), which is not in its public_modules
```

Limits: names are matched as the package name with `-` as `_` (a `[lib] name`
or a dependency rename is not followed); the namespace-blind module test means
`use core::memory::slab;` is a module path even when `slab` is also an item;
paths inside macros other than `verus!` and item macros whose body parses as
items are seen only when they start with `crate`, `self` or `super`; only the crates' `mod` trees are scanned, not
tests or examples. An unknown name in `public_modules` is a configuration
error.

### `[[authority]]`

Only listed files may construct a type (or call a function). Scans every `.rs`
file under each matching crate's directory (`dirs`, default `["."]`). The type
and function are matched by name; with `resolve`, a path also matches when it
resolves to that name through an `as` rename, a `type` alias or `Self`.

| Key | Meaning |
| --- | --- |
| `name` | Label in messages |
| `type`, `literal` | A struct literal `Type { .. }` (path-qualified allowed; last segment is compared). `literal` defaults to true |
| `assoc` | Associated functions: `Type::new(..)` counts as construction |
| `calls` | Free functions: any call whose path ends in this name counts (methods and `fn` definitions do not) |
| `allow` | File globs (workspace-relative) that may construct it |
| `crates` | Crate-name globs checked (default all) |
| `exempt_marker` | A construct on a line containing this text, or inside the innermost fn whose text contains it, is exempt (a negative control) |

Struct patterns, type definitions, comments and strings are not constructs.
Macro arguments (`vec![T { a: 1 }]`) are token-scanned and count.

### `[[forbid]]`

Token patterns allowed only in listed files. A lexical scan (it also sees
inside `verus!` without the feature).

| Key | Meaning |
| --- | --- |
| `name`, `patterns` | Patterns are space-separated words over consecutive tokens at one nesting level: an identifier, a path `a::b`, `name(` (a call, not preceded by `.` or `:`), or `name()` (empty call) |
| `allow` | File globs where the patterns may appear |
| `crates`, `dirs` | Crates (default all) and directories of each (default `["src"]`) |
| `require_comment` | In allowed files, each hit needs a line comment containing this tag on its line or in the comment/attribute block directly above |
| `[[forbid.exempt]]` | `file` and `pattern` of a tolerated hit outside `allow`; one that matches nothing is reported as stale |

## What module rules see

* `use` items: groups, renames, globs, `self` in groups.
* Any path starting with `crate`, `self` or `super` in types, expressions,
  patterns and signatures.
* In a `use`, a first segment that is a child module of the current module
  (`use memory::x;`).
* With `resolve`: any multi-segment path outside `use` whose first segment is
  a child module or arrives through a glob (`memory::f()`); a first segment
  bound by an explicit `use` is not reported again, the `use` already is.
* Token streams of other macro invocations (`vec![crate::a::B]`): scanned for
  `crate`/`self`/`super` chains, not parsed.
* With `verus`: `verus! { }` bodies parsed with `verus_syn`, so `use` items,
  fn bodies, `requires`/`ensures`/`recommends`, spec and proof fns, `assert ...
  by { }` are covered.

Files are mapped to modules from `mod x;` declarations (`x.rs`, `x/mod.rs`,
`#[path]`) starting at each lib, proc-macro and bin target root. Files not
reachable by `mod` declarations are not checked by module rules (source lints
scan them, each as a crate of its own).

## Name resolution

With `resolve = true` (the default) strata builds a table per crate from the
same parse as the rules (`verus!` bodies included with the feature): for each
module, the items it defines (structs, enums, unions, traits, fns, consts,
statics, `type` aliases to a plain path), its `use` bindings with renames,
groups, `self`, `as`, `extern crate ... as`, and its glob imports. A path is
resolved from the module it is written in:

* `crate::`, `self::`, `super::` are followed to the named module, then each
  segment is looked up: child module, definition, explicit import, then the
  module's glob imports (recursively). An import is followed to what it
  names, so `pub use crate::x::Y as Z` chains, glob chains and glob cycles
  end at the module that defines the item. A `type` alias is followed to its
  target. `Self` in an `impl` is replaced by the impl's self type.
* The result is a module, an item with its defining module, or a path that
  leaves the crate (`std`, extern crates, unknown names), reported as
  written after local aliases are substituted.
* Each query has a budget of 2,048 steps; a name defined only in terms of
  itself does not resolve, and a query that runs out of budget or hits a
  circular import reports nothing rather than a wrong module.
* Lints use it as follows: authority matches a struct literal, associated call
  or function call whose path resolves to the listed name, in addition to the
  spelled name; `deny_reexport` follows local aliases, modules that re-export
  (named items and globs) and, for workspace dependencies, the dependency's own
  table (up to 8 crates deep).
* Resolution only adds findings. Every violation `resolve = false` reports is
  still reported, and a resolved-only finding says so (`the name resolves to
  ...`, or `written ...` for lints).

Limits of resolution (all syntactic):

* No visibility, namespaces, traits, method calls, generics or macros. Every
  binding in a module is importable; a name with several bindings uses the
  first that resolves inside the crate. `pub` matters only when listing what
  a glob re-exports.
* Single identifiers brought in by a glob (`use crate::api::*; Thing::new()`)
  are attributed at the glob: the glob reports the modules that define what
  the target module publicly re-exports, not each later use of a name.
* Items declared inside fn bodies are not in the table; a `use` inside a fn is
  treated as a binding of the enclosing module. Items generated by a macro's
  expansion are invisible (items written in an item macro's body are seen);
  `cfg` is not evaluated beyond `#[cfg(test)]`.
* `depends_on` judges the path as written (see above). The `fn main` rule
  matches calls as written.
* Cross-crate: only `deny_reexport` follows a path into a workspace
  dependency, and only dependencies that source lints scanned (crates with a
  lint rule); a path into a crate outside the workspace is never resolved.
  Module rules and authority stay inside one crate (a re-export from another
  crate is the other crate's module rule, or an authority name written as is).
* A file outside the `mod` tree (tests, examples) resolves against its own
  imports only; `crate::` in it means the file itself.

## Limits

* With `resolve = false`: path-based, not name-resolved. A reference through
  a re-export, a type alias or a glob import of a module that itself
  re-exports is attributed to the path as written, and source lints do not see
  `use gate::Permit as P; P { .. }`, a type alias, `Self { .. }` inside
  `impl Permit` or a call through a renamed import.
* `use a::b` is read as a local module only if `a` is a child module of the
  current module; extern crates and prelude names are ignored.
* An item-position macro invocation other than `verus!`
  (`cfg_rt! { pub mod runtime; }`) is parsed as a list of items when its body
  is one, and read as if the items were written there: `mod` declarations,
  inline modules, `use` items and paths, nested up to 8 deep. Any other macro
  body is token-scanned: no `use` tree structure, so `crate::a::{b, c}`
  attributes to `crate::a`. Modules a macro generates by expanding its
  definition are invisible. Without the feature, `verus!` bodies are skipped
  entirely.
* `cfg` is not evaluated except `#[cfg(test)]` skipping. `mod x;` whose file
  is missing is a warning.
* Crate rules read declared dependencies (`cargo metadata` `packages`), so
  feature-gated and target-specific deps are included; edges are by package
  name, ignoring renames.
* Source files larger than 2 MiB, nested deeper than 256 brackets or generic
  arguments, or with more than 20,000 tokens between separators (`;` `,` `{`
  `}`) are reported as parse errors instead of risking a stack overflow in
  the parser.
* Without the `verus` feature, `verus!` bodies are skipped by the AST lints
  (unsafe counts and constructs there are invisible); `[[forbid]]` still sees
  them.
* `check_optional = false` drops an optional dependency only when cargo's
  resolve graph does not enable it, so a default feature that switches it on
  keeps the edge.

## Runtime on external workspaces

`tests/corpus.toml` pins eight workspaces (commit hashes only);
`tests/corpus/run.py` clones them (shallow), writes a config with
`cargo strata init`, requires a clean run, then removes one existing crate edge
and two module edges from the config and requires each to be reported with the
right file and line (`tests/corpus/inject.py`). Pinned runs, 64-core host,
`cargo strata check` wall time with the generated config (`resolve` on, verus
entry built with `--features verus`):

| Workspace | `.rs` files | LOC | Crates | Module rules | Wall |
| --- | --- | --- | --- | --- | --- |
| verus (`source/`) | 972 | 640k | 19 | 285 | 6.1 s |
| rust-analyzer | 1,485 | 587k | 44 | 817 | 7.7 s |
| tokio | 808 | 185k | 10 | 524 | 2.1 s |
| serde | 208 | 43k | 5 | 32 | 0.3 s |
| ripgrep | 110 | 56k | 11 | 75 | 0.7 s |
| cargo | 1,372 | 345k | 27 | 324 | 3.5 s |
| clap | 337 | 85k | 8 | 118 | 1.1 s |
| wasmtime | 2,088 | 799k | 98 | 1,267 | 17.8 s |

About 22 microseconds per line. Known gaps seen there: verus's forked rustc
crates use nightly-only syntax (`gen` blocks, default field values) that `syn`
rejects, so two files are `error[parse]` (exit 2) with their line and column;
tokio declares many modules inside `cfg_*!` macros; these are read as written
(129 module rules before that was supported, 524 now).

## Development

```sh
cargo test                  # fixtures under tests/fixtures
cargo test --features verus
cargo clippy --all-targets [--features verus]
```

Rule evaluation is pure: `crates::evaluate(config, workspace, edges)` and
`modules::evaluate(...)`, `dirs::evaluate(...)` and `lints::evaluate(...)` take data in and return violations, for direct
testing without `cargo metadata` or the file system.

## Testing

* `tests/prop_crates.rs`: crate rules against a brute-force oracle written from
  this document (equality, edge-removal and edge-addition monotonicity, order
  independence, ordered-layer transitivity), plus an end-to-end variant over
  real `cargo metadata` workspaces. `STRATA_CASES=n` sets the case count.
* `tests/prop_modules.rs`: generated module trees with known references
  (use groups, renames, globs, `self`/`super` chains, inline and file
  modules, `verus!` bodies, `cfg(test)`); extraction must match exactly, module
  rules must match an oracle. Run with and without `--features verus`.
* `tests/prop_main_items.rs`: generated binary roots (items with
  attributes and doc comments around `fn main`) against an oracle for
  `[crate.main] items`.
* `tests/prop_lints.rs`: generated files (fn bodies, impls, inline and
  `cfg(test)` modules, macro arguments, `verus!` blocks) with line-exact
  ground truth; fact extraction must match it, and authority, re-export and
  unsafe-ratchet verdicts must match an oracle. `tests/prop_dirs.rs`: reach
  and placement against a closure oracle; token patterns against generated
  occurrences. Run with and without `--features verus`.
* `tests/prop_resolve.rs`: generated crates with `pub use` chains (renames,
  groups, `crate::`/`self::`/`super::` forms) and glob imports including
  cycles. A forward-closure oracle gives each exported name's defining
  module; `Table::resolve` must agree for every spelling, `glob_sources` must
  equal the oracle, module rules with `resolve = false` must equal an
  as-written oracle and with `resolve = true` the as-written plus
  defining-module oracle (deny exact; `depends_on` unchanged; the
  `resolve = false` violations are a subset).
  `tests/prop_resolve_lints.rs`: authority through renames, aliases and
  `Self`, and `deny_reexport` through local aliases and relay modules, against
  ground truth by construction. `tests/resolve_cases.rs`: the cases the
  path-based version missed, end to end with `resolve` on and off, including a
  re-export through a workspace dependency.
* `tests/corpus_regressions.rs`: bugs found on the external workspaces,
  minimized (raw-identifier modules, use-tree line numbers, renamed
  dependencies). `tests/bad_config.rs`: each configuration error names its
  config line.
* `tests/deep_nesting.rs`: pathological nesting must be an error, not a crash.
* `fuzz/`: `cargo +nightly fuzz run scan`, `config`, `facts` and `resolve`.
