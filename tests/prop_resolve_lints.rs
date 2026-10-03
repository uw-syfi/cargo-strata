//! Source lints through name resolution, against ground truth known by
//! construction.
//!
//! Authority: a type `Permit` and a function `mint` live in module `gate`;
//! other modules re-export them under new names, a `type` alias names one,
//! and a `site` module imports them (renamed or not) and constructs them in
//! many spellings, including `Self` inside `impl` blocks. Each site's
//! expected verdict comes from what the generator made it point at: flagged
//! iff the name resolves to `Permit` or `mint`, or is spelled that way
//! (the as-written rule, kept). `resolve = false` flags exactly the spelled
//! ones, which are a subset.
//!
//! Re-export: modules re-export `eff` items under aliases, through local
//! aliases of the crate, and through globs of re-exporting modules.

use cargo_strata::config::{Authority, Config, CrateRule};
use cargo_strata::facts::scan_facts;
use cargo_strata::lints::{CrateInfo, FileInfo, evaluate};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const VERUS: bool = cfg!(feature = "verus");

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, a: usize, b: usize) -> bool {
        self.below(b) < a
    }
}

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

fn file(src: &str) -> FileInfo {
    FileInfo {
        krate: "c".into(),
        rel: "c/src/lib.rs".into(),
        crate_rel: "src/lib.rs".into(),
        abs: PathBuf::from("/w/c/src/lib.rs"),
        src: src.to_string(),
        facts: scan_facts(src, VERUS, false).unwrap_or_else(|e| panic!("{e}\n{src}")),
        hits: vec![],
    }
}

fn run(cfg: &Config, src: &str, kind: &str) -> BTreeSet<usize> {
    let crates = vec![CrateInfo {
        name: "c".into(),
        manifest_rel: "c/Cargo.toml".into(),
        dir_abs: PathBuf::from("/w/c"),
        roots: vec!["c/src/lib.rs".into()],
        bins: vec![],
    }];
    let out = evaluate(
        cfg,
        Path::new("/w"),
        &crates,
        &[file(src)],
        &BTreeMap::new(),
        "r.json",
    );
    out.iter()
        .filter(|v| v.kind == kind)
        .map(|v| v.line)
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Permit,
    Mint,
    Other,
}

/// Importable names: (path inside the crate, original last segment, what it is).
const POOL: &[(&str, &str, Kind)] = &[
    ("gate::Permit", "Permit", Kind::Permit),
    ("mid1::R1", "R1", Kind::Permit),
    ("mid2::R2", "R2", Kind::Permit),
    ("alias::A", "A", Kind::Permit),
    ("alias::B", "B", Kind::Permit),
    ("gate::Other", "Other", Kind::Other),
    ("mid1::O1", "O1", Kind::Other),
    ("gate::mint", "mint", Kind::Mint),
    ("mid1::m1", "m1", Kind::Mint),
    ("mid2::m2", "m2", Kind::Mint),
];

const PRELUDE: &[&str] = &[
    "pub mod gate {",
    "    pub struct Permit {}",
    "    pub struct Other {}",
    "    impl Permit { pub fn new() -> Permit { Permit {} } }",
    "    impl Other { pub fn new() -> Other { Other {} } }",
    "    pub fn mint() {}",
    "}",
    "pub mod mid1 {",
    "    pub use crate::gate::Permit as R1;",
    "    pub use crate::gate::Other as O1;",
    "    pub use crate::gate::mint as m1;",
    "}",
    "pub mod mid2 {",
    "    pub use crate::mid1::R1 as R2;",
    "    pub use super::mid1::{m1 as m2};",
    "}",
    "pub mod alias {",
    "    pub type A = crate::gate::Permit;",
    "    pub type B = crate::mid1::R1;",
    "}",
];

struct Gen {
    lines: Vec<String>,
    /// line -> (flagged as written, resolves to a listed name)
    sites: BTreeMap<usize, (bool, bool)>,
}

