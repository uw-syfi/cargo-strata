//! Property tests for crate-level rules: `crates::evaluate` against a brute-force
//! reference oracle written from the documented semantics (README), not from
//! the implementation.
#![allow(clippy::nonminimal_bool, clippy::needless_range_loop)] // oracles are written to mirror the spec, not minimized

use cargo_strata::config::{Config, CrateRule, Layer};
use cargo_strata::crates::{Edge, evaluate};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

const WS: &[&str] = &[
    "core",
    "core-util",
    "net",
    "net-http",
    "net-tls",
    "app",
    "app-cli",
    "sim",
];
const EXT: &[&str] = &["serde", "tokio", "tokio-util", "regex"];
const GLOBS: &[&str] = &[
    "core",
    "core-util",
    "net",
    "net-http",
    "net-tls",
    "app",
    "app-cli",
    "sim",
    "core*",
    "net*",
    "app*",
    "*-util",
    "*",
    "tokio*",
    "serde",
    "regex",
    "c?re",
    "*-http",
    "s*",
];
const KINDS: &[&str] = &["normal", "dev", "build"];

/// Reference glob: `*` any run, `?` one char, everything else literal.
fn g(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| go(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some(c) => t.first() == Some(c) && go(&p[1..], &t[1..]),
        }
    }
    go(&p, &t)
}

type Key = (String, String, String, String); // (category, from, to, kind)

fn k(c: &str, f: &str, t: &str, kind: &str) -> Key {
    (c.into(), f.into(), t.into(), kind.into())
}

fn member(axis: &[Layer], name: &str) -> Option<usize> {
    (0..axis.len()).find(|&i| axis[i].crates.iter().any(|p| g(p, name)))
}

/// The axis verdict for an internal edge, as a bool, before exemptions.
fn axis_bad(axis: &[Layer], from: &str, to: &str, kind: &str) -> bool {
    let Some(a) = member(axis, from) else {
        return false;
    };
    if axis[a].deny.iter().any(|p| g(p, to)) {
        return true;
    }
    if kind == "normal" && axis[a].deny_normal.iter().any(|p| g(p, to)) {
        return true;
    }
    let Some(b) = member(axis, to) else {
        return false;
    };
    match &axis[a].may_depend_on {
        Some(l) => !l.contains(&axis[b].name),
        None => b > a,
    }
}

