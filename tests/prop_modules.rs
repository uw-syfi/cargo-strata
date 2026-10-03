//! Source-level generation and module-rule property tests.
//!
//! A generator builds a random module tree (inline modules, file modules,
//! `mod.rs` layout) and writes references to other modules in many syntactic
//! forms, remembering the exact (source module, target module, file, line) of
//! each. Extraction must recover exactly those; rule verdicts must equal a
//! brute-force oracle over them.

use cargo_strata::config::{Config, CrateRule, ModuleRule};
use cargo_strata::modules::check_crate;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const NAMES: &[&str] = &["a", "b", "c", "d"];

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
}

type Path_ = Vec<String>;

fn is_prefix(p: &[String], of: &[String]) -> bool {
    of.len() >= p.len() && of[..p.len()] == *p
}

/// One ground-truth reference.
#[derive(Debug, Clone)]
struct Truth {
    from: Path_,
    to: Path_,
    file: String,
    line: usize,
    in_verus: bool,
    cfg_test: bool,
}

#[derive(Default)]
struct Out {
    files: BTreeMap<String, Vec<String>>,
    truth: Vec<Truth>,
}

struct Gen {
    rng: Rng,
    mods: Vec<Path_>,
    inline: BTreeSet<Path_>,
    mod_rs: BTreeSet<Path_>,
    counter: usize,
    test_mod: bool,
    out: Out,
}

#[derive(Clone, Copy)]
enum Leaf {
    Item,
    Rename,
    Glob,
    SelfMod,
}

#[derive(Default)]
struct Node {
    leaves: Vec<(Leaf, usize)>,
    kids: BTreeMap<String, Node>,
}

impl Gen {
    fn children(&self, p: &[String]) -> Vec<Path_> {
        self.mods
            .iter()
            .filter(|m| m.len() == p.len() + 1 && is_prefix(p, m))
            .cloned()
            .collect()
    }

    fn fresh(&mut self) -> usize {
        self.counter += 1;
        self.counter
    }

    /// Base text and base module for a reference placed in module `s`.
    fn base(&mut self, s: &Path_, allow_self_super: bool) -> (String, Path_) {
        let r = self.rng.below(if allow_self_super { 4 } else { 1 });
        match r {
            0 => ("crate".into(), vec![]),
            1 => ("self".into(), s.clone()),
            _ => {
                let k = 1 + self.rng.below(s.len().max(1));
                let k = k.min(s.len());
                if k == 0 {
                    ("crate".into(), vec![])
                } else {
                    (vec!["super"; k].join("::"), s[..s.len() - k].to_vec())
                }
            }
        }
    }

    /// A random module that is `b` or below it (as relative segments).
    fn target_under(&mut self, b: &Path_) -> Path_ {
        let under: Vec<Path_> = self
            .mods
            .iter()
            .filter(|m| is_prefix(b, m))
            .cloned()
            .collect();
        let mut c = under;
        c.push(b.clone());
        let t = c[self.rng.below(c.len())].clone();
        t[b.len()..].to_vec()
    }

    fn path_text(base: &str, rel: &[String], tail: &str) -> String {
        let mut s = base.to_string();
        for r in rel {
            s.push_str("::");
            s.push_str(r);
        }
        s.push_str("::");
        s.push_str(tail);
        s
    }

    fn render_node(n: &Node, force: bool) -> Vec<String> {
        let mut items = Vec::new();
        for (leaf, id) in &n.leaves {
            items.push(match leaf {
                Leaf::Item => "Item".to_string(),
                Leaf::Rename => format!("Item as r{id}"),
                Leaf::Glob => "*".to_string(),
                Leaf::SelfMod => "self".to_string(),
            });
        }
        for (name, k) in &n.kids {
            let sub = Self::render_node(k, force);
            if sub.len() == 1 && sub[0] != "self" && !force {
                items.push(format!("{name}::{}", sub[0]));
            } else {
                items.push(format!("{name}::{{{}}}", sub.join(", ")));
            }
        }
        items
    }