fn gen_authority(seed: u64) -> Gen {
    let mut r = Rng(seed | 1);
    let mut g = Gen {
        lines: PRELUDE.iter().map(|s| s.to_string()).collect(),
        sites: BTreeMap::new(),
    };
    g.lines.push("pub mod site {".into());
    // Imports: (local name, kind, spelled-as-listed-name).
    let mut locals: Vec<(String, Kind, &str)> = Vec::new();
    for i in 0..(2 + r.below(5)) {
        let (path, orig, kind) = POOL[r.below(POOL.len())];
        let local: String = match r.below(4) {
            0 => orig.to_string(),
            // A rename that happens to spell a listed name (name-blind hit kept).
            1 if kind == Kind::Other => "Permit".to_string(),
            1 => format!("L{i}"),
            _ => format!("L{i}"),
        };
        if local == orig {
            g.lines.push(format!("    use crate::{path};"));
        } else {
            g.lines.push(format!("    use crate::{path} as {local};"));
        }
        if locals.iter().any(|(n, _, _)| *n == local) {
            continue; // duplicate local names would not compile
        }
        locals.push((local, kind, orig));
    }
    // Spelled Permit/mint names that are imports of the wrong thing, or none.
    let site = |g: &mut Gen, text: String, written: bool, resolved: bool| {
        g.lines.push(format!("    {text}"));
        let line = g.lines.len();
        g.sites.insert(line, (written, resolved));
    };
    let mut n = 0;
    for (local, kind, _) in locals.clone() {
        n += 1;
        match kind {
            Kind::Permit | Kind::Other => {
                let res = kind == Kind::Permit;
                site(
                    &mut g,
                    format!("fn lit{n}() {{ let _ = {local} {{}}; }}"),
                    local == "Permit",
                    res,
                );
                site(
                    &mut g,
                    format!("fn asc{n}() {{ let _ = {local}::new(); }}"),
                    local == "Permit",
                    res,
                );
                if r.chance(1, 2) {
                    g.lines.push(format!("    impl {local} {{"));
                    site(
                        &mut g,
                        format!("    fn s{n}() {{ let _ = Self {{}}; }}"),
                        false,
                        res,
                    );
                    site(
                        &mut g,
                        format!("    fn t{n}() {{ let _ = Self::new(); }}"),
                        false,
                        res,
                    );
                    g.lines.push("    }".into());
                }
            }
            Kind::Mint => {
                site(
                    &mut g,
                    format!("fn call{n}() {{ {local}(); }}"),
                    local == "mint",
                    true,
                );
            }
        }
    }
    // Spelled paths with no import.
    if r.chance(1, 2) {
        site(
            &mut g,
            "fn p1() { let _ = crate::gate::Permit {}; }".into(),
            true,
            true,
        );
        site(
            &mut g,
            "fn p2() { crate::gate::mint(); }".into(),
            true,
            true,
        );
        site(
            &mut g,
            "fn p3() { let _ = crate::gate::Other {}; }".into(),
            false,
            false,
        );
        site(
            &mut g,
            "fn p4() { let _ = crate::mid2::R2 {}; }".into(),
            false,
            true,
        );
    }
    g.lines.push("}".into());
    g
}

fn authority_cfg(resolve: bool) -> Config {
    let a = Authority {
        name: "permit".into(),
        ty: Some("Permit".into()),
        literal: true,
        assoc: vec!["new".into()],
        calls: vec!["mint".into()],
        allow: vec![],
        crates: vec!["c".into()],
        dirs: vec![".".into()],
        exempt_marker: None,
    };
    Config {
        authority: vec![a],
        resolve,
        ..Config::default()
    }
}

proptest! {
    #![proptest_config(cases(300))]

    #[test]
    fn authority_sees_renames_aliases_and_self(seed in any::<u64>()) {
        let g = gen_authority(seed);
        let src = g.lines.join("\n");
        // Sites inside `gate` itself are not generated, so only `site` lines count.
        let want_written: BTreeSet<usize> = g.sites.iter().filter(|(_, (w, _))| *w).map(|(l, _)| *l).collect();
        let want_resolved: BTreeSet<usize> = g.sites.iter().filter(|(_, (w, r))| *w || *r).map(|(l, _)| *l).collect();
        // The prelude's own constructions (`Permit {}` in `new`, `Other {}`) are in `gate`
        // and flagged by the as-written rule; strip lines before the site module.
        let first = PRELUDE.len() + 1;
        let strip = |s: BTreeSet<usize>| -> BTreeSet<usize> { s.into_iter().filter(|l| *l > first).collect() };
        let off = strip(run(&authority_cfg(false), &src, "authority"));
        let on = strip(run(&authority_cfg(true), &src, "authority"));
        prop_assert_eq!(&off, &want_written, "resolve=false\n{}", src);
        prop_assert_eq!(&on, &want_resolved, "resolve=true\n{}", src);
        prop_assert!(off.is_subset(&on));
    }
}

