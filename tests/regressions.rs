//! Regression tests for bugs found by the property and fuzz tests.

use cargo_strata::config::Config;
use cargo_strata::crates::{Edge, evaluate};

fn ws(names: &[&str]) -> std::collections::BTreeMap<String, String> {
    names
        .iter()
        .map(|s| (s.to_string(), String::new()))
        .collect()
}

fn edge(from: &str, to: &str, kind: &'static str, optional: bool) -> Edge {
    Edge {
        from: from.into(),
        to: to.into(),
        kind,
        manifest: String::new(),
        optional,
    }
}

/// Two exemption globs that both cover a violating dependency: neither is
/// stale, whatever their order.
#[test]
fn overlapping_exemptions_are_not_stale() {
    for order in [["net", "n*"], ["n*", "net"]] {
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[layer]]
            name = "low"
            crates = ["a"]
            [[layer]]
            name = "high"
            crates = ["net"]
            [[crate]]
            name = "a"
            layer_exempt = ["{}", "{}"]
            "#,
            order[0], order[1]
        ))
        .unwrap();
        let v = evaluate(
            &cfg,
            &ws(&["a", "net"]),
            &[edge("a", "net", "normal", false)],
        );
        assert!(v.is_empty(), "{order:?}: {v:?}");
    }
}

/// `deny_reach` honors `check_optional = false`: an optional edge is not part
/// of the graph being checked.
#[test]
fn deny_reach_ignores_optional_edges_when_unchecked() {
    let cfg: Config = toml::from_str(
        r#"
        check_optional = false
        [[crate]]
        name = "a"
        deny_reach = ["c"]
        "#,
    )
    .unwrap();
    let edges = [
        edge("a", "b", "normal", false),
        edge("b", "c", "normal", true),
    ];
    assert!(evaluate(&cfg, &ws(&["a", "b", "c"]), &edges).is_empty());
}

/// `skip_cfg_test` skips every `#[cfg(test)]` item kind, not just modules,
/// fns and uses: structs, impls, consts, type aliases, impl and trait members.
#[test]
fn cfg_test_skips_all_item_kinds() {
    let src = r#"
        #[cfg(test)] pub type A = crate::x::T;
        #[cfg(test)] const C: u8 = crate::x::C;
        #[cfg(test)] static S: crate::x::T = todo!();
        #[cfg(test)] struct W { f: crate::x::T }
        #[cfg(test)] impl crate::x::Tr for W {}
        struct Z;
        impl Z { #[cfg(test)] fn f(&self) -> crate::x::T { todo!() } }
        trait Q { #[cfg(test)] fn g() -> crate::x::T; }
        fn live() -> crate::y::T { todo!() }
    "#;
    let skipped = cargo_strata::scan::scan_source(src, false, true).unwrap();
    let segs: Vec<_> = skipped.refs.iter().map(|r| r.segs.join("::")).collect();
    assert_eq!(segs, ["crate::y::T"]);
    let all = cargo_strata::scan::scan_source(src, false, false).unwrap();
    assert_eq!(all.refs.len(), 8);
}

/// An item macro whose body is not a list of items (`m! { crate::a::B => 1 }`)
/// is still scanned for `crate::` chains; one that is (`cfg_x! { mod a; }`)
/// contributes its `mod` declarations at the position it is written.
#[test]
fn item_macro_bodies() {
    use cargo_strata::scan::scan_source;
    let s = scan_source(
        "mod top;\ncfg_x! {\n    mod a;\n    mod b {\n        mod c;\n    }\n}\nm! { crate::a::B => 1 }\n",
        false,
        true,
    )
    .unwrap();
    let decls: Vec<(String, Vec<String>)> = s
        .decls
        .iter()
        .map(|d| (d.name.clone(), d.inline.clone()))
        .collect();
    assert!(decls.contains(&("top".into(), vec![])), "{decls:?}");
    assert!(decls.contains(&("a".into(), vec![])), "{decls:?}");
    assert!(decls.contains(&("c".into(), vec!["b".into()])), "{decls:?}");
    assert_eq!(s.inline_mods, vec![vec!["b".to_string()]]);
    assert!(
        s.refs
            .iter()
            .any(|r| r.segs == ["crate", "a", "B"] && r.line == 8)
    );
}
