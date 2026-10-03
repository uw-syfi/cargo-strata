//! The cases the README listed as missed by path-based checking, each run end
//! to end through `run` with name resolution on and off.

use cargo_strata::{Outcome, run};
use std::path::Path;

/// A one-crate workspace `app` (plus workspace crate `gate` when asked) with
/// the given sources and strata.toml.
fn check(files: &[(&str, &str)], toml: &str) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n",
    );
    for (p, s) in files {
        write(root, &format!("app/{p}"), s);
    }
    write(root, "strata.toml", toml);
    run(Some(&root.join("Cargo.toml")), None).unwrap()
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn kind<'a>(o: &'a Outcome, k: &str) -> Vec<&'a String> {
    o.lines
        .iter()
        .filter(|l| l.starts_with(&format!("error[{k}]")))
        .collect()
}

const MODS: &str = r#"
[[crate]]
name = "app"
[[crate.module]]
path = "memory"
deny = ["engine"]
"#;

fn modules(resolve: bool) -> String {
    format!("resolve = {resolve}\n{MODS}")
}

const LIB: &str = "pub mod engine;\npub mod api;\npub mod memory;\n";
const ENGINE: &str = "pub struct Thing;\npub fn step() {}\n";

#[test]
fn use_through_a_reexport() {
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "pub use crate::engine::Thing;\n"),
        ("src/memory.rs", "use crate::api::Thing;\n"),
    ];
    assert!(kind(&check(&f, &modules(false)), "module").is_empty());
    let on = check(&f, &modules(true));
    let l = kind(&on, "module");
    assert_eq!(l.len(), 1, "{:?}", on.lines);
    assert!(
        l[0].contains("references `engine`") && l[0].contains("resolves to `engine`"),
        "{}",
        l[0]
    );
}

#[test]
fn glob_import_of_a_reexporting_module() {
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "pub use crate::engine::*;\n"),
        (
            "src/memory.rs",
            "use crate::api::*;\nfn f() { let _ = Thing; }\n",
        ),
    ];
    assert!(kind(&check(&f, &modules(false)), "module").is_empty());
    let on = check(&f, &modules(true));
    assert_eq!(kind(&on, "module").len(), 1, "{:?}", on.lines);
}

#[test]
fn glob_cycle_terminates_and_still_finds_the_definer() {
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        (
            "src/api.rs",
            "pub use crate::memory::*;\npub use crate::engine::*;\n",
        ),
        ("src/memory.rs", "pub use crate::api::*;\n"),
    ];
    let on = check(&f, &modules(true));
    // `memory` globs `api`, which globs `engine`: reached through the cycle.
    assert_eq!(kind(&on, "module").len(), 1, "{:?}", on.lines);
}

#[test]
fn type_alias_of_a_denied_type() {
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "pub type Alias = crate::engine::Thing;\n"),
        ("src/memory.rs", "use crate::api::Alias;\n"),
    ];
    assert!(kind(&check(&f, &modules(false)), "module").is_empty());
    assert_eq!(kind(&check(&f, &modules(true)), "module").len(), 1);
}

#[test]
fn renamed_module_is_reported_once() {
    // `e::step()` goes through the explicit import, which is already reported.
    let f = [
        ("src/lib.rs", "pub mod engine;\npub mod memory;\n"),
        ("src/engine.rs", ENGINE),
        (
            "src/memory.rs",
            "use crate::engine as e;\nfn f() { e::step(); }\n",
        ),
    ];
    assert_eq!(kind(&check(&f, &modules(false)), "module").len(), 1);
    assert_eq!(kind(&check(&f, &modules(true)), "module").len(), 1);
}

#[test]
fn sibling_reaches_child_module_by_relative_path() {
    let f = [
        ("src/lib.rs", "pub mod engine;\npub mod memory;\n"),
        ("src/engine.rs", ENGINE),
        // A bare path whose first segment arrives through a glob of the crate root.
        (
            "src/memory.rs",
            "use crate::*;\nfn f() { engine::step(); }\n",
        ),
    ];
    let off = check(&f, &modules(false));
    let on = check(&f, &modules(true));
    assert!(kind(&off, "module").is_empty(), "{:?}", off.lines);
    assert_eq!(kind(&on, "module").len(), 1, "{:?}", on.lines);
}

#[test]
fn depends_on_accepts_a_facade() {
    // memory may use `api`; api re-exports engine. Allowed: depends_on judges
    // the path as written. A deny of engine would not be.
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "pub use crate::engine::Thing;\n"),
        ("src/memory.rs", "use crate::api::Thing;\n"),
    ];
    let toml =
        "[[crate]]\nname = \"app\"\n[[crate.module]]\npath = \"memory\"\ndepends_on = [\"api\"]\n";
    assert!(kind(&check(&f, toml), "module").is_empty());
}

#[test]
fn resolve_false_is_the_old_behavior() {
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "pub use crate::engine::*;\n"),
        (
            "src/memory.rs",
            "use crate::api::*;\nuse crate::api::Thing;\n",
        ),
    ];
    assert!(kind(&check(&f, &modules(false)), "module").is_empty());
}

// ---------- authority ----------

fn authority(resolve: bool) -> String {
    format!(
        "resolve = {resolve}\n[[authority]]\nname = \"permit\"\ntype = \"Permit\"\nassoc = [\"new\"]\ncalls = [\"mint\"]\nallow = [\"app/src/gate.rs\"]\ncrates = [\"app\"]\n"
    )
}

const GATE: &str = "pub struct Permit {}\nimpl Permit { pub fn new() -> Permit { Permit {} } }\npub fn mint() {}\n";