// ---------- re-exports ----------

fn reexport_cfg(resolve: bool) -> Config {
    let rule = CrateRule {
        name: "c".into(),
        deny_reexport: vec!["eff".into()],
        allow_reexport: vec!["Allowed".into()],
        ..Default::default()
    };
    Config {
        crates: vec![rule],
        resolve,
        ..Config::default()
    }
}

const LEAVES: &[&str] = &["Allowed", "Bad", "Worse"];

fn gen_reexports(
    seed: u64,
) -> (
    Vec<String>,
    BTreeSet<usize>, /* as written */
    BTreeSet<usize>, /* resolved */
) {
    let mut r = Rng(seed | 1);
    let mut lines: Vec<String> = vec![
        "use eff as e;".into(),
        "pub mod local { pub struct Thing {} }".into(),
    ];
    let (mut written, mut resolved) = (BTreeSet::new(), BTreeSet::new());
    let bad = |leaf: &str| leaf != "Allowed";
    // relay modules: (name, public exported leaves)
    let mut relays: Vec<(String, Vec<String>)> = Vec::new();
    for k in 0..(1 + r.below(3)) {
        let name = format!("relay{k}");
        lines.push(format!("pub mod {name} {{"));
        let mut leaves: Vec<String> = Vec::new();
        for j in 0..r.below(4) {
            let leaf = LEAVES[r.below(LEAVES.len())];
            let alias = format!("N{j}");
            let public = r.chance(3, 4);
            let vis = if public { "pub " } else { "" };
            // Inside a module the alias `e` is not in scope; spell `eff` directly.
            lines.push(format!("    {vis}use eff::{leaf} as {alias};"));
            let ln = lines.len();
            if public {
                if bad(leaf) {
                    written.insert(ln);
                    resolved.insert(ln);
                }
                leaves.push(leaf.to_string());
            }
        }
        // A relay may glob an earlier relay.
        if k > 0 && r.chance(1, 2) {
            let (prev, pl) = relays[r.below(relays.len())].clone();
            lines.push(format!("    pub use crate::{prev}::*;"));
            let ln = lines.len();
            if pl.iter().any(|l| bad(l)) {
                resolved.insert(ln);
            }
            leaves.extend(pl);
        }
        lines.push("}".into());
        relays.push((name, leaves));
    }
    for _ in 0..(2 + r.below(6)) {
        match r.below(6) {
            0 => {
                let leaf = LEAVES[r.below(LEAVES.len())];
                lines.push(format!("pub use eff::{leaf};"));
                if bad(leaf) {
                    written.insert(lines.len());
                    resolved.insert(lines.len());
                }
            }
            1 => {
                let leaf = LEAVES[r.below(LEAVES.len())];
                lines.push(format!("pub use e::{leaf};"));
                if bad(leaf) {
                    resolved.insert(lines.len());
                }
            }
            2 => {
                lines.push("pub use e::*;".into());
                resolved.insert(lines.len());
            }
            3 if !relays.is_empty() => {
                let (name, leaves) = relays[r.below(relays.len())].clone();
                lines.push(format!("pub use crate::{name}::*;"));
                if leaves.iter().any(|l| bad(l)) {
                    resolved.insert(lines.len());
                }
            }
            4 => {
                lines.push("pub use crate::local::Thing;".into());
            }
            _ => {
                lines.push("pub use self::local::*;".into());
            }
        }
    }
    (lines, written, resolved)
}

proptest! {
    #![proptest_config(cases(300))]

    #[test]
    fn reexport_follows_aliases_and_relays(seed in any::<u64>()) {
        let (lines, written, resolved) = gen_reexports(seed);
        let src = lines.join("\n");
        let off = run(&reexport_cfg(false), &src, "reexport");
        let on = run(&reexport_cfg(true), &src, "reexport");
        prop_assert_eq!(&off, &written, "resolve=false\n{}", src);
        prop_assert_eq!(&on, &resolved, "resolve=true\n{}", src);
        prop_assert!(off.is_subset(&on));
    }
}
