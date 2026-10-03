//! `[crate.main] items`: a binary root holds only `fn main`, inner attributes
//! and the listed top-level items. Generated files put a random sequence of
//! items (with attributes, doc comments and blank lines) around `fn main`;
//! each item's descriptor and line are known by construction, and the
//! expected violations are the items whose descriptor matches no listed glob.

use cargo_strata::config::{Config, CrateRule, MainRule, glob_match};
use cargo_strata::facts::scan_facts;
use cargo_strata::lints::{CrateInfo, FileInfo, evaluate};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const VERUS: bool = cfg!(feature = "verus");

fn cases(default: u32) -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        cases: std::env::var("STRATA_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default),
        ..ProptestConfig::default()
    }
}

/// (source lines, descriptor, line offset of the item keyword within them).
const POOL: &[(&[&str], &str, usize)] = &[
    (&["mod neg;"], "mod neg", 0),
    (&["#[cfg(feature = \"x\")]", "mod neg;"], "mod neg", 1),
    (&["mod util {", "    pub fn f() {}", "}"], "mod util", 0),
    (&["use std::env;"], "use", 0),
    (&["use std::{fmt, io};"], "use", 0),
    (&["fn helper() {}"], "fn helper", 0),
    (
        &["/// doc", "fn helper2(x: u32) -> u32 {", "    x", "}"],
        "fn helper2",
        1,
    ),
    (&["struct S;"], "struct S", 0),
    (&["#[derive(Debug)]", "enum E { A }"], "enum E", 1),
    (&["const C: u32 = 1;"], "const C", 0),
    (&["static N: u32 = 1;"], "static N", 0),
    (&["type T = u32;"], "type T", 0),
    (&["trait Tr {}"], "trait Tr", 0),
    (&["impl S {}"], "impl", 0),
    (&["extern crate alloc;"], "extern crate alloc", 0),
    (&["macro_rules! m { () => {} }"], "macro_rules!", 0),
];

const GLOBS: &[&str] = &[
    "mod neg",
    "mod *",
    "use",
    "fn helper",
    "fn *",
    "struct S",
    "*",
    "enum ?",
    "impl",
    "const C",
    "macro_rules!",
    "extern crate *",
];

fn gen_file(order: &[usize], main_at: usize) -> (String, Vec<(usize, &'static str)>) {
    let mut lines = vec!["#![forbid(unsafe_code)]".to_string(), String::new()];
    let mut truth = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        if k == main_at {
            lines.push("fn main() {".into());
            lines.push("    let _x = 1;".into());
            lines.push("}".into());
        }
        let (src, desc, off) = POOL[i];
        truth.push((lines.len() + 1 + off, desc));
        lines.extend(src.iter().map(|s| s.to_string()));
        lines.push(String::new());
    }
    if main_at >= order.len() {
        lines.push("fn main() {}".into());
    }
    (lines.join("\n") + "\n", truth)
}

fn run(src: &str, items: Option<Vec<String>>) -> BTreeSet<usize> {
    let cfg = Config {
        crates: vec![CrateRule {
            name: "c".into(),
            main: Some(MainRule {
                items,
                ..MainRule::default()
            }),
            ..CrateRule::default()
        }],
        ..Config::default()
    };
    let rel = "c/src/main.rs".to_string();
    let file = FileInfo {
        krate: "c".into(),
        rel: rel.clone(),
        crate_rel: "src/main.rs".into(),
        abs: PathBuf::from("/w/c/src/main.rs"),
        src: src.to_string(),
        facts: scan_facts(src, VERUS, false).unwrap_or_else(|e| panic!("{e}\n{src}")),
        hits: vec![],
    };
    let crates = vec![CrateInfo {
        name: "c".into(),
        manifest_rel: "c/Cargo.toml".into(),
        dir_abs: PathBuf::from("/w/c"),
        roots: vec![rel.clone()],
        bins: vec![rel],
    }];
    evaluate(
        &cfg,
        Path::new("/w"),
        &crates,
        &[file],
        &BTreeMap::new(),
        "r.json",
    )
    .iter()
    .filter(|v| v.kind == "main" && v.msg.starts_with("top-level"))
    .map(|v| v.line)
    .collect()
}

proptest! {
    #![proptest_config(cases(256))]

    #[test]
    fn items_rule_equals_oracle(
        order in prop::collection::vec(0..POOL.len(), 0..8),
        main_at in 0usize..9,
        globs in prop::option::of(prop::collection::vec(prop::sample::select(GLOBS), 0..4)),
    ) {
        let (src, truth) = gen_file(&order, main_at);
        let items: Option<Vec<String>> = globs.map(|g| g.iter().map(|s| s.to_string()).collect());
        let want: BTreeSet<usize> = match &items {
            None => BTreeSet::new(),
            Some(g) => truth
                .iter()
                .filter(|(_, d)| !g.iter().any(|p| glob_match(p, d)))
                .map(|(l, _)| *l)
                .collect(),
        };
        prop_assert_eq!(run(&src, items), want, "{}", src);
    }
}