    /// A `use` line with 1..3 targets under one base; returns text and targets.
    fn use_line(&mut self, s: &Path_) -> (String, Vec<Path_>) {
        let (base, bmod) = self.base(s, true);
        let n = 1 + self.rng.below(3);
        let mut root = Node::default();
        let mut targets = Vec::new();
        for _ in 0..n {
            let rel = self.target_under(&bmod);
            let id = self.fresh();
            let leaf = match self.rng.below(5) {
                0 => Leaf::Rename,
                1 => Leaf::Glob,
                2 if !rel.is_empty() => Leaf::SelfMod,
                _ => Leaf::Item,
            };
            let mut node = &mut root;
            for seg in &rel {
                node = node.kids.entry(seg.clone()).or_default();
            }
            // Duplicate leaves of the same kind on one node are legal Rust
            // for rename/item only once; skip duplicates to stay compilable.
            if node.leaves.iter().any(|(l, _)| {
                std::mem::discriminant(l) == std::mem::discriminant(&leaf)
                    && !matches!(leaf, Leaf::Rename)
            }) {
                continue;
            }
            node.leaves.push((leaf, id));
            targets.push([&bmod[..], &rel[..]].concat());
        }
        let items = Self::render_node(&root, self.rng.chance(1, 4));
        let vis = if self.rng.chance(1, 5) { "pub " } else { "" };
        let text = if items.len() == 1 && items[0] != "self" && !items[0].contains('*')
            || items.len() == 1
                && items[0].starts_with(|c: char| c.is_alphabetic())
                && !items[0].contains('{')
                && items[0] != "self"
        {
            format!("{vis}use {base}::{};", items[0])
        } else {
            format!("{vis}use {base}::{{{}}};", items.join(", "))
        };
        (text, targets)
    }

    /// A non-`use` line referencing one module; `P` is a path to an item.
    fn path_line(&mut self, s: &Path_, verus: bool) -> (String, Path_) {
        let (base, bmod) = self.base(s, true);
        let rel = self.target_under(&bmod);
        let p = Self::path_text(&base, &rel, "Item");
        let n = self.fresh();
        let tpl: &[&str] = if verus {
            &[
                "spec fn v{n}() -> {P} {{ arbitrary() }}",
                "proof fn w{n}() requires {P} == 1 ensures {P} == 1 {{ }}",
                "fn x{n}() {{ let _ = {P}; }}",
                "proof fn y{n}() {{ assert(true) by {{ let _ = {P}; }} }}",
                "spec fn z{n}(a: {P}) -> bool {{ true }}",
                "proof fn u{n}() {{ assert({P} == 2); }}",
            ]
        } else {
            &[
                "pub struct S{n} {{ f: {P}, }}",
                "pub type A{n} = {P};",
                "impl {P} for Z{n} {{}}",
                "fn f{n}<T: {P}>() {{}}",
                "fn g{n}() {{ let _ = {P}; }}",
                "fn h{n}() {{ {P}(); }}",
                "fn m{n}(x: u8) {{ match x {{ {P} => {{}}, _ => {{}} }} }}",
                "const C{n}: u8 = {P};",
                "static X{n}: {P} = todo!();",
                "fn q{n}() -> impl {P} {{ }}",
                "fn k{n}() {{ let _ = |a: {P}| a; }}",
                "fn mm{n}() {{ let _ = vec![{P}]; }}",
                "fn st{n}() {{ let _ = {P} {{ x: 1 }}; }}",
                "fn ex{n}() {{ let _ = 1u8 as {P}; }}",
                "fn wh{n}<T>() where T: {P} {{}}",
            ]
        };
        let t = tpl[self.rng.below(tpl.len())]
            .replace("{n}", &n.to_string())
            .replace("{P}", &p)
            .replace("{{", "{")
            .replace("}}", "}");
        (t, [&bmod[..], &rel[..]].concat())
    }

