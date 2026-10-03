//! Bad configurations give an error that names the config line.

use cargo_strata::Outcome;

/// Workspace with crates `a` (modules `m`, `n`) and `b`; returns the result
/// of running strata with `config`.
fn run_cfg(config: &str) -> Result<Outcome, String> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let w = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    );
    let pkg =
        |n: &str| format!("[package]\nname = \"{n}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n");
    w("a/Cargo.toml", &pkg("a"));
    w("a/src/lib.rs", "pub mod m;\npub mod n;\n");
    w("a/src/m.rs", "");
    w("a/src/n.rs", "");
    w("b/Cargo.toml", &pkg("b"));
    w("b/src/lib.rs", "");
    w("strata.toml", config);
    cargo_strata::run(Some(&root.join("Cargo.toml")), None)
}

fn err(config: &str) -> String {
    match run_cfg(config) {
        Err(e) => e,
        Ok(o) => o.lines.join("\n").to_string(),
    }
}

#[test]
fn unknown_crate_rule_name() {
    let e = err("[[crate]]\nname = \"a\"\n\n[[crate]]\nname = \"typo\"\n");
    assert!(
        e.contains(":5: `[[crate]] name = \"typo\"` matches no workspace crate"),
        "{e}"
    );
}

#[test]
fn unknown_crate_in_layer_and_group() {
    let e = err(
        "[[layer]]\nname = \"low\"\ncrates = [\"a\",\n  \"nope\"]\n\n[[group]]\nname = \"g\"\ncrates = [\"gone*\"]\n",
    );
    assert!(
        e.contains(":4: layer `low`: `nope` matches no workspace crate"),
        "{e}"
    );
    assert!(
        e.contains(":8: group `g`: `gone*` matches no workspace crate"),
        "{e}"
    );
}

#[test]
fn unknown_crate_in_allow() {
    let e = err("[[crate]]\nname = \"a\"\nallow = [\"b\",\n \"c\"]\n");
    assert!(
        e.contains(":4: crate `a`: `allow` names `c`, which is not a workspace crate"),
        "{e}"
    );
}

#[test]
fn overlapping_groups() {
    let e = err(
        "[[group]]\nname = \"one\"\ncrates = [\"a\", \"b\"]\n\n[[group]]\nname = \"two\"\ncrates = [\"b\"]\n",
    );
    assert!(
        e.contains(":7: crate `b` is in groups `one` (line 3) and `two`"),
        "{e}"
    );
}

#[test]
fn overlapping_layers_by_glob() {
    let e = err(
        "[[layer]]\nname = \"x\"\ncrates = [\"*\"]\n[[layer]]\nname = \"y\"\ncrates = [\"a\"]\n",
    );
    assert!(
        e.contains(":6: crate `a` is in layers `x` (line 3) and `y`"),
        "{e}"
    );
}

#[test]
fn unknown_layer_in_may_depend_on() {
    let e = err("[[layer]]\nname = \"x\"\ncrates = [\"a\"]\nmay_depend_on = [\"y\"]\n");
    assert!(
        e.contains(":4: layer `x`: may_depend_on names unknown layer `y`"),
        "{e}"
    );
}

#[test]
fn duplicate_layer_name() {
    let e = err(
        "[[layer]]\nname = \"x\"\ncrates = [\"a\"]\n[[layer]]\nname = \"x\"\ncrates = [\"b\"]\n",
    );
    assert!(e.contains(":5: layer `x` is defined twice"), "{e}");
}

/// A permission cycle between layers is a warning: a matrix may allow both
/// directions, and crates themselves cannot cycle.
#[test]
fn layer_cycle_warns_with_line() {
    let o = run_cfg(
        "[[layer]]\nname = \"x\"\ncrates = [\"a\"]\nmay_depend_on = [\"y\"]\n[[layer]]\nname = \"y\"\ncrates = [\"b\"]\nmay_depend_on = [\"x\"]\n",
    )
    .unwrap();
    let w = o.warnings.join("\n");
    assert!(
        w.contains(":4: layers permit each other in a cycle: x -> y -> x"),
        "{w}"
    );
    assert_eq!(o.violations, 0);
}

#[test]
fn unknown_module_names() {
    let o = run_cfg(
        "[[crate]]\nname = \"a\"\n\n[[crate.module]]\npath = \"m\"\ndepends_on = [\"n\",\n  \"zz\"]\ndeny = [\"yy\"]\n\n[[crate.module]]\npath = \"gone\"\n",
    )
    .unwrap();
    let t = o.lines.join("\n");
    assert!(
        t.contains(":7: crate `a`: module rule `m`: `depends_on` names `zz`"),
        "{t}"
    );
    assert!(
        t.contains(":8: crate `a`: module rule `m`: `deny` names `yy`"),
        "{t}"
    );
    assert!(
        t.contains(":11: crate `a`: module rule `gone` matches no module"),
        "{t}"
    );
    assert_eq!(o.errors, 3, "{t}");
}

#[test]
fn allow_unknown_names_lifts_name_checks() {
    let o = run_cfg(
        "allow_unknown_names = true\n[[crate]]\nname = \"typo\"\n[[crate]]\nname = \"a\"\nallow = [\"c\"]\n[[crate.module]]\npath = \"gone\"\n",
    )
    .unwrap();
    assert_eq!((o.violations, o.errors), (0, 0), "{:?}", o.lines);
}

#[test]
fn unknown_key_names_its_line() {
    let e = err("[[crate]]\nname = \"a\"\nalow = []\n");
    assert!(
        e.contains("line 3") && e.contains("unknown field `alow`"),
        "{e}"
    );
}

#[test]
fn valid_config_is_clean() {
    let o = run_cfg("[[group]]\nname = \"g\"\ncrates = [\"a\"]\n[[group]]\nname = \"h\"\ncrates = [\"b\"]\n[[crate]]\nname = \"a\"\nallow = [\"b\"]\n[[crate.module]]\npath = \"m\"\ndepends_on = [\"n\"]\n").unwrap();
    assert_eq!((o.violations, o.errors), (0, 0), "{:?}", o.lines);
}

#[test]
fn public_module_names_must_exist() {
    let e = err("[[crate]]\nname = \"a\"\npublic_modules = [\"m\",\n  \"typo\"]\n");
    assert!(
        e.contains(":4: crate `a`: public_modules names `typo`, which is not a module"),
        "{e}"
    );
    assert!(!e.contains("`m`, which"), "{e}");
}

#[test]
fn stale_module_exemption_names_its_line() {
    // `m` references nothing, so its exemption of `n` suppresses nothing.
    let e = err(
        "[[crate]]\nname = \"a\"\n\n[[crate.module]]\npath = \"m\"\ndeny = [\"n\"]\nexempt = [\"n\"]\n",
    );
    assert!(
        e.contains(":7: crate `a`: module `m`: stale exemption of `n`"),
        "{e}"
    );
}

#[test]
fn exempt_names_must_be_modules() {
    let e = err("[[crate]]\nname = \"a\"\n\n[[crate.module]]\npath = \"m\"\nexempt = [\"gone\"]\n");
    assert!(
        e.contains("`exempt` names `gone`, which is not a module"),
        "{e}"
    );
}