pub fn oracle(cfg: &Config, ws: &[String], edges: &[Edge]) -> BTreeSet<Key> {
    let is_ws = |n: &str| ws.iter().any(|w| w == n);
    let live = |e: &&Edge| {
        !(e.kind == "dev" && !cfg.check_dev
            || e.kind == "build" && !cfg.check_build
            || e.optional && !cfg.check_optional)
    };
    let mut out = BTreeSet::new();
    // Exemption (rule index, glob, axis) is used if it suppresses a violation.
    let mut used: BTreeSet<(usize, String, &str)> = BTreeSet::new();
    for e in edges.iter().filter(live) {
        let rules: Vec<(usize, &CrateRule)> = cfg
            .crates
            .iter()
            .enumerate()
            .filter(|(_, r)| g(&r.name, &e.from))
            .collect();
        for (_, r) in &rules {
            if r.deny.iter().any(|p| g(p, &e.to)) {
                out.insert(k("deny", &e.from, &e.to, e.kind));
            }
            if e.kind == "normal" && r.deny_normal.iter().any(|p| g(p, &e.to)) {
                out.insert(k("deny_normal", &e.from, &e.to, e.kind));
            }
        }
        if is_ws(&e.to) {
            for (_, r) in &rules {
                if let Some(al) = &r.allow {
                    let ok = al.iter().any(|p| g(p, &e.to))
                        || e.kind == "dev" && r.allow_dev.iter().any(|p| g(p, &e.to));
                    if !ok {
                        out.insert(k("allow", &e.from, &e.to, e.kind));
                    }
                }
            }
            for (axis, tag, on) in [
                (&cfg.layers, "layer", e.kind != "dev" || cfg.layer_dev),
                (&cfg.groups, "group", e.kind != "dev" || cfg.group_dev),
            ] {
                if !on || !axis_bad(axis, &e.from, &e.to, e.kind) {
                    continue;
                }
                let mut exempted = false;
                for (i, r) in &rules {
                    let l = if tag == "layer" {
                        &r.layer_exempt
                    } else {
                        &r.group_exempt
                    };
                    for p in l.iter().filter(|p| g(p, &e.to)) {
                        exempted = true;
                        used.insert((*i, p.clone(), tag));
                    }
                }
                if !exempted {
                    out.insert(k(tag, &e.from, &e.to, e.kind));
                }
            }
        } else if e.kind == "normal"
            && let Some(a) = member(&cfg.layers, &e.from)
        {
            let l = &cfg.layers[a];
            if l.external_deny.iter().any(|p| g(p, &e.to))
                || l.external_allow
                    .as_ref()
                    .is_some_and(|al| !al.iter().any(|p| g(p, &e.to)))
            {
                out.insert(k("ext", &e.from, &e.to, e.kind));
            }
        }
    }
    // Reachability: closure over live normal edges between workspace crates.
    let mut reach: BTreeSet<(String, String)> = edges
        .iter()
        .filter(live)
        .filter(|e| e.kind == "normal" && is_ws(&e.to))
        .map(|e| (e.from.clone(), e.to.clone()))
        .collect();
    loop {
        let more: Vec<_> = reach
            .iter()
            .flat_map(|(a, b)| {
                reach
                    .iter()
                    .filter(move |(c, _)| c == b)
                    .map(move |(_, d)| (a.clone(), d.clone()))
            })
            .filter(|p| !reach.contains(p))
            .collect();
        if more.is_empty() {
            break;
        }
        reach.extend(more);
    }
    for r in &cfg.crates {
        for (a, b) in &reach {
            if g(&r.name, a) && r.deny_reach.iter().any(|p| g(p, b)) && is_ws(a) {
                out.insert(k("reach", a, b, "normal"));
            }
        }
    }
    // Stale exemptions.
    for (i, r) in cfg.crates.iter().enumerate() {
        for (tag, l) in [("layer", &r.layer_exempt), ("group", &r.group_exempt)] {
            for p in l {
                for w in ws.iter().filter(|w| g(&r.name, w)) {
                    // Used by this crate: some live internal edge from `w`
                    // with an axis violation matched by `p`.
                    let used_here = edges.iter().filter(live).any(|e| {
                        e.from == *w
                            && is_ws(&e.to)
                            && g(p, &e.to)
                            && match tag {
                                "layer" => {
                                    (e.kind != "dev" || cfg.layer_dev)
                                        && axis_bad(&cfg.layers, w, &e.to, e.kind)
                                }
                                _ => {
                                    (e.kind != "dev" || cfg.group_dev)
                                        && axis_bad(&cfg.groups, w, &e.to, e.kind)
                                }
                            }
                    });
                    let _ = (&used, i);
                    if !used_here {
                        out.insert(k(&format!("stale_{tag}"), w, p, "normal"));
                    }
                }
            }
        }
    }
    for (axis, tag, req) in [
        (&cfg.layers, "require_layer", cfg.require_layer),
        (&cfg.groups, "require_group", cfg.require_group),
    ] {
        if req {
            for w in ws {
                if member(axis, w).is_none() {
                    out.insert(k(tag, w, "", "normal"));
                }
            }
        }
    }
    out
}

/// Map strata's output to oracle keys.
pub fn actual(cfg: &Config, ws: &[String], edges: &[Edge]) -> BTreeSet<Key> {
    let map: BTreeMap<String, String> = ws.iter().map(|w| (w.clone(), String::new())).collect();
    evaluate(cfg, &map, edges)
        .into_iter()
        .map(|(e, reason)| {
            let cat = if reason.starts_with("denied by `deny_normal") {
                "deny_normal"
            } else if reason.starts_with("denied by `deny =") {
                "deny"
            } else if reason.starts_with("not in the `allow`") {
                "allow"
            } else if reason.starts_with("layer `") && reason.contains("external") {
                "ext"
            } else if reason.starts_with("layer `") {
                "layer"
            } else if reason.starts_with("group `") {
                "group"
            } else if reason.contains("is reachable") {
                "reach"
            } else if reason.starts_with("stale `layer_exempt`") {
                "stale_layer"
            } else if reason.starts_with("stale `group_exempt`") {
                "stale_group"
            } else if reason.starts_with("crate belongs to no `layer`") {
                "require_layer"
            } else if reason.starts_with("crate belongs to no `group`") {
                "require_group"
            } else {
                panic!("unclassified reason: {reason}")
            };
            k(cat, &e.from, &e.to, e.kind)
        })
        .collect()
}