    /// Render module `m` (whose body is being written into `lines` of `file`).
    fn body(
        &mut self,
        m: &Path_,
        file: &str,
        lines: &mut Vec<String>,
        file_dir: &Path_,
        dir_text: &str,
    ) {
        let _ = (file_dir, dir_text);
        let k = self.rng.below(5);
        let mut verus_lines: Vec<(String, Vec<Path_>)> = Vec::new();
        let is_root = m.is_empty();
        for _ in 0..k {
            if is_root {
                break; // the root has no rules; its references are not checked
            }
            let kind = self.rng.below(10);
            match kind {
                0..=2 => {
                    let (t, ts) = self.use_line(m);
                    self.emit(m, file, lines, t, ts, false, false);
                }
                3..=5 => {
                    let (t, to) = self.path_line(m, false);
                    self.emit(m, file, lines, t, vec![to], false, false);
                }
                6 => {
                    // Decoys: root items, std paths, relative non-use paths.
                    let d = [
                        "pub use crate::Item;",
                        "use std::collections::HashMap;",
                        "fn dd() { let _ = crate::Item; let _ = std::mem::drop(1); let _ = a::f(); }",
                        "use core::fmt;",
                    ];
                    lines.push(d[self.rng.below(d.len())].to_string());
                }
                7 => {
                    // cfg(test) items: references only when skip_cfg_test is off.
                    let (t, to) = self.path_line(m, false);
                    lines.push("#[cfg(test)]".into());
                    if self.rng.chance(1, 2) {
                        self.emit(m, file, lines, t, vec![to], false, true);
                    } else {
                        let (u, ts) = self.use_line(m);
                        self.emit(m, file, lines, u, ts, false, true);
                    }
                }
                _ => {
                    let (t, to) = self.path_line(m, true);
                    verus_lines.push((t, vec![to]));
                    if self.rng.chance(1, 2) {
                        let (u, ts) = self.use_line(m);
                        verus_lines.push((u, ts));
                    }
                }
            }
        }
        if !verus_lines.is_empty() {
            lines.push("verus! {".into());
            for (t, ts) in verus_lines {
                self.emit(m, file, lines, t, ts, true, false);
            }
            lines.push("}".into());
        }
        if !m.is_empty() && self.test_mod && self.rng.chance(1, 4) {
            // A cfg(test) module full of references: skipped when
            // skip_cfg_test is on (tests only generate it then).
            let (t, _) = self.path_line(m, false);
            lines.push("#[cfg(test)]".into());
            lines.push("mod tests {".into());
            lines.push(t);
            lines.push("}".into());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        m: &Path_,
        file: &str,
        lines: &mut Vec<String>,
        text: String,
        targets: Vec<Path_>,
        in_verus: bool,
        cfg_test: bool,
    ) {
        lines.push(text);
        let line = lines.len();
        for to in targets {
            self.out.truth.push(Truth {
                from: m.clone(),
                to,
                file: file.into(),
                line,
                in_verus,
                cfg_test,
            });
        }
    }

    /// Write module `m` (a file module) to `file`, recursively.
    fn file_module(&mut self, m: &Path_, file: String, child_dir: String) {
        let mut lines = Vec::new();
        self.module_into(m, &file, &child_dir, &mut lines, &[]);
        self.out.files.insert(file, lines);
    }

    /// Emit the body of `m` plus declarations of its children into `lines`.
    /// `inline_prefix`: inline module names between the file module and `m`.
    fn module_into(
        &mut self,
        m: &Path_,
        file: &str,
        child_dir: &str,
        lines: &mut Vec<String>,
        inline_prefix: &[String],
    ) {
        let _ = inline_prefix;
        self.body(m, file, lines, &vec![], "");
        for c in self.children(m) {
            let name = c.last().unwrap().clone();
            if self.inline.contains(&c) {
                lines.push(format!("mod {name} {{"));
                let mut sub_dir = child_dir.to_string();
                sub_dir = format!("{sub_dir}/{name}");
                self.module_into(&c, file, &sub_dir, lines, &[]);
                lines.push("}".into());
            } else {
                lines.push(format!("mod {name};"));
                let (f, cd) = if self.mod_rs.contains(&c) {
                    (
                        format!("{child_dir}/{name}/mod.rs"),
                        format!("{child_dir}/{name}"),
                    )
                } else {
                    (
                        format!("{child_dir}/{name}.rs"),
                        format!("{child_dir}/{name}"),
                    )
                };
                self.file_module(&c, f, cd);
            }
        }
    }
}

struct Crate {
    mods: Vec<Path_>,
    out: Out,
}

fn gen_crate(seed: u64, size: usize, test_mod: bool) -> Crate {
    let mut g = Gen {
        rng: Rng(seed | 1),
        mods: vec![],
        inline: BTreeSet::new(),
        mod_rs: BTreeSet::new(),
        counter: 0,
        test_mod,
        out: Out::default(),
    };
    for _ in 0..8 {
        g.rng.next();
    }
    // Random tree of up to `size` non-root modules, depth <= 3.
    let mut tries = 0;
    while g.mods.len() < size && tries < 50 {
        tries += 1;
        let parent: Path_ = if g.mods.is_empty() || g.rng.chance(1, 3) {
            vec![]
        } else {
            let p = g.mods[g.rng.below(g.mods.len())].clone();
            if p.len() >= 3 { continue } else { p }
        };
        let name = NAMES[g.rng.below(NAMES.len())].to_string();
        let mut p = parent;
        p.push(name);
        if g.mods.contains(&p) {
            continue;
        }
        g.mods.push(p);
    }
    g.mods.sort();
    for m in g.mods.clone() {
        if g.rng.chance(1, 3) {
            g.inline.insert(m.clone());
        } else if g.rng.chance(1, 3) {
            g.mod_rs.insert(m.clone());
        }
    }
    // File child directory text is relative to `src`; the root is `src`.
    g.file_module(&vec![], "src/lib.rs".into(), "src".into());
    // `mod.rs` and non-inline children of an inline module live under the
    // inline module's directory, which `module_into` already tracks.
    Crate {
        mods: g.mods,
        out: g.out,
    }
}

fn write_files(root: &Path, files: &BTreeMap<String, Vec<String>>) {
    for (f, lines) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, lines.join("\n") + "\n").unwrap();
    }
}

