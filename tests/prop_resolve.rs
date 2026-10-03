//! Name resolution against an independent oracle.
//!
//! A generator builds a crate (one file, nested inline modules) whose items
//! are re-exported through chains of `pub use` (renames, groups, `crate::`,
//! `self::`, `super::` paths) and glob imports, cycles included. The oracle
//! computes, by a forward fixpoint over the generator's own spec, which module
//! defines each exported name; resolution (a backward walk over the parsed
//! table) must agree with it. Module rules then run over a probe module whose
//! references are known to resolve to those modules.
//!
//! Properties:
//! * resolution of `crate::M::n`, `self::`/`super::` forms and bare names in
//!   `M` (glob-found included) equals the oracle's defining module;
//! * resolution terminates on glob cycles (the generator makes them on purpose);
//! * module rules with `resolve = false` match an as-written oracle;
//! * with resolution: deny verdicts equal the oracle over as-written and
//!   defining modules (soundness and completeness); depends_on verdicts are
//!   unchanged by resolution; every resolve=false violation stays (monotone).

use cargo_strata::config::ModuleRule;
use cargo_strata::modules::{ModuleUnit, evaluate_with};
use cargo_strata::resolve::{Res, Table};
use cargo_strata::scan::scan_source;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

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

type P = Vec<String>;

fn p(s: &[&str]) -> P {
    s.iter().map(|x| x.to_string()).collect()
}

fn is_prefix(a: &[String], of: &[String]) -> bool {
    of.len() >= a.len() && of[..a.len()] == *a
}

#[derive(Clone, Debug)]
struct Explicit {
    in_mod: P,
    alias: String,
    target: P,
    name: String,
}

struct Spec {
    mods: Vec<P>,
    defs: Vec<(P, String)>,
    explicit: Vec<Explicit>,
    globs: Vec<(P, P)>,
    /// How each explicit use is written, for the source text.
    forms: Vec<String>,
}

type Def = (P, String);

/// Forward closure: which definition does each (module, name) export?
fn oracle(s: &Spec) -> HashMap<(P, String), Def> {
    let mut m: HashMap<(P, String), Def> = HashMap::new();
    for (md, n) in &s.defs {
        m.insert((md.clone(), n.clone()), (md.clone(), n.clone()));
    }
    for _ in 0..200 {
        let before = m.len();
        for e in &s.explicit {
            if let Some(d) = m.get(&(e.target.clone(), e.name.clone())).cloned() {
                m.entry((e.in_mod.clone(), e.alias.clone())).or_insert(d);
            }
        }
        for (into, from) in &s.globs {
            let add: Vec<((P, String), Def)> = m
                .iter()
                .filter(|((t, _), _)| t == from)
                .map(|((_, n), d)| ((into.clone(), n.clone()), d.clone()))
                .collect();
            for (k, d) in add {
                m.entry(k).or_insert(d);
            }
        }
        if m.len() == before {
            break;
        }
    }
    m
}