fn authority_hits(user: &str, resolve: bool) -> usize {
    let f = [
        ("src/lib.rs", "pub mod gate;\npub mod user;\n"),
        ("src/gate.rs", GATE),
        ("src/user.rs", user),
    ];
    kind(&check(&f, &authority(resolve)), "authority").len()
}

#[test]
fn authority_rename_alias_self_and_call() {
    let cases: &[(&str, usize)] = &[
        (
            "use crate::gate::Permit as P;\nfn f() { let _ = P {}; }\n",
            1,
        ),
        (
            "use crate::gate::Permit as P;\nfn f() { let _ = P::new(); }\n",
            1,
        ),
        (
            "type Q = crate::gate::Permit;\nfn f() { let _ = Q {}; }\n",
            1,
        ),
        (
            "use crate::gate::Permit;\nimpl Permit { fn f() { let _ = Self {}; } }\n",
            1,
        ),
        (
            "use crate::gate::Permit;\nimpl Permit { fn f() { let _ = Self::new(); } }\n",
            1,
        ),
        ("use crate::gate::mint as m;\nfn f() { m(); }\n", 1),
        (
            "pub use crate::gate::Permit as R;\nfn f() { let _ = R {}; }\n",
            1,
        ),
        // A glob import keeps the spelled name, which was already seen.
        ("use crate::gate::*;\nfn f() { let _ = Permit {}; }\n", 1),
    ];
    for (src, hits_on) in cases {
        assert_eq!(authority_hits(src, true), *hits_on, "resolve=true\n{src}");
    }
    // Without resolution only the spelled ones are seen.
    let off: Vec<usize> = cases
        .iter()
        .map(|(s, _)| authority_hits(s, false))
        .collect();
    assert_eq!(off, vec![0, 0, 0, 0, 0, 0, 0, 1]);
}

#[test]
fn authority_does_not_flag_other_types() {
    let src = "pub use crate::gate::Permit as R;\nstruct Other {}\nuse self::Other as P;\nfn f() { let _ = P {}; let _ = Other {}; }\n";
    assert_eq!(authority_hits(src, true), 0);
}

// ---------- re-export lint ----------

#[test]
fn deny_reexport_through_alias_and_relay() {
    let toml = |r| {
        format!(
            "resolve = {r}\n[[crate]]\nname = \"app\"\ndeny_reexport = [\"eff\"]\nallow_reexport = [\"Allowed\"]\n"
        )
    };
    // `eff` is not a dependency here; only the paths matter to the lint.
    let f = [(
        "src/lib.rs",
        "use eff as e;\n\
         pub mod relay { pub use eff::Allowed; pub use eff::Bad as Hidden; }\n\
         pub use e::Bad;\n\
         pub use e::Allowed;\n\
         pub use crate::relay::Hidden;\n\
         pub use crate::relay::*;\n",
    )];
    let off = check(&f, &toml(false));
    // As written: only the relay's own `Hidden` line.
    assert_eq!(kind(&off, "reexport").len(), 1, "{:?}", off.lines);
    let on = check(&f, &toml(true));
    // Also `e::Bad` (line 3), `crate::relay::Hidden` (5), `crate::relay::*` (6).
    let lines: Vec<&String> = kind(&on, "reexport");
    assert_eq!(lines.len(), 4, "{:?}", on.lines);
}

#[cfg(feature = "verus")]
#[test]
fn reexport_chain_inside_verus_block() {
    let toml = "[[crate]]\nname = \"app\"\nverus = true\n[[crate.module]]\npath = \"memory\"\ndeny = [\"engine\"]\n";
    let f = [
        ("src/lib.rs", LIB),
        ("src/engine.rs", ENGINE),
        ("src/api.rs", "verus! { pub use crate::engine::Thing; }\n"),
        ("src/memory.rs", "verus! { use crate::api::Thing; }\n"),
    ];
    assert_eq!(kind(&check(&f, toml), "module").len(), 1);
}

#[test]
fn deny_reexport_through_a_workspace_dependency() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\", \"facade\"]\nresolver = \"2\"\n",
    );
    write(
        root,
        "facade/Cargo.toml",
        "[package]\nname = \"facade\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    write(
        root,
        "facade/src/lib.rs",
        "pub use eff::Bad;\npub use eff::Allowed;\n",
    );
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[dependencies]\nfacade = { path = \"../facade\" }\n",
    );
    write(
        root,
        "app/src/lib.rs",
        "pub use facade::Bad;\npub use facade::Allowed;\npub use facade::*;\nuse facade as f;\npub use f::Bad as Hidden;\n",
    );
    let toml = |r| {
        format!(
            "resolve = {r}\n[[crate]]\nname = \"facade\"\ndeny_reexport = [\"eff\"]\nallow_reexport = [\"*\"]\n\
             [[crate]]\nname = \"app\"\ndeny_reexport = [\"eff\"]\nallow_reexport = [\"Allowed\"]\n"
        )
    };
    write(root, "strata.toml", &toml(false));
    let off = run(Some(&root.join("Cargo.toml")), None).unwrap();
    assert!(kind(&off, "reexport").is_empty(), "{:?}", off.lines);
    write(root, "strata.toml", &toml(true));
    let on = run(Some(&root.join("Cargo.toml")), None).unwrap();
    let l = kind(&on, "reexport");
    // Lines 1 (Bad), 3 (glob: the facade exports Bad) and 5 (Bad through an alias of the crate).
    assert_eq!(l.len(), 3, "{:?}", on.lines);
    assert!(
        l.iter().any(|x| x.contains("app/src/lib.rs:1"))
            && l.iter().any(|x| x.contains("lib.rs:3"))
            && l.iter().any(|x| x.contains("lib.rs:5")),
        "{l:?}"
    );
}