fn mp(p: &[String]) -> String {
    p.join("::")
}

type Viol = (String, String, String, usize); // from, to, file, line

fn run_crate(
    c: &Crate,
    dir: &Path,
    rules: Vec<ModuleRule>,
    verus_flag: bool,
    skip_cfg_test: bool,
) -> (BTreeSet<Viol>, Vec<String>, Vec<String>) {
    write_files(dir, &c.out.files);
    let rule = CrateRule {
        name: "k".into(),
        allow: None,
        allow_dev: vec![],
        deny: vec![],
        deny_normal: vec![],
        deny_reach: vec![],
        layer_exempt: vec![],
        group_exempt: vec![],
        verus: verus_flag,
        modules: rules,
        ..Default::default()
    };
    let cfg = Config {
        skip_cfg_test,
        ..Config::default()
    };
    let roots: Vec<PathBuf> = vec![dir.join("src/lib.rs")];
    let rep = check_crate("k", &roots, &[&rule], &cfg, cfg!(feature = "verus"), dir);
    let v = rep
        .violations
        .iter()
        .map(|v| {
            (
                v.from.clone(),
                v.to.clone(),
                v.file.display().to_string(),
                v.line,
            )
        })
        .collect();
    // Unknown module rule paths are reported in `rep.unknown` (the library
    // turns them into config errors); keep the old warning shape here.
    let mut warnings = rep.warnings;
    warnings.extend(
        rep.unknown
            .iter()
            .filter(|u| u.key == "path")
            .map(|u| format!("crate `c`: module rule `{}` matches no module", u.name)),
    );
    (v, rep.errors, warnings)
}

