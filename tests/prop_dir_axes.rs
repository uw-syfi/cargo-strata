//! Property tests for layers and groups held by directory: `DirPattern`
//! matching against a segment-wise oracle, and `crates::evaluate_local` with
//! `{name}` captures against an oracle written from the README.

use cargo_strata::config::Config;
use cargo_strata::crates::{Edge, Local, evaluate_local};
use cargo_strata::dirs::{Captures, DirPattern};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn cases(default: u32) -> ProptestConfig {
    let n = std::env::var("STRATA_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default);
    ProptestConfig::with_cases(n)
}

/// A pattern segment: a literal, `*`, or a capture.
#[derive(Debug, Clone)]
enum Seg {
    Lit(&'static str),
    Star,
    Cap(&'static str),
}

fn oracle_contains(pat: &[Seg], dir: &[&str], exact: bool) -> Option<Captures> {
    if dir.len() < pat.len() || exact && dir.len() != pat.len() {
        return None;
    }
    let mut caps = Captures::new();
    for (s, d) in pat.iter().zip(dir) {
        match s {
            Seg::Lit(l) if l == d => {}
            Seg::Lit(_) => return None,
            Seg::Star => {}
            Seg::Cap(n) => {
                caps.insert(n.to_string(), d.to_string());
            }
        }
    }
    Some(caps)
}

fn seg() -> impl Strategy<Value = Seg> {
    prop_oneof![
        prop::sample::select(vec!["a", "b", "c"]).prop_map(Seg::Lit),
        Just(Seg::Star),
        prop::sample::select(vec!["x", "y", "z"]).prop_map(Seg::Cap),
    ]
}

proptest! {
    #![proptest_config(cases(512))]

    #[test]
    fn dir_pattern_matches_oracle(
        pat in prop::collection::vec(seg(), 0..4),
        dir in prop::collection::vec(prop::sample::select(vec!["a", "b", "c"]), 0..5),
    ) {
        let mut seen = Vec::new();
        for s in &pat {
            if let Seg::Cap(n) = s {
                prop_assume!(!seen.contains(n));
                seen.push(*n);
            }
        }
        let text: Vec<String> = pat.iter().map(|s| match s {
            Seg::Lit(l) => l.to_string(),
            Seg::Star => "*".into(),
            Seg::Cap(n) => format!("{{{n}}}"),
        }).collect();
        let p = DirPattern::new(&text.join("/")).unwrap();
        let d = dir.join("/");
        prop_assert_eq!(p.contains(&d), oracle_contains(&pat, &dir, false));
        prop_assert_eq!(p.matches(&d), oracle_contains(&pat, &dir, true));
        // Substituting a match's own captures keeps exactly that instance.
        if let Some(caps) = p.matches(&d) {
            let q = p.substitute(&caps).unwrap();
            prop_assert_eq!(q.captures().count(), 0);
            prop_assert!(q.matches(&d).is_some());
        }
    }
}

const DOMAINS: [&str; 3] = ["x", "y", "z"];

/// Where a generated crate lives: the SDK, a domain, or a domain's version.
#[derive(Debug, Clone)]
enum Place {
    Sdk,
    Domain(usize),
    Version(usize, usize),
}

impl Place {
    fn dir(&self, i: usize) -> String {
        match self {
            Place::Sdk => format!("sdk/c{i}"),
            Place::Domain(d) => format!("dom/{}/c{i}", DOMAINS[*d]),
            Place::Version(d, v) => format!("ver/{}/v{v}/c{i}", DOMAINS[*d]),
        }
    }

    /// Entry index (sdk, dom, ver) and its captures.
    fn entry(&self) -> (usize, Captures) {
        let caps = |pairs: &[(&str, String)]| {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect()
        };
        match self {
            Place::Sdk => (0, Captures::new()),
            Place::Domain(d) => (1, caps(&[("d", DOMAINS[*d].into())])),
            Place::Version(d, v) => (
                2,
                caps(&[("d", DOMAINS[*d].into()), ("v", format!("v{v}"))]),
            ),
        }
    }
}

fn place() -> impl Strategy<Value = Place> {
    prop_oneof![
        Just(Place::Sdk),
        (0..3usize).prop_map(Place::Domain),
        (0..3usize, 1..3usize).prop_map(|(d, v)| Place::Version(d, v)),
    ]
}

const ENTRIES: [(&str, &str); 3] = [("sdk", "sdk"), ("dom", "dom/{d}"), ("ver", "ver/{d}/{v}")];

/// Groups with an explicit `may_depend_on` per entry (bit j of `allow[i]`:
/// entry i may depend on entry j), or ordered layers when `allow` is None.
fn config(allow: Option<&[u8; 3]>) -> Config {
    let mut text = String::new();
    for (i, (name, dir)) in ENTRIES.iter().enumerate() {
        text.push_str(&format!(
            "[[{}]]\nname = \"{name}\"\ndir = \"{dir}\"\n",
            if allow.is_some() { "group" } else { "layer" }
        ));
        if let Some(a) = allow {
            let deps: Vec<String> = (0..3)
                .filter(|j| a[i] >> j & 1 == 1)
                .map(|j| format!("\"{}\"", ENTRIES[j].0))
                .collect();
            text.push_str(&format!("may_depend_on = [{}]\n", deps.join(", ")));
        }
    }
    toml::from_str(&text).unwrap()
}

/// The README's rule: an edge between two held crates is allowed when the
/// target's entry is permitted by name (listed, or not higher for ordered
/// layers) and the two instances agree on every shared capture.
fn oracle(places: &[Place], allow: Option<&[u8; 3]>, edges: &[(usize, usize)]) -> usize {
    edges
        .iter()
        .filter(|(f, t)| {
            let (a, ca) = places[*f].entry();
            let (b, cb) = places[*t].entry();
            let by_name = match allow {
                Some(al) => al[a] >> b & 1 == 1,
                None => b <= a,
            };
            let agree = ca.iter().all(|(k, v)| cb.get(k).is_none_or(|w| w == v));
            !(by_name && agree)
        })
        .count()
}

proptest! {
    #![proptest_config(cases(256))]

    #[test]
    fn captured_axes_match_oracle(
        places in prop::collection::vec(place(), 2..9),
        members in prop::collection::vec(any::<bool>(), 9),
        allow in prop::option::of(prop::array::uniform3(0u8..8)),
        raw_edges in prop::collection::vec((0..9usize, 0..9usize), 0..16),
    ) {
        let n = places.len();
        let name = |i: usize| format!("c{i}");
        let locals: BTreeMap<String, Local> = (0..n)
            .map(|i| (name(i), Local {
                manifest: format!("{}/Cargo.toml", places[i].dir(i)),
                dir: Some(places[i].dir(i)),
                // The first crate is always a member, so some edges exist.
                member: i == 0 || members[i],
            }))
            .collect();
        let mut edges: Vec<(usize, usize)> = raw_edges
            .into_iter()
            .map(|(f, t)| (f % n, t % n))
            .filter(|(f, t)| f != t && locals[&name(*f)].member)
            .collect();
        edges.sort();
        edges.dedup();
        let cfg = config(allow.as_ref());
        let es: Vec<Edge> = edges.iter().map(|(f, t)| Edge {
            from: name(*f),
            to: name(*t),
            kind: "normal",
            manifest: String::new(),
            optional: false,
        }).collect();
        let what = if allow.is_some() { "group " } else { "layer " };
        let got = evaluate_local(&cfg, &locals, &es)
            .into_iter()
            .filter(|(_, r)| r.starts_with(what))
            .count();
        prop_assert_eq!(got, oracle(&places, allow.as_ref(), &edges));
    }
}
