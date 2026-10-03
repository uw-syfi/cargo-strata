//! Source-lint property tests. A generator writes random Rust files (fn
//! bodies, impls, inline modules, `cfg(test)` modules, macro arguments, and
//! `verus!` blocks) and records, line by line, the struct literals, calls,
//! unsafe sites and `pub use` leaves it wrote. Fact extraction must recover
//! exactly those; the lint verdicts must equal an oracle computed from the
//! recorded truth alone.

use cargo_strata::config::{Authority, Config, CrateRule, UnsafePolicy};
use cargo_strata::facts::scan_facts;
use cargo_strata::lints::{CrateInfo, FileInfo, evaluate};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const VERUS: bool = cfg!(feature = "verus");
const TYPES: &[&str] = &["Aa", "Bb"];
const FNS: &[&str] = &["fa", "permit", "fb"];
const ROOTS: &[&str] = &["eff", "oth"];
const LEAVES: &[&str] = &["X", "Y", "Z", "device"];
const MARK: &str = "negctl: fails here";

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
    fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

#[derive(Default, Clone)]
struct Truth {
    lits: Vec<(String, usize)>,
    calls: Vec<(Vec<String>, usize)>,
    unsafe_sites: Vec<usize>,
    /// (leaf, glob, line of the `use` keyword) per leaf of each `pub use`.
    pub_uses: Vec<(Vec<String>, bool, usize)>,
    /// (first line, last line) of fns with bodies.
    fns: Vec<(usize, usize)>,
    markers: Vec<usize>,
    /// Facts only visible when verus bodies are parsed / cfg(test) is not skipped.
    in_verus: bool,
    cfg_test: bool,
}

struct Gen {
    rng: Rng,
    lines: Vec<String>,
    /// Lines written so far (entries may hold several).
    n: usize,
    facts: Vec<Truth>,
}

impl Gen {
    fn line(&mut self, s: &str) -> usize {
        self.lines.push(s.to_string());
        self.n += 1 + s.matches('\n').count();
        self.n
    }
    fn t(&self, v: bool, c: bool) -> Truth {
        Truth {
            in_verus: v,
            cfg_test: c,
            ..Truth::default()
        }
    }

    /// One statement; may span several lines. `in_macro` restricts to shapes
    /// the token scanner recovers. Returns the lines it wrote as text via
    /// self.lines; facts are appended to self.facts.
    fn stmt(&mut self, v: bool, c: bool, in_macro: bool) {
        let ty = self.rng.pick(TYPES).to_string();
        let f = self.rng.pick(FNS).to_string();
        let _ = in_macro;
        let k = self.rng.below(14);
        let mark = self.rng.chance(1, 8);
        let tail = if mark {
            format!(" // {MARK}")
        } else {
            String::new()
        };
        let mut tr = self.t(v, c);
        let start = self.n + 1;
        match k {
            0 => {
                self.line(&format!("let _ = {ty} {{ a: 1 }};{tail}"));
                tr.lits.push((ty, start));
            }
            1 => {
                self.line(&format!("let _ = {ty} {{ a }};{tail}"));
                tr.lits.push((ty, start));
            }
            2 => {
                self.line(&format!("let _ = {ty} {{ ..x }};{tail}"));
                tr.lits.push((ty, start));
            }
            3 => {
                self.line(&format!("let _ = {ty}::new(1);{tail}"));
                tr.calls.push((vec![ty, "new".into()], start));
            }
            4 => {
                self.line(&format!("let _ = m::{f}(1);{tail}"));
                tr.calls.push((vec!["m".into(), f], start));
            }
            5 => {
                self.line(&format!("{f}(1);{tail}"));
                tr.calls.push((vec![f], start));
            }
            6 => {
                self.line(&format!("let _ = unsafe {{ 1 }};{tail}"));
                tr.unsafe_sites.push(start);
            }
            7 => {
                self.line(&format!("let _ = crate::{ty} {{ a: 1 }};{tail}"));
                tr.lits.push((ty, start));
            }
            // outside macros only: shapes whose token scan differs, or nothing to record
            8 => {
                self.line(&format!("let _ = {ty}"));
                self.line("{");
                self.line(&format!("a: 1 }};{tail}"));
                tr.lits.push((ty, start));
            }
            9 => {
                self.line(&format!("x.{f}(1);{tail}"));
            }
            10 => {
                self.line(&format!("let {ty} {{ a }} = x;{tail}"));
            }
            11 => {
                self.line(&format!("let _ = \"{ty} {{ a: 1 }} unsafe {{ }} {f}(1)\"; // {ty} {{ a: 1 }} unsafe {{ }}{tail}"));
            }
            12 => {
                self.line(&format!("let _ = {ty} {{}};{tail}"));
                tr.lits.push((ty, start));
            }
            _ => {
                self.line("let _ = vec![");
                self.macro_body(v, c);
                self.line("];");
                return;
            }
        }
        if mark {
            tr.markers.push(start);
        }
        self.facts.push(tr);
    }