fn glob_list(max: usize) -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(prop::sample::select(GLOBS).prop_map(String::from), 0..=max)
}

fn axis(prefix: &'static str) -> impl Strategy<Value = Vec<Layer>> {
    // 0..4 axes; membership globs; ordered or exact `may_depend_on`.
    (
        prop::collection::vec(
            (
                glob_list(3),
                prop::option::of(prop::collection::vec(0usize..4, 0..4)),
                glob_list(1),
                glob_list(1),
                prop::option::of(glob_list(3)),
                glob_list(1),
            ),
            0..4,
        ),
        any::<bool>(),
    )
        .prop_map(move |(ls, use_deps)| {
            ls.into_iter()
                .enumerate()
                .map(
                    |(i, (crates, deps, deny, deny_normal, ext_allow, ext_deny))| Layer {
                        name: format!("{prefix}{i}"),
                        crates,
                        may_depend_on: if use_deps {
                            deps.map(|d| d.iter().map(|j| format!("{prefix}{j}")).collect())
                        } else {
                            None
                        },
                        deny,
                        deny_normal,
                        external_allow: ext_allow,
                        external_deny: ext_deny,
                        ..Default::default()
                    },
                )
                .collect()
        })
}

fn crate_rule() -> impl Strategy<Value = CrateRule> {
    (
        prop::sample::select(GLOBS).prop_map(String::from),
        prop::option::of(glob_list(4)),
        glob_list(2),
        glob_list(1),
        glob_list(1),
        glob_list(1),
        glob_list(2),
        glob_list(2),
    )
        .prop_map(
            |(
                name,
                allow,
                allow_dev,
                deny,
                deny_normal,
                deny_reach,
                layer_exempt,
                group_exempt,
            )| {
                CrateRule {
                    name,
                    allow,
                    allow_dev,
                    deny,
                    deny_normal,
                    deny_reach,
                    layer_exempt,
                    group_exempt,
                    verus: false,
                    modules: vec![],
                    ..Default::default()
                }
            },
        )
}

#[derive(Debug, Clone)]
pub struct Case {
    pub cfg_flags: [bool; 7],
    pub layers: Vec<Layer>,
    pub groups: Vec<Layer>,
    pub rules: Vec<CrateRule>,
    pub edges: Vec<(usize, usize, usize, bool)>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        prop::array::uniform7(any::<bool>()),
        axis("L"),
        axis("G"),
        prop::collection::vec(crate_rule(), 0..5),
        prop::collection::vec(
            (
                0..WS.len(),
                0..WS.len() + EXT.len(),
                0..3usize,
                any::<bool>(),
            ),
            0..24,
        ),
    )
        .prop_map(|(cfg_flags, layers, groups, rules, edges)| Case {
            cfg_flags,
            layers,
            groups,
            rules,
            edges,
        })
}

impl Case {
    pub fn build(&self) -> (Config, Vec<String>, Vec<Edge>) {
        let f = self.cfg_flags;
        let cfg = Config {
            check_dev: f[0],
            check_build: f[1],
            check_optional: f[2],
            layer_dev: f[3],
            group_dev: f[4],
            require_layer: f[5],
            require_group: f[6],
            skip_cfg_test: true,
            layers: self.layers.clone(),
            groups: self.groups.clone(),
            crates: self.rules.iter().map(clone_rule).collect(),
            ..Config::default()
        };
        let ws: Vec<String> = WS.iter().map(|s| s.to_string()).collect();
        let edges = self
            .edges
            .iter()
            .filter(|(a, b, _, _)| a != b)
            .map(|&(a, b, kd, opt)| Edge {
                from: WS[a].into(),
                to: if b < WS.len() {
                    WS[b]
                } else {
                    EXT[b - WS.len()]
                }
                .into(),
                kind: KINDS[kd],
                manifest: String::new(),
                optional: opt,
            })
            .collect();
        (cfg, ws, edges)
    }
}