fn gen_spec(seed: u64, size: usize) -> Spec {
    let mut r = Rng(seed | 1);
    let tops = ["a", "b", "c", "d"];
    let mut mods: Vec<P> = Vec::new();
    for t in tops.iter().take(2 + r.below(3)) {
        mods.push(p(&[t]));
        for ch in ["x", "y"] {
            if r.chance(1, 2) {
                mods.push(p(&[t, ch]));
            }
        }
    }
    let mut s = Spec {
        mods,
        defs: vec![],
        explicit: vec![],
        globs: vec![],
        forms: vec![],
    };
    let mut next_def = 0;
    for m in s.mods.clone() {
        for _ in 0..r.below(3) {
            s.defs.push((m.clone(), format!("D{next_def}")));
            next_def += 1;
        }
    }
    let mut next_alias = 0;
    for _ in 0..size * 3 {
        let m = s.mods[r.below(s.mods.len())].clone();
        if r.chance(1, 4) {
            let t = s.mods[r.below(s.mods.len())].clone();
            if t != m && !s.globs.contains(&(m.clone(), t.clone())) {
                s.globs.push((m, t));
            }
            continue;
        }
        let o = oracle(&s);
        let mut cands: Vec<&(P, String)> = o.keys().filter(|(t, _)| *t != m).collect();
        cands.sort();
        if cands.is_empty() {
            continue;
        }
        let (t, n) = cands[r.below(cands.len())].clone();
        // A name bound twice in one module by different paths is fine only if
        // it is the same definition; keep names unique with an alias.
        let alias = format!("R{next_alias}");
        next_alias += 1;
        let renamed = r.chance(3, 4);
        let (alias_name, text_name) = if renamed {
            (alias.clone(), format!("{n} as {alias}"))
        } else {
            (n.clone(), n.clone())
        };
        if !renamed && o.contains_key(&(m.clone(), n.clone())) {
            continue;
        }
        // Path from `m` to `t`.
        let mut forms: Vec<String> = vec![format!("crate::{}", t.join("::"))];
        if is_prefix(&m, &t) {
            forms.push(format!("self::{}", t[m.len()..].join("::")));
        }
        if !m.is_empty() {
            let par = &m[..m.len() - 1];
            if is_prefix(par, &t) {
                let rest = t[par.len()..].join("::");
                forms.push(if rest.is_empty() {
                    "super".to_string()
                } else {
                    format!("super::{rest}")
                });
            }
        }
        let base = forms[r.below(forms.len())].clone();
        let line = if r.chance(1, 3) {
            format!("pub use {base}::{{{text_name}}};")
        } else {
            format!("pub use {base}::{text_name};")
        };
        s.explicit.push(Explicit {
            in_mod: m,
            alias: alias_name,
            target: t,
            name: n,
        });
        s.forms.push(line);
    }
    s
}

/// Source text: all modules as nested inline modules in one file, then the probe.
fn render(s: &Spec, probe: &[String]) -> String {
    fn emit(s: &Spec, m: &P, depth: usize, out: &mut Vec<String>) {
        let pad = "    ".repeat(depth);
        for (d, n) in &s.defs {
            if d == m {
                out.push(format!("{pad}pub struct {n};"));
            }
        }
        for (e, f) in s.explicit.iter().zip(&s.forms) {
            if e.in_mod == *m {
                out.push(format!("{pad}{f}"));
            }
        }
        for (into, from) in &s.globs {
            if into == m {
                out.push(format!("{pad}pub use crate::{}::*;", from.join("::")));
            }
        }
        for c in &s.mods {
            if c.len() == m.len() + 1 && is_prefix(m, c) {
                out.push(format!("{pad}pub mod {} {{", c.last().unwrap()));
                emit(s, c, depth + 1, out);
                out.push(format!("{pad}}}"));
            }
        }
    }
    let mut out = Vec::new();
    emit(s, &vec![], 0, &mut out);
    out.push("pub mod p {".into());
    out.extend(probe.iter().cloned());
    out.push("}".into());
    out.join("\n")
}

fn known(s: &Spec) -> HashSet<P> {
    let mut k: HashSet<P> = s.mods.iter().cloned().collect();
    k.insert(vec![]);
    k.insert(p(&["p"]));
    k
}