    fn macro_body(&mut self, v: bool, c: bool) {
        // Each element is `expr,` on its own line.
        for _ in 0..1 + self.rng.below(3) {
            let ty = self.rng.pick(TYPES).to_string();
            let f = self.rng.pick(FNS).to_string();
            let k = self.rng.below(5);
            let start = self.n + 1;
            let mut tr = self.t(v, c);
            match k {
                0 => {
                    self.line(&format!("{ty} {{ a: 1 }},"));
                    tr.lits.push((ty, start));
                }
                1 => {
                    self.line(&format!("{ty}::new(1),"));
                    tr.calls.push((vec![ty, "new".into()], start));
                }
                2 => {
                    self.line(&format!("{f}(2),"));
                    tr.calls.push((vec![f], start));
                }
                3 => {
                    self.line("unsafe { 1 },");
                    tr.unsafe_sites.push(start);
                }
                _ => {
                    self.line(&format!("x.{f}(3),"));
                }
            }
            self.facts.push(tr);
        }
    }

    fn body(&mut self, v: bool, c: bool, kw: &str, name: &str) {
        let start = self.line(&format!("{kw}fn {name}() {{"));
        for _ in 0..self.rng.below(5) {
            self.stmt(v, c, false);
        }
        let end = self.line("}");
        let mut tr = self.t(v, c);
        tr.fns.push((start, end));
        self.facts.push(tr);
    }

    fn item(&mut self, depth: usize, v: bool, c: bool) {
        let n = self.rng.below(11);
        let id = self.n;
        match n {
            0..=2 => self.body(v, c, "", &format!("f{id}")),
            3 => {
                // unsafe fn
                let start = self.n + 1;
                self.body(v, c, "unsafe ", &format!("u{id}"));
                let mut tr = self.t(v, c);
                tr.unsafe_sites.push(start);
                self.facts.push(tr);
            }
            4 => {
                // impl with a method, optionally unsafe impl
                let un = self.rng.chance(1, 3);
                let start = self.n + 1;
                self.line(&format!("{}impl S{id} {{", if un { "unsafe " } else { "" }));
                self.body(v, c, "", &format!("m{id}"));
                self.line("}");
                if un {
                    let mut tr = self.t(v, c);
                    tr.unsafe_sites.push(start);
                    self.facts.push(tr);
                }
            }
            5 if depth < 2 => {
                self.line(&format!("mod md{id} {{"));
                for _ in 0..1 + self.rng.below(3) {
                    self.item(depth + 1, v, c);
                }
                self.line("}");
            }
            6 => {
                // pub use
                let root = self.rng.pick(ROOTS).to_string();
                let line = self.n + 1;
                let mut tr = self.t(v, c);
                match self.rng.below(5) {
                    0 => {
                        let l = self.rng.pick(LEAVES).to_string();
                        self.line(&format!("pub use {root}::{l};"));
                        tr.pub_uses.push((vec![root, l], false, line));
                    }
                    1 => {
                        self.line(&format!("pub use {root}::*;"));
                        tr.pub_uses.push((vec![root], true, line));
                    }
                    2 => {
                        let a = self.rng.pick(LEAVES).to_string();
                        let b = self.rng.pick(LEAVES).to_string();
                        self.line(&format!("pub use {root}::{{{a}, {b} as Renamed}};"));
                        tr.pub_uses.push((vec![root.clone(), a], false, line));
                        tr.pub_uses.push((vec![root, b], false, line));
                    }
                    3 => {
                        let a = self.rng.pick(LEAVES).to_string();
                        self.line(&format!("pub(crate) use {root}::sub::{{\n    {a},\n}};"));
                        tr.pub_uses.push((vec![root, "sub".into(), a], false, line));
                    }
                    _ => {
                        let l = self.rng.pick(LEAVES).to_string();
                        self.line(&format!("use {root}::{l};"));
                    }
                }
                self.facts.push(tr);
            }
            7 => {
                // cfg(test) module
                self.line("#[cfg(test)]");
                self.line(&format!("mod tests{id} {{"));
                self.body(v, true, "", &format!("t{id}"));
                self.line("}");
            }
            8 if VERUS && depth == 0 && !v => {
                // verus! block
                self.line("verus! {");
                self.body(true, c, "", &format!("v{id}"));
                self.line("}");
            }
            9 => {
                // a construct outside any fn, with or without the marker
                let ty = self.rng.pick(TYPES).to_string();
                let tail = if self.rng.chance(1, 2) {
                    format!(" // {MARK}")
                } else {
                    String::new()
                };
                let l = self.line(&format!("const C{id}: {ty} = {ty} {{ a: 1 }};{tail}"));
                let mut tr = self.t(v, c);
                tr.lits.push((ty, l));
                self.facts.push(tr);
            }
            _ => {
                // noise: a definition named like a call target, comments
                let l = self.line(&format!("fn permit(a: u32) {{}} // permit(1) {MARK}"));
                let mut tr = self.t(v, c);
                tr.fns.push((l, l));
                self.facts.push(tr);
                self.line("// let _ = Aa { a: 1 }; unsafe { }");
            }
        }
    }
}

