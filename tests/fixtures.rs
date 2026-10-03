use cargo_strata::{Outcome, run};
use std::path::PathBuf;

fn fixture(name: &str) -> Outcome {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    run(Some(&dir.join("Cargo.toml")), None).expect("run")
}

fn lines(o: &Outcome, kind: &str) -> Vec<String> {
    o.lines
        .iter()
        .filter(|l| l.starts_with(&format!("error[{kind}]")))
        .cloned()
        .collect()
}

#[test]
fn crate_layers_report_each_violating_edge() {
    let o = fixture("layers");
    let l = lines(&o, "crate");
    let text = l.join("\n");
    assert_eq!(l.len(), 3, "{text}");
    assert!(
        text.contains("`low` -> `high` (normal dependency)"),
        "{text}"
    );
    assert!(text.contains("`low` -> `mid` (dev dependency)"), "{text}");
    assert!(
        text.contains("`mid` -> `peer` (normal dependency): denied"),
        "{text}"
    );
    // Manifest path and line are reported.
    assert!(text.contains("low/Cargo.toml:7:"), "{text}");
    // peer -> high is a layer violation made legal by `allow`; mid -> low goes down.
    assert!(!text.contains("`peer` ->"), "{text}");
    assert!(!text.contains("`mid` -> `low`"), "{text}");
}

#[test]
fn dev_dependencies_ignored_by_default() {
    use cargo_strata::config::Config;
    use cargo_strata::crates::{Edge, evaluate};
    let cfg: Config = toml::from_str(
        r#"
        [[layer]]
        name = "a"
        crates = ["a"]
        [[layer]]
        name = "b"
        crates = ["b"]
        "#,
    )
    .unwrap();
    let ws = ["a", "b"]
        .iter()
        .map(|s| (s.to_string(), String::new()))
        .collect();
    let edge = |kind| Edge {
        from: "a".into(),
        to: "b".into(),
        kind,
        manifest: String::new(),
        optional: false,
    };
    assert!(evaluate(&cfg, &ws, &[edge("dev")]).is_empty());
    assert_eq!(evaluate(&cfg, &ws, &[edge("normal")]).len(), 1);
}

#[test]
fn plain_module_rules() {
    let o = fixture("mods");
    let l = lines(&o, "module");
    let text = l.join("\n");
    let at = |loc: &str| l.iter().filter(|x| x.contains(loc)).count();
    assert!(at("src/memory/mod.rs:5:") >= 1, "{text}");
    assert!(at("src/memory/mod.rs:6:") >= 2, "{text}");
    assert!(at("src/memory/mod.rs:8:") == 1, "{text}");
    assert!(at("src/memory/mod.rs:9:") == 1, "{text}");
    assert!(at("src/memory/mod.rs:10:") == 1, "{text}");
    assert!(at("src/memory/mod.rs:13:") == 1, "{text}");
    assert!(at("src/memory/private.rs:2:") == 1, "{text}");
    assert!(
        at("src/engine/mod.rs:3:") == 1 && text.contains("denies"),
        "{text}"
    );
    // ok cases: util use (3), self::private (4), super::P2 (14), cfg(test) (18), util glob.
    for ok in [
        "mod.rs:3:",
        "mod.rs:4:",
        "mod.rs:14:",
        "mod.rs:18:",
        "private.rs:3:",
    ] {
        assert!(!text.contains(&format!("src/memory/{ok}")), "{ok}: {text}");
    }
    assert!(!text.contains("src/util.rs"), "{text}");
    assert_eq!(o.errors, 0);
}

#[cfg(not(feature = "verus"))]
#[test]
fn verus_bodies_not_checked_without_feature() {
    let o = fixture("verus");
    assert_eq!(lines(&o, "module").len(), 0, "{:?}", o.lines);
    assert!(
        o.warnings
            .iter()
            .any(|w| w.contains("without the `verus` feature")),
        "{:?}",
        o.warnings
    );
}

#[cfg(feature = "verus")]
#[test]
fn verus_bodies_checked_with_feature() {
    let o = fixture("verus");
    let l = lines(&o, "module");
    let text = l.join("\n");
    for line in [4, 10, 19, 20, 29] {
        assert!(
            l.iter()
                .any(|x| x.contains(&format!("src/memory.rs:{line}:"))),
            "line {line}: {text}"
        );
    }
    assert!(
        !l.iter()
            .any(|x| x.contains("src/memory.rs:3:") || x.contains("src/memory.rs:17:")),
        "{text}"
    );
    assert!(
        o.warnings
            .iter()
            .all(|w| !w.contains("without the `verus` feature"))
    );
    assert_eq!(o.errors, 0, "{:?}", o.lines);
}