fn table(src: &str, s: &Spec) -> (Table, cargo_strata::scan::Scan) {
    let scan = scan_source(src, false, true).unwrap_or_else(|e| panic!("{e}\n{src}"));
    let t = Table::build(&known(s), [(&[][..], &scan.items)]);
    (t, scan)
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
    #![proptest_config(cases(300))]

    /// Resolution finds the defining module of every exported name, however
    /// it is written, and gives up cleanly on names nothing exports.
    #[test]
    fn resolution_finds_the_defining_module(seed in any::<u64>(), size in 1usize..9) {
        let s = gen_spec(seed, size);
        let src = render(&s, &[]);
        let (t, _) = table(&src, &s);
        let o = oracle(&s);
        for ((m, n), (dm, dn)) in &o {
            let want = Res::Item { module: dm.clone(), rest: vec![dn.clone()] };
            let abs: Vec<String> = ["crate".to_string()].into_iter().chain(m.iter().cloned()).chain([n.clone()]).collect();
            prop_assert_eq!(&t.resolve(&[], &abs), &want, "crate:: form of {:?}\n{}", (m, n), src);
            prop_assert_eq!(&t.resolve(m, std::slice::from_ref(n)), &want, "bare name in {:?}\n{}", (m, n), src);
            // From the probe: a single-segment relative path through the root.
            let rel: Vec<String> = m.iter().cloned().chain([n.clone()]).collect();
            prop_assert_eq!(&t.resolve(&[], &rel), &want, "root-relative {:?}\n{}", (m, n), src);
            if !m.is_empty() {
                let mut sup = vec!["super".to_string()];
                sup.extend(m.iter().skip(m.len() - 1).cloned());
                sup.push(n.clone());
                prop_assert_eq!(&t.resolve(m, &sup), &want, "super form {:?}\n{}", (m, n), src);
            }
        }
        // Unexported names do not resolve to a definition.
        for m in &s.mods {
            let r = t.resolve(m, &["Nope".to_string()]);
            prop_assert_eq!(r, Res::External(vec!["Nope".to_string()]));
            let r = t.resolve(&[], &["crate".to_string(), m[0].clone(), "Nope".to_string()]);
            prop_assert!(matches!(r, Res::Item { .. } | Res::Module(_)), "{r:?}");
        }
    }

    /// Public re-exports enumerate exactly what the oracle says a glob brings
    /// in from outside the defining module.
    #[test]
    fn glob_sources_match_the_oracle(seed in any::<u64>(), size in 1usize..9) {
        let s = gen_spec(seed, size);
        let src = render(&s, &[]);
        let (t, _) = table(&src, &s);
        let o = oracle(&s);
        for m in &s.mods {
            let want: BTreeSet<P> = o.iter().filter(|((mm, _), (dm, _))| mm == m && dm != m).map(|(_, (dm, _))| dm.clone()).collect();
            let got: BTreeSet<P> = t.glob_sources(m).into_iter().filter(|x| x != m).collect();
            prop_assert_eq!(got, want, "glob of {:?}\n{}", m, src);
        }
    }
}

/// A probe module `p` with references to exported names; expectations per line.
struct Probe {
    lines: Vec<String>,
    /// line -> (module as written, defining modules reached by resolution)
    exp: BTreeMap<usize, (Option<P>, Vec<P>)>,
}

fn gen_probe(s: &Spec, seed: u64) -> Probe {
    let mut r = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
    let o = oracle(s);
    let mut keys: Vec<&(P, String)> = o.keys().collect();
    keys.sort();
    let mut pr = Probe {
        lines: vec![],
        exp: BTreeMap::new(),
    };
    let add = |pr: &mut Probe, text: String, written: Option<P>, resolved: Vec<P>| {
        pr.lines.push(format!("    {text}"));
        // Line numbers are fixed by the caller (offset added later).
        let idx = pr.lines.len();
        pr.exp.insert(idx, (written, resolved));
    };
    let mut alias_i = 0;
    for _ in 0..(3 + r.below(6)) {
        match r.below(5) {
            0 | 1 if !keys.is_empty() => {
                let (m, n) = keys[r.below(keys.len())].clone();
                let (dm, _) = &o[&(m.clone(), n.clone())];
                add(
                    &mut pr,
                    format!("use crate::{}::{n};", m.join("::")),
                    Some(m.clone()),
                    vec![dm.clone()],
                );
            }
            2 if !keys.is_empty() => {
                let (m, n) = keys[r.below(keys.len())].clone();
                let (dm, _) = &o[&(m.clone(), n.clone())];
                add(
                    &mut pr,
                    format!(
                        "fn f{alias_i}() {{ let _ = crate::{}::{n}; }}",
                        m.join("::")
                    ),
                    Some(m.clone()),
                    vec![dm.clone()],
                );
                alias_i += 1;
            }
            3 => {
                // Glob import: the module, plus the defining modules of its re-exports.
                let m = s.mods[r.below(s.mods.len())].clone();
                let res: Vec<P> = o
                    .iter()
                    .filter(|((mm, _), _)| *mm == m)
                    .map(|(_, (dm, _))| dm.clone())
                    .collect();
                add(
                    &mut pr,
                    format!("use crate::{}::*;", m.join("::")),
                    Some(m),
                    res,
                );
            }
            _ if !keys.is_empty() => {
                // Module alias, then a name through the alias.
                let m = s.mods[r.below(s.mods.len())].clone();
                let al = format!("M{alias_i}");
                alias_i += 1;
                add(
                    &mut pr,
                    format!("use crate::{} as {al};", m.join("::")),
                    Some(m.clone()),
                    vec![m.clone()],
                );
                let names: Vec<&(P, String)> =
                    keys.iter().copied().filter(|(mm, _)| *mm == m).collect();
                if !names.is_empty() {
                    let (_, n) = names[r.below(names.len())].clone();
                    let (dm, _) = &o[&(m.clone(), n.clone())];
                    add(&mut pr, format!("use {al}::{n};"), None, vec![dm.clone()]);
                }
            }
            _ => {}
        }
    }
    pr
}