/// Ground-truth refs that the extractor is expected to see.
fn visible(c: &Crate, verus_parsed: bool, skip_cfg_test: bool) -> Vec<&Truth> {
    c.out
        .truth
        .iter()
        .filter(|t| !t.in_verus || verus_parsed)
        .filter(|t| !t.cfg_test || !skip_cfg_test)
        .filter(|t| !t.to.is_empty())
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

fn dump(c: &Crate) -> String {
    c.out
        .files
        .iter()
        .map(|(f, l)| format!("--- {f}\n{}", l.join("\n")))
        .collect::<Vec<_>>()
        .join("\n")
}

proptest! {
    #![proptest_config(cases(1500))]

    /// With a deny-everything rule on every module, the reported violations
    /// are exactly the generated references that leave the source's subtree.
    #[test]
    fn extraction_recovers_exactly_the_generated_edges(
        seed in any::<u64>(), size in 1usize..7, verus_flag in any::<bool>(), skip in any::<bool>()
    ) {
        let c = gen_crate(seed, size, skip);
        let all: Vec<String> = c.mods.iter().map(|m| mp(m)).collect();
        let rules = c.mods.iter().map(|m| ModuleRule {
            path: mp(m), depends_on: None, deny: all.clone(),
        }).collect();
        let dir = tempfile::tempdir().unwrap();
        let (got, errors, warnings) = run_crate(&c, dir.path(), rules, verus_flag, skip);
        let parsed = verus_flag && cfg!(feature = "verus");
        let want: BTreeSet<Viol> = visible(&c, parsed, skip).into_iter()
            .filter(|t| !t.from.is_empty() && !is_prefix(&t.from, &t.to))
            .map(|t| (mp(&t.from), mp(&t.to), t.file.clone(), t.line))
            .collect();
        prop_assert!(errors.is_empty(), "errors {:?}\n{}", errors, dump(&c));
        prop_assert!(warnings.iter().all(|w| !w.contains("matches no module")), "{warnings:?}");
        prop_assert_eq!(&got, &want, "\n{}", dump(&c));
    }

    /// Random rules (allow lists, deny lists, nested rules, bogus names) give
    /// the same verdicts as a brute-force evaluation over the generated edges.
    #[test]
    fn module_rules_equal_oracle(
        seed in any::<u64>(), size in 1usize..7, rule_seed in any::<u64>()
    ) {
        let c = gen_crate(seed, size, true);
        let mut rng = Rng(rule_seed | 1);
        for _ in 0..4 { rng.next(); }
        let pool: Vec<String> = c.mods.iter().map(|m| mp(m)).chain(["zz".to_string(), "a::zz".to_string()]).collect();
        let pick = |rng: &mut Rng| {
            let p = pool[rng.below(pool.len())].clone();
            if rng.chance(1, 4) { format!("crate::{p}") } else { p }
        };
        let mut seen = BTreeSet::new();
        let mut rules: Vec<ModuleRule> = Vec::new();
        for _ in 0..rng.below(6) {
            let path = pick(&mut rng);
            let key = path.trim_start_matches("crate::").to_string();
            if !seen.insert(key) { continue; }
            let depends_on = rng.chance(1, 2).then(|| (0..rng.below(4)).map(|_| pick(&mut rng)).collect());
            let deny = (0..rng.below(3)).map(|_| pick(&mut rng)).collect();
            rules.push(ModuleRule { path, depends_on, deny });
        }
        let norm = |s: &str| -> Path_ { s.split("::").filter(|x| *x != "crate" && !x.is_empty()).map(String::from).collect() };
        let dir = tempfile::tempdir().unwrap();
        let (got, errors, warnings) = run_crate(&c, dir.path(), rules.iter().map(|r| ModuleRule {
            path: r.path.clone(), depends_on: r.depends_on.clone(), deny: r.deny.clone() }).collect(), false, true);
        prop_assert!(errors.is_empty());
        // Oracle.
        let mut want: BTreeSet<Viol> = BTreeSet::new();
        for t in visible(&c, false, true) {
            let src = rules.iter().filter(|r| is_prefix(&norm(&r.path), &t.from))
                .max_by_key(|r| norm(&r.path).len());
            let Some(r) = src else { continue };
            if is_prefix(&norm(&r.path), &t.to) { continue; }
            let bad = r.deny.iter().any(|d| is_prefix(&norm(d), &t.to))
                || r.depends_on.as_ref().is_some_and(|l| !l.iter().any(|d| is_prefix(&norm(d), &t.to)));
            if bad { want.insert((mp(&t.from), mp(&t.to), t.file.clone(), t.line)); }
        }
        prop_assert_eq!(&got, &want, "rules {:?}\n{}", rules, dump(&c));
        // Rules naming no module warn, once each.
        let known: BTreeSet<String> = c.mods.iter().map(|m| mp(m)).collect();
        let mut bogus: Vec<String> = rules.iter().filter(|r| !known.contains(&mp(&norm(&r.path)))
            && !norm(&r.path).is_empty()).map(|r| r.path.clone()).collect();
        bogus.sort();
        let mut warned: Vec<String> = warnings.iter().filter(|w| w.contains("matches no module"))
            .map(|w| w.split('`').nth(3).unwrap().to_string()).collect();
        warned.sort();
        prop_assert_eq!(warned, bogus);
    }
}

proptest! {
    #![proptest_config(cases(800))]

    /// Adding a `depends_on` entry or removing a `deny` entry never adds a
    /// violation; shuffling rule order never changes the verdicts.
    #[test]
    fn module_rule_monotonicity_and_order(seed in any::<u64>(), size in 1usize..7, rule_seed in any::<u64>()) {
        let c = gen_crate(seed, size, true);
        let mut rng = Rng(rule_seed | 1);
        for _ in 0..4 { rng.next(); }
        let mods: Vec<String> = c.mods.iter().map(|m| mp(m)).collect();
        let mut rules: Vec<ModuleRule> = Vec::new();
        for m in &mods {
            if !rng.chance(2, 3) { continue; }
            let has = rng.chance(1, 2);
            let mut pick = |p: usize, q: usize| -> Vec<String> {
                let mut v = Vec::new();
                for x in &mods { if rng.chance(p, q) { v.push(x.clone()); } }
                v
            };
            let depends_on = if has { Some(pick(1, 3)) } else { None };
            let deny = pick(1, 4);
            rules.push(ModuleRule { path: m.clone(), depends_on, deny });
        }
        let dir = tempfile::tempdir().unwrap();
        let (base, _, _) = run_crate(&c, dir.path(), rules.clone(), false, true);
        // Extra depends_on entry.
        let mut more = rules.clone();
        for r in more.iter_mut() {
            if let Some(d) = &mut r.depends_on { d.push(mods[rng.below(mods.len())].clone()); }
        }
        let (v, _, _) = run_crate(&c, dir.path(), more, false, true);
        prop_assert!(v.is_subset(&base), "allow-list growth added {:?}", v.difference(&base).collect::<Vec<_>>());
        // One fewer deny entry.
        let mut fewer = rules.clone();
        for r in fewer.iter_mut() { r.deny.pop(); }
        let (v, _, _) = run_crate(&c, dir.path(), fewer, false, true);
        prop_assert!(v.is_subset(&base), "deny removal added {:?}", v.difference(&base).collect::<Vec<_>>());
        // Rule order.
        for i in (1..rules.len()).rev() { let j = rng.below(i + 1); rules.swap(i, j); }
        let (v, _, _) = run_crate(&c, dir.path(), rules, false, true);
        prop_assert_eq!(v, base);
    }
}

/// End to end through `cargo metadata` and `run`: a generated workspace gives
/// the same violations as the ground truth, and the `verus` warning appears
/// exactly when `verus = true` is configured without the feature.
mod end_to_end {
    use super::*;

    proptest! {
        #![proptest_config(cases(120))]

        #[test]
        fn run_matches_ground_truth_and_warns_about_verus(seed in any::<u64>(), size in 1usize..6, verus_flag in any::<bool>()) {
            let c = gen_crate(seed, size, true);
            let dir = tempfile::tempdir().unwrap();
            write_files(dir.path(), &c.out.files);
            std::fs::write(dir.path().join("Cargo.toml"),
                "[package]\nname = \"k\"\nversion = \"0.0.0\"\nedition = \"2021\"\n[lib]\npath = \"src/lib.rs\"\n").unwrap();
            let all: Vec<String> = c.mods.iter().map(|m| mp(m)).collect();
            let mut toml = format!("resolve = false\n[[crate]]\nname = \"k\"\nverus = {verus_flag}\n");
            for m in &all {
                toml.push_str(&format!("[[crate.module]]\npath = \"{m}\"\ndeny = {all:?}\n"));
            }
            std::fs::write(dir.path().join("strata.toml"), toml).unwrap();
            let o = cargo_strata::run(Some(&dir.path().join("Cargo.toml")), None).unwrap();
            prop_assert_eq!(o.errors, 0, "{:?}", o.lines);
            let parsed = verus_flag && cfg!(feature = "verus");
            let want: BTreeSet<(String, String, usize)> = visible(&c, parsed, true).into_iter()
                .filter(|t| !t.from.is_empty() && !is_prefix(&t.from, &t.to))
                .map(|t| (mp(&t.from), mp(&t.to), t.line)).collect();
            let rx = |l: &str| -> Option<(String, String, usize)> {
                let rest = l.strip_prefix("error[module]: ")?;
                let (loc, rest) = rest.split_once(": module `")?;
                let line: usize = loc.rsplit(':').next()?.parse().ok()?;
                let (from, rest) = rest.split_once("` references `")?;
                let (to, _) = rest.split_once('`')?;
                Some((from.to_string(), to.to_string(), line))
            };
            let got: BTreeSet<_> = o.lines.iter().filter_map(|l| rx(l)).collect();
            prop_assert_eq!(&got, &want, "\n{}", dump(&c));
            let warned = o.warnings.iter().any(|w| w.contains("without the `verus` feature"));
            prop_assert_eq!(warned, verus_flag && !cfg!(feature = "verus") && !all.is_empty());
        }
    }
}