fn clone_rule(r: &CrateRule) -> CrateRule {
    CrateRule {
        name: r.name.clone(),
        allow: r.allow.clone(),
        allow_dev: r.allow_dev.clone(),
        deny: r.deny.clone(),
        deny_normal: r.deny_normal.clone(),
        deny_reach: r.deny_reach.clone(),
        layer_exempt: r.layer_exempt.clone(),
        group_exempt: r.group_exempt.clone(),
        verus: false,
        modules: vec![],
        ..Default::default()
    }
}

fn is_stale(c: &str) -> bool {
    c.starts_with("stale")
}

fn cases() -> ProptestConfig {
    ProptestConfig {
        failure_persistence: None,
        cases: std::env::var("STRATA_CASES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4000),
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(cases())]

    /// Soundness and completeness: the reported set equals the oracle's.
    #[test]
    fn verdicts_equal_oracle(c in case()) {
        let (cfg, ws, edges) = c.build();
        let want = oracle(&cfg, &ws, &edges);
        let got = actual(&cfg, &ws, &edges);
        prop_assert_eq!(&got, &want, "missing {:?} extra {:?}",
            want.difference(&got).collect::<Vec<_>>(), got.difference(&want).collect::<Vec<_>>());
    }

    /// Removing an edge never adds a violation (stale-exemption reports are
    /// about absence of edges and are excluded).
    #[test]
    fn removing_an_edge_is_monotone(c in case(), idx in any::<prop::sample::Index>()) {
        let (cfg, ws, edges) = c.build();
        prop_assume!(!edges.is_empty());
        let mut fewer = edges.clone();
        fewer.remove(idx.index(edges.len()));
        let before = actual(&cfg, &ws, &edges);
        let after = actual(&cfg, &ws, &fewer);
        for v in after.difference(&before) {
            prop_assert!(is_stale(&v.0), "new violation {v:?}");
        }
    }

    /// Adding an edge that is itself allowed adds no violation, except through
    /// `deny_reach` (new reachability) and it may only remove stale reports.
    #[test]
    fn adding_an_allowed_edge_is_monotone(c in case(), a in 0..WS.len(), b in 0..WS.len() + EXT.len(), kd in 0..3usize, opt in any::<bool>()) {
        let (mut cfg, ws, edges) = c.build();
        prop_assume!(a != b);
        for r in &mut cfg.crates { r.deny_reach.clear(); }
        let e = Edge { from: WS[a].into(), to: if b < WS.len() { WS[b] } else { EXT[b - WS.len()] }.into(),
            kind: KINDS[kd], manifest: String::new(), optional: opt };
        let before = actual(&cfg, &ws, &edges);
        let mut more = edges.clone();
        more.push(e.clone());
        let after = actual(&cfg, &ws, &more);
        let mut only = edges.clone();
        only.clear();
        only.push(e.clone());
        let alone: BTreeSet<Key> = actual(&cfg, &ws, &only).into_iter().filter(|v| !is_stale(&v.0) && !v.0.starts_with("require")).collect();
        prop_assume!(alone.is_empty());
        let non_stale = |s: &BTreeSet<Key>| s.iter().filter(|v| !is_stale(&v.0)).cloned().collect::<BTreeSet<_>>();
        prop_assert_eq!(non_stale(&after), non_stale(&before));
        prop_assert!(after.iter().filter(|v| is_stale(&v.0)).all(|v| before.contains(v)));
    }

    /// Verdicts do not depend on the order of `[[crate]]` entries, edges, or
    /// (for disjoint-name layers) glob lists.
    #[test]
    fn order_independence(c in case(), seed in any::<u64>()) {
        let (cfg, ws, edges) = c.build();
        let want = actual(&cfg, &ws, &edges);
        let mut c2 = c.clone();
        let mut s = seed;
        let mut next = || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
        for i in (1..c2.rules.len()).rev() { let j = (next() % (i as u64 + 1)) as usize; c2.rules.swap(i, j); }
        for i in (1..c2.edges.len()).rev() { let j = (next() % (i as u64 + 1)) as usize; c2.edges.swap(i, j); }
        for r in &mut c2.rules {
            for l in [&mut r.allow_dev, &mut r.deny, &mut r.deny_normal, &mut r.layer_exempt, &mut r.group_exempt] {
                for i in (1..l.len()).rev() { let j = (next() % (i as u64 + 1)) as usize; l.swap(i, j); }
            }
        }
        let (cfg2, ws2, edges2) = c2.build();
        prop_assert_eq!(actual(&cfg2, &ws2, &edges2), want);
    }

    /// Ordered layers (no `may_depend_on`, no denies): allowed edges compose
    /// transitively: a -> b and b -> c allowed implies a -> c allowed.
    #[test]
    fn ordered_layers_are_transitive(n in 2usize..6, assign in prop::collection::vec(0usize..6, 8)) {
        let layers: Vec<Layer> = (0..n).map(|i| Layer {
            name: format!("L{i}"),
            crates: WS.iter().enumerate().filter(|(j, _)| assign[*j] % n == i).map(|(_, w)| w.to_string()).collect(),
            may_depend_on: None, deny: vec![], deny_normal: vec![], external_allow: None, external_deny: vec![], dir: None,
        }).collect();
        let cfg = Config { check_build: true, check_optional: true, skip_cfg_test: true, group_dev: true, layers, ..Config::default() };
        let ws: Vec<String> = WS.iter().map(|s| s.to_string()).collect();
        let ok = |a: usize, b: usize| {
            let e = Edge { from: WS[a].into(), to: WS[b].into(), kind: "normal", manifest: String::new(), optional: false };
            actual(&cfg, &ws, &[e]).is_empty()
        };
        for a in 0..WS.len() { for b in 0..WS.len() { for d in 0..WS.len() {
            if a != b && b != d && a != d && ok(a, b) && ok(b, d) {
                prop_assert!(ok(a, d), "{} -> {} -> {} but not direct", WS[a], WS[b], WS[d]);
            }
        }}}
    }
}

fn toml_list(l: &[String]) -> String {
    format!(
        "[{}]",
        l.iter()
            .map(|x| format!("{x:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn axis_toml(kind: &str, axis: &[Layer]) -> String {
    let mut t = String::new();
    for l in axis {
        t += &format!(
            "[[{kind}]]\nname = {:?}\ncrates = {}\n",
            l.name,
            toml_list(&l.crates)
        );
        if let Some(m) = &l.may_depend_on {
            // Names of layers that do not exist are config errors; they
            // match nothing, so dropping them keeps the oracle's meaning.
            let m: Vec<String> = m
                .iter()
                .filter(|n| axis.iter().any(|o| &o.name == *n))
                .cloned()
                .collect();
            t += &format!("may_depend_on = {}\n", toml_list(&m));
        }
        t += &format!(
            "deny = {}\ndeny_normal = {}\nexternal_deny = {}\n",
            toml_list(&l.deny),
            toml_list(&l.deny_normal),
            toml_list(&l.external_deny)
        );
        if let Some(a) = &l.external_allow {
            t += &format!("external_allow = {}\n", toml_list(a));
        }
    }
    t
}

fn config_toml(c: &Case) -> String {
    let f = c.cfg_flags;
    let mut t = format!(
        "allow_unknown_names = true\ncheck_dev = {}\ncheck_build = {}\ncheck_optional = {}\nlayer_dev = {}\ngroup_dev = {}\nrequire_layer = {}\nrequire_group = {}\n",
        f[0], f[1], f[2], f[3], f[4], f[5], f[6]
    );
    t += &axis_toml("layer", &c.layers);
    t += &axis_toml("group", &c.groups);
    for r in &c.rules {
        t += &format!("[[crate]]\nname = {:?}\n", r.name);
        if let Some(a) = &r.allow {
            t += &format!("allow = {}\n", toml_list(a));
        }
        t += &format!(
            "allow_dev = {}\ndeny = {}\ndeny_normal = {}\ndeny_reach = {}\nlayer_exempt = {}\ngroup_exempt = {}\n",
            toml_list(&r.allow_dev),
            toml_list(&r.deny),
            toml_list(&r.deny_normal),
            toml_list(&r.deny_reach),
            toml_list(&r.layer_exempt),
            toml_list(&r.group_exempt)
        );
    }
    t
}

/// End to end through real `cargo metadata`: workspace manifests with normal,
/// dev, build and optional path dependencies and renames (all acyclic).
mod end_to_end {
    use super::*;
    use cargo_strata::run;

    proptest! {
        #![proptest_config(ProptestConfig { failure_persistence: None, cases: 150, ..ProptestConfig::default() })]

        #[test]
        fn run_equals_oracle_on_real_workspaces(c in case(), renames in prop::collection::vec(any::<bool>(), 24)) {
            let mut c = c;
            // Internal, acyclic edges only (externals need a registry): from index > to index.
            c.edges.retain(|&(a, b, _, _)| b < WS.len() && a > b);
            for e in &mut c.edges { if e.2 == 1 { e.3 = false; } } // dev-dependencies cannot be optional
            // Overlapping layers or groups are a configuration error end to end
            // (the pure oracle keeps "first wins").
            for axis in [&c.layers, &c.groups] {
                for w in WS {
                    prop_assume!(axis.iter().filter(|l| l.crates.iter().any(|g| cargo_strata::config::glob_match(g, w))).count() <= 1);
                }
            }
            let (cfg, ws, edges) = c.build();
            let dir = tempfile::tempdir().unwrap();
            let mut members = Vec::new();
            for (i, w) in WS.iter().enumerate() {
                let mut m = format!("[package]\nname = {w:?}\nversion = \"0.0.0\"\nedition = \"2021\"\n");
                let mut seen = BTreeSet::new();
                for &(a, b, kd, opt) in c.edges.iter() {
                    if a != i || !seen.insert((b, kd)) { continue; }
                    let table = ["dependencies", "dev-dependencies", "build-dependencies"][kd];
                    let key = if renames[(i * WS.len() + b) % renames.len()] { format!("alias{b}") } else { WS[b].to_string() };
                    let pkg = if key != WS[b] { format!("package = {:?}\n", WS[b]) } else { String::new() };
                    m += &format!("[{table}.{key}]\npath = \"../{}\"\n{pkg}optional = {opt}\n", WS[b]);
                }
                std::fs::create_dir_all(dir.path().join(w).join("src")).unwrap();
                std::fs::write(dir.path().join(w).join("src/lib.rs"), "").unwrap();
                std::fs::write(dir.path().join(w).join("Cargo.toml"), m).unwrap();
                members.push(format!("{w:?}"));
            }
            std::fs::write(dir.path().join("Cargo.toml"), format!("[workspace]\nresolver = \"2\"\nmembers = [{}]\n", members.join(", "))).unwrap();
            std::fs::write(dir.path().join("strata.toml"), config_toml(&c)).unwrap();
            // Optional dependencies without a feature: cargo makes an implicit feature, fine.
            let o = run(Some(&dir.path().join("Cargo.toml")), None).unwrap();
            let re = |l: &str| -> Option<(String, String, String)> {
                let rest = l.strip_prefix("error[crate]: ")?;
                let rest = rest.split_once(": `")?.1;
                let (from, rest) = rest.split_once("` -> `")?;
                let (to, rest) = rest.split_once("` (")?;
                let (kind, _) = rest.split_once(" dependency)")?;
                Some((from.to_string(), to.to_string(), kind.to_string()))
            };
            let got: BTreeSet<_> = o.lines.iter().filter_map(|l| re(l)).collect();
            // The edges strata can see: those cargo accepted (deduplicated per table above).
            let mut want_edges = Vec::new();
            for e in &edges {
                let (a, b) = (WS.iter().position(|w| *w == e.from).unwrap(), WS.iter().position(|w| *w == e.to).unwrap());
                let kd = KINDS.iter().position(|k| *k == e.kind).unwrap();
                // First declaration of (from, to, kind) wins, as in the manifest writer.
                let first = c.edges.iter().find(|&&(x, y, k, _)| x == a && y == b && k == kd).unwrap();
                if first.3 == e.optional { want_edges.push(e.clone()); }
            }
            let want: BTreeSet<_> = oracle(&cfg, &ws, &want_edges).into_iter().map(|(_, f, t, k)| (f, t, k)).collect();
            prop_assert_eq!(&got, &want, "{}", config_toml(&c));
        }
    }
}