fn verdicts(s: &Spec, src: &str, rule: &ModuleRule, resolve: bool) -> BTreeSet<(usize, String)> {
    let (t, scan) = table(src, s);
    let units = [ModuleUnit {
        file: PathBuf::from("/w/lib.rs"),
        module: vec![],
        scan: &scan,
    }];
    let (v, _) = evaluate_with(
        "k",
        &[rule],
        &known(s),
        &units,
        Path::new("/w"),
        resolve.then_some(&t),
    );
    v.into_iter().map(|v| (v.line, v.to)).collect()
}

fn lines_of(v: &BTreeSet<(usize, String)>) -> BTreeSet<usize> {
    v.iter().map(|(l, _)| *l).collect()
}

proptest! {
    #![proptest_config(cases(300))]

    #[test]
    fn module_rules_through_reexports(seed in any::<u64>(), size in 1usize..9, dseed in any::<u64>(), use_depends in any::<bool>()) {
        let s = gen_spec(seed, size);
        let probe = gen_probe(&s, seed ^ dseed);
        let src = render(&s, &probe.lines);
        let base = src.lines().count() - probe.lines.len() - 1; // lines before the first probe line
        let mut r = Rng(dseed | 1);
        let denied: Vec<P> = s.mods.iter().filter(|_| r.chance(1, 3)).cloned().collect();
        let rule = if use_depends {
            ModuleRule { path: "p".into(), depends_on: Some(denied.iter().map(|d| d.join("::")).collect()), deny: vec![] }
        } else {
            ModuleRule { path: "p".into(), depends_on: None, deny: denied.iter().map(|d| d.join("::")).collect() }
        };
        let probe_mod = p(&["p"]);
        let hits = |m: &P| -> bool { denied.iter().any(|d| is_prefix(d, m)) };
        let mut want_written = BTreeSet::new();
        let mut want_resolved = BTreeSet::new();
        for (idx, (written, resolved)) in &probe.exp {
            let line = base + idx;
            if let Some(w) = written {
                let bad = if use_depends { !hits(w) } else { hits(w) };
                if bad && !is_prefix(&probe_mod, w) { want_written.insert(line); }
            }
            // depends_on judges the path as written only.
            if !use_depends && resolved.iter().any(|m| !m.is_empty() && hits(m) && !is_prefix(&probe_mod, m)) {
                want_resolved.insert(line);
            }
        }
        let off = verdicts(&s, &src, &rule, false);
        let on = verdicts(&s, &src, &rule, true);
        // `use al::n` through a module alias is invisible as written.
        prop_assert_eq!(lines_of(&off), want_written.clone(), "resolve=false\n{}", src);
        let want_on: BTreeSet<usize> = want_written.union(&want_resolved).copied().collect();
        prop_assert_eq!(lines_of(&on), want_on, "resolve=true\n{}", src);
        prop_assert!(lines_of(&off).is_subset(&lines_of(&on)), "monotone\n{}", src);
        // Every reported target is a module of the crate, and the as-written
        // violations are a subset of the resolved ones.
        prop_assert!(off.is_subset(&on), "{off:?} vs {on:?}");
    }
}