fn gen_file(seed: u64) -> (String, Vec<Truth>) {
    let mut g = Gen {
        rng: Rng(seed | 1),
        lines: vec![],
        n: 0,
        facts: vec![],
    };
    for _ in 0..1 + g.rng.below(6) {
        g.item(0, false, false);
    }
    (g.lines.join("\n") + "\n", g.facts)
}

fn visible(t: &[Truth], parse_verus: bool, skip: bool) -> Vec<&Truth> {
    t.iter()
        .filter(|t| (!t.in_verus || parse_verus) && (!t.cfg_test || !skip))
        .collect()
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

proptest! {
    #![proptest_config(cases(1500))]

    /// Fact extraction recovers exactly the generated constructs.
    #[test]
    fn facts_match_generated_truth(seed in any::<u64>(), skip in any::<bool>()) {
        let (src, truth) = gen_file(seed);
        let f = scan_facts(&src, VERUS, skip).unwrap_or_else(|e| panic!("{e}\n{src}"));
        let vis = visible(&truth, VERUS, skip);
        let mut lits: Vec<_> = vis.iter().flat_map(|t| t.lits.clone()).collect();
        let mut calls: Vec<_> = vis.iter().flat_map(|t| t.calls.clone()).collect();
        let mut us: Vec<_> = vis.iter().flat_map(|t| t.unsafe_sites.clone()).collect();
        let mut fns: Vec<_> = vis.iter().flat_map(|t| t.fns.clone()).collect();
        let mut got_lits = f.struct_lits.clone();
        let mut got_calls = f.calls.clone();
        let mut got_us = f.unsafe_sites.clone();
        let mut got_fns = f.fns.clone();
        lits.sort(); calls.sort(); us.sort(); fns.sort();
        got_lits.sort(); got_calls.sort(); got_us.sort(); got_fns.sort();
        // `Ty {}` is an expression (AST) but not a token-scan literal; the
        // generator only emits it outside macros, so it is always recovered.
        prop_assert_eq!(&got_lits, &lits, "literals\n{}", src);
        prop_assert_eq!(&got_calls, &calls, "calls\n{}", src);
        prop_assert_eq!(&got_us, &us, "unsafe\n{}", src);
        prop_assert_eq!(&got_fns, &fns, "fns\n{}", src);
        let mut pu: Vec<_> = vis.iter().flat_map(|t| t.pub_uses.clone()).collect();
        let mut got_pu: Vec<_> = f.pub_uses.iter().map(|u| (u.segs.clone(), u.glob, u.line)).collect();
        pu.sort(); got_pu.sort();
        prop_assert_eq!(got_pu, pu, "pub uses\n{}", src);
    }

    /// Authority, re-export and unsafe verdicts equal an oracle over the truth.
    #[test]
    fn lint_verdicts_match_oracle(
        seeds in prop::collection::vec(any::<u64>(), 1..4),
        skip in any::<bool>(),
        ty in prop::option::of(0usize..2),
        literal in any::<bool>(),
        assoc in any::<bool>(),
        call in any::<bool>(),
        use_marker in any::<bool>(),
        allow_mask in 0u8..8,
        deny_eff in any::<bool>(),
        allow_leaves in prop::collection::vec(any::<bool>(), 4),
        ratchet_delta in -2i64..3,
    ) {
        let n = seeds.len();
        let names: Vec<String> = (0..n).map(|i| format!("w/c/src/f{i}.rs")).collect();
        let allow: Vec<String> = names.iter().enumerate().filter(|(i, _)| allow_mask >> i & 1 == 1).map(|(_, s)| s.clone()).collect();
        let gens: Vec<(String, Vec<Truth>)> = seeds.iter().map(|s| gen_file(*s)).collect();
        let auth = Authority {
            name: "T".into(),
            ty: ty.map(|i| TYPES[i].to_string()),
            literal,
            assoc: if assoc { vec!["new".into()] } else { vec![] },
            calls: if call { vec!["permit".into()] } else { vec![] },
            allow: allow.clone(),
            crates: vec!["c".into()],
            dirs: vec![".".into()],
            exempt_marker: use_marker.then(|| MARK.to_string()),
        };
        let allow_leaf: Vec<String> = LEAVES.iter().zip(&allow_leaves).filter(|(_, b)| **b).map(|(l, _)| l.to_string()).collect();
        let rule = CrateRule {
            name: "c".into(),
            deny_reexport: if deny_eff { vec!["eff".into()] } else { vec![] },
            allow_reexport: allow_leaf.clone(),
            unsafe_policy: Some(UnsafePolicy::Ratchet),
            ..Default::default()
        };
        let cfg = Config { authority: vec![auth.clone()], crates: vec![rule], lint_skip_cfg_test: skip, ..Config::default() };

        let mut files = Vec::new();
        let mut expect: BTreeSet<(String, usize, &str)> = BTreeSet::new();
        let mut total_unsafe = 0usize;
        for (i, (src, truth)) in gens.iter().enumerate() {
            let facts = scan_facts(src, VERUS, skip).unwrap();
            let vis = visible(truth, VERUS, skip);
            let fns: Vec<(usize, usize)> = vis.iter().flat_map(|t| t.fns.clone()).collect();
            let markers: Vec<usize> = src.lines().enumerate().filter(|(_, l)| l.contains(MARK)).map(|(i, _)| i + 1).collect();
            let exempt = |line: usize| -> bool {
                if !use_marker { return false; }
                if markers.contains(&line) { return true; }
                let inner = fns.iter().filter(|(a, b)| *a <= line && line <= *b).min_by_key(|(a, b)| b - a);
                inner.is_some_and(|(a, b)| markers.iter().any(|m| a <= m && m <= b))
            };
            if !allow.contains(&names[i]) {
                for t in &vis {
                    if let Some(ty) = &auth.ty {
                        if literal { for (n, l) in &t.lits { if n == ty && !exempt(*l) { expect.insert((names[i].clone(), *l, "authority")); } } }
                        if assoc { for (s, l) in &t.calls { if s.len() == 2 && &s[0] == ty && s[1] == "new" && !exempt(*l) { expect.insert((names[i].clone(), *l, "authority")); } } }
                    }
                    if call { for (s, l) in &t.calls { if s.last().unwrap() == "permit" && !exempt(*l) { expect.insert((names[i].clone(), *l, "authority")); } } }
                }
            }
            if deny_eff {
                let mut by_line: BTreeMap<usize, bool> = BTreeMap::new();
                for t in &vis {
                    for (segs, glob, l) in &t.pub_uses {
                        if segs[0] != "eff" { continue; }
                        let bad = *glob || !allow_leaf.contains(segs.last().unwrap());
                        *by_line.entry(*l).or_default() |= bad;
                    }
                }
                for (l, bad) in by_line { if bad { expect.insert((names[i].clone(), l, "reexport")); } }
            }
            total_unsafe += vis.iter().map(|t| t.unsafe_sites.len()).sum::<usize>();
            files.push(FileInfo {
                krate: "c".into(),
                rel: names[i].clone(),
                crate_rel: format!("src/f{i}.rs"),
                abs: PathBuf::from(format!("/w/c/src/f{i}.rs")),
                src: src.clone(),
                facts,
                hits: vec![],
            });
        }
        let allowed = (total_unsafe as i64 + ratchet_delta).max(0) as u64;
        let ratchet: BTreeMap<String, u64> = [("c".to_string(), allowed)].into();
        let crates = vec![CrateInfo { name: "c".into(), manifest_rel: "w/c/Cargo.toml".into(), dir_abs: PathBuf::from("/w/c"), roots: vec![], bins: vec![] }];
        let out = evaluate(&cfg, Path::new("/w"), &crates, &files, &ratchet, "r.json");
        let got: BTreeSet<(String, usize, &str)> = out.iter().filter(|v| v.kind == "authority" || v.kind == "reexport").map(|v| (v.file.clone(), v.line, v.kind)).collect();
        prop_assert_eq!(&got, &expect, "{:#?}", gens.iter().map(|g| g.0.clone()).collect::<Vec<_>>());
        let uns: Vec<&str> = out.iter().filter(|v| v.kind == "unsafe").map(|v| v.msg.as_str()).collect();
        let want = if (total_unsafe as u64) > allowed { Some("allows") } else if (total_unsafe as u64) < allowed { Some("says") } else { None };
        match want {
            None => prop_assert!(uns.is_empty(), "{uns:?}"),
            Some(w) => prop_assert!(uns.len() == 1 && uns[0].contains(&format!("ratchet {w}")), "{uns:?} want {w} (n={total_unsafe}, allowed={allowed})"),
        }
    }
}