mod axes {
    use cargo_strata::config::Config;
    use cargo_strata::crates::{Edge, evaluate};
    use std::collections::BTreeMap;

    fn ws(names: &[&str]) -> BTreeMap<String, String> {
        names
            .iter()
            .map(|n| (n.to_string(), format!("{n}/Cargo.toml")))
            .collect()
    }
    fn edge(from: &str, to: &str, kind: &'static str) -> Edge {
        Edge {
            from: from.into(),
            to: to.into(),
            kind,
            manifest: format!("{from}/Cargo.toml"),
            optional: false,
        }
    }
    fn reasons(cfg: &str, names: &[&str], edges: &[Edge]) -> Vec<String> {
        let cfg: Config = toml::from_str(cfg).unwrap();
        evaluate(&cfg, &ws(names), edges)
            .into_iter()
            .map(|(e, r)| format!("{}->{}: {r}", e.from, e.to))
            .collect()
    }

    const MATRIX: &str = r#"
        [[layer]]
        name = "driver"
        crates = ["drv-*"]
        may_depend_on = ["effects"]
        [[layer]]
        name = "effects"
        crates = ["eff"]
        may_depend_on = []
        [[layer]]
        name = "mediator"
        crates = ["med"]
        may_depend_on = ["effects", "mediator"]
    "#;

    #[test]
    fn matrix_layers_are_exact_not_ordered() {
        let names = ["drv-a", "drv-b", "eff", "med"];
        let r = reasons(
            MATRIX,
            &names,
            &[
                edge("drv-a", "eff", "normal"),
                edge("drv-a", "drv-b", "normal"), // own layer is not implicit
                edge("med", "drv-a", "normal"),   // mediator above driver in no order
                edge("med", "med", "normal"),
            ],
        );
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(r[0].starts_with("drv-a->drv-b"), "{r:?}");
        assert!(r[1].starts_with("med->drv-a"), "{r:?}");
    }

    #[test]
    fn exempt_must_suppress_a_real_violation() {
        let cfg =
            format!("{MATRIX}\n[[crate]]\nname = \"med\"\nlayer_exempt = [\"drv-a\", \"eff\"]\n");
        let r = reasons(
            &cfg,
            &["drv-a", "eff", "med"],
            &[edge("med", "drv-a", "normal"), edge("med", "eff", "normal")],
        );
        assert_eq!(r.len(), 1, "{r:?}");
        assert!(r[0].starts_with("med->eff: stale `layer_exempt`"), "{r:?}");
    }

    #[test]
    fn allow_is_exact_and_dev_extends_it() {
        let cfg = "[[crate]]\nname = \"a\"\nallow = [\"b\"]\nallow_dev = [\"c\"]\n";
        check_allow(cfg);
    }

    fn check_allow(cfg: &str) {
        let names = ["a", "b", "c", "d"];
        let cfg2 = format!("check_dev = true\n{cfg}");
        let r = reasons(
            &cfg2,
            &names,
            &[
                edge("a", "b", "normal"),
                edge("a", "c", "normal"), // violation
                edge("a", "c", "dev"),
                edge("a", "d", "dev"), // violation
            ],
        );
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(
            r[0].starts_with("a->c") && r[1].starts_with("a->d"),
            "{r:?}"
        );
    }

    #[test]
    fn external_rules_reach_and_membership() {
        let cfg = r#"
            require_layer = true
            [[layer]]
            name = "verified"
            crates = ["v*"]
            external_allow = ["vstd"]
            [[layer]]
            name = "plain"
            crates = ["bin", "tool"]
            external_deny = ["vstd"]
            [[crate]]
            name = "bin"
            deny_reach = ["tool"]
        "#;
        let r = reasons(
            cfg,
            &["v1", "bin", "mid", "tool", "orphan"],
            &[
                edge("v1", "vstd", "normal"),
                edge("v1", "serde", "normal"),
                edge("bin", "vstd", "normal"),
                edge("bin", "mid", "normal"),
                edge("mid", "tool", "normal"),
            ],
        );
        let t = r.join("\n");
        assert!(
            t.contains("v1->serde: layer `verified` may depend on external"),
            "{t}"
        );
        assert!(
            t.contains("bin->vstd: layer `plain` may not depend on external"),
            "{t}"
        );
        assert!(t.contains("bin->tool: `tool` is reachable"), "{t}");
        assert!(t.contains("orphan->: crate belongs to no `layer`"), "{t}");
        assert!(t.contains("mid->: crate belongs to no `layer`"), "{t}");
        assert_eq!(r.len(), 5, "{t}");
    }
}

#[test]
fn directory_rules_reach_and_placement() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dirs");
    let o = run(
        Some(&dir.join("Cargo.toml")),
        Some(&dir.join("strata.toml")),
    )
    .expect("run");
    let text = o.lines.join("\n");
    let kind = |k: &str| lines(&o, k).len();
    assert_eq!(kind("placement"), 2, "{text}");
    assert_eq!(kind("reach"), 2, "{text}");
    assert!(
        text.contains("`core` reaches `evil` in `banned/evil` (core -> p1 -> evil)"),
        "{text}"
    );
    // A path dependency outside the workspace is still followed.
    assert!(
        text.contains("`p1` reaches `outside` in `ext/outside`"),
        "{text}"
    );
    assert!(
        text.contains("`misplaced` group `misc` belongs under `plug/`"),
        "{text}"
    );
}

fn lints_fixture() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lints");
    let o = run(
        Some(&dir.join("Cargo.toml")),
        Some(&dir.join("strata.toml")),
    )
    .expect("run");
    assert_eq!(o.errors, 0, "{:?}", o.lines);
    o.lines
}

fn at(lines: &[String], kind: &str) -> Vec<String> {
    let mut v: Vec<String> = lines
        .iter()
        .filter(|l| l.starts_with(&format!("error[{kind}]")))
        .map(|l| l.split(": ").nth(1).unwrap().to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn authority_lint() {
    let l = lints_fixture();
    // gate (allowed file) is silent; exempt fns (marker on the line or in the
    // fn), the struct pattern, the method call, comments and strings are not
    // constructs; cfg(test) code and integration tests are scanned.
    assert_eq!(
        at(&l, "authority"),
        [
            "app/src/lib.rs:10",
            "app/src/lib.rs:11",
            "app/src/lib.rs:12",
            "app/src/lib.rs:13",
            "app/src/lib.rs:35",
            "app/tests/it.rs:2"
        ]
    );
}

#[test]
fn reexport_unsafe_forbid_and_source_reach_lints() {
    let l = lints_fixture();
    assert_eq!(at(&l, "reexport"), ["app/src/lib.rs:3", "app/src/lib.rs:4"]);
    assert_eq!(
        at(&l, "source-reach"),
        ["app/src/lib.rs:2", "app/src/lib.rs:6"]
    );
    assert_eq!(at(&l, "forbid"), ["app/src/lib.rs:16"]);
    let text = l.join("\n");
    assert!(
        text.contains("`app` has 3 unsafe sites, ratchet allows 1"),
        "{text}"
    );
    assert!(
        text.contains("`gate/src/lib.rs` lacks `#![forbid(unsafe_code)]`"),
        "{text}"
    );
    assert!(text.contains("entry for `ghost`"), "{text}");
}

/// Regression: with `check_optional = false`, an optional dependency switched
/// on by a default feature is still a dependency (cargo's resolve graph has
/// it); one whose feature is off is not.
#[test]
fn optional_dependency_enabled_by_default_feature_counts() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/optact");
    let o = run(
        Some(&dir.join("Cargo.toml")),
        Some(&dir.join("strata.toml")),
    )
    .expect("run");
    let text = o.lines.join("\n");
    assert!(text.contains("`a` -> `b`"), "{text}");
    assert!(text.contains("`a` reaches `b` in `banned/b`"), "{text}");
    assert!(!text.contains("`c`"), "{text}");
}

#[test]
fn main_call_rule() {
    let l = lints_fixture();
    assert_eq!(
        at(&l, "main"),
        [
            "srv/src/main.rs:1",
            "srv/src/main.rs:3",
            "srv/src/main.rs:6",
            "srv/src/main.rs:7",
            "srv/src/main.rs:8"
        ]
    );
}
