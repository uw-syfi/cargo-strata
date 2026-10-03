//! Directory rules and token-pattern hits against oracles.

use cargo_strata::config::{Config, CrateRule, Layer};
use cargo_strata::dirs::{Pkg, evaluate};
use cargo_strata::facts::token_hits;
use proptest::prelude::*;
use std::collections::BTreeSet;

const DIRS: &[&str] = &["a", "a/b", "c", "ban", "ban/x", "ban/x/y", "banner", "d/e"];
const NAMES: &[&str] = &["p0", "p1", "p2", "p3", "p4", "p5"];
const KINDS: &[&str] = &["normal", "dev", "build"];

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

/// Plain-string oracle for "dir is base or below it".
fn under(dir: &str, base: &str) -> bool {
    dir == base || dir.starts_with(&format!("{base}/"))
}

proptest! {
    #![proptest_config(cases(1500))]

    #[test]
    fn reach_and_placement_match_oracle(
        dirs in prop::collection::vec(0usize..DIRS.len(), 6),
        member in prop::collection::vec(any::<bool>(), 6),
        edges in prop::collection::vec((0usize..6, 0usize..6, 0usize..3, any::<bool>()), 0..14),
        flags in prop::collection::vec(any::<bool>(), 3),
        deny in prop::collection::vec(0usize..DIRS.len(), 0..3),
        ruled in prop::collection::vec(any::<bool>(), 6),
        group_of in prop::collection::vec(0usize..3, 6),
        group_dirs in prop::collection::vec(prop::option::of(0usize..DIRS.len()), 3),
    ) {
        let pkgs: Vec<Pkg> = (0..6).map(|i| Pkg {
            name: NAMES[i].into(), dir: format!("{}/{}", DIRS[dirs[i]], NAMES[i]), manifest: format!("{}/Cargo.toml", NAMES[i]), member: member[i],
        }).collect();
        let es: Vec<(String, String, &'static str, bool)> = edges.iter()
            .map(|(a, b, k, o)| (NAMES[*a].to_string(), NAMES[*b].to_string(), KINDS[*k], *o && *k != 1)).collect();
        let denied: Vec<String> = deny.iter().map(|i| DIRS[*i].to_string()).collect();
        let groups: Vec<Layer> = (0..3).map(|g| Layer {
            name: format!("g{g}"),
            crates: (0..6).filter(|i| group_of[*i] == g).map(|i| NAMES[i].to_string()).collect(),
            dir: group_dirs[g].map(|d| DIRS[d].to_string()),
            ..Default::default()
        }).collect();
        let cfg = Config {
            check_dev: flags[0], check_build: flags[1], check_optional: flags[2],
            groups,
            crates: (0..6).filter(|i| ruled[*i]).map(|i| CrateRule { name: NAMES[i].into(), deny_reach_dir: denied.clone(), ..Default::default() }).collect(),
            ..Config::default()
        };
        let out = evaluate(&cfg, &pkgs, &es);

        // Oracle: reachability by repeated relaxation over the kept edges.
        let kept: Vec<(usize, usize)> = edges.iter().filter(|(_, _, k, o)| {
            (*k != 1 || flags[0]) && (*k != 2 || flags[1]) && (!*o || *k == 1 || flags[2])
        }).map(|(a, b, _, _)| (*a, *b)).collect();
        let mut reach = [[false; 6]; 6];
        for (a, b) in &kept { reach[*a][*b] = true; }
        for _ in 0..6 { for a in 0..6 { for b in 0..6 { for c in 0..6 {
            if reach[a][b] && reach[b][c] { reach[a][c] = true; }
        } } } }
        let mut want_reach = BTreeSet::new();
        for a in 0..6 {
            if !(member[a] && ruled[a]) { continue; }
            for b in 0..6 {
                if b != a && reach[a][b] && denied.iter().any(|d| under(&pkgs[b].dir, d)) {
                    want_reach.insert((NAMES[a].to_string(), NAMES[b].to_string()));
                }
            }
        }
        let got_reach: BTreeSet<(String, String)> = out.iter().filter(|v| v.kind == "reach").map(|v| {
            let hit = v.reason.split('`').nth(1).unwrap().to_string();
            (v.krate.clone(), hit)
        }).collect();
        prop_assert_eq!(got_reach, want_reach);

        let mut want_place = BTreeSet::new();
        for i in 0..6 {
            if let (true, Some(d)) = (member[i], group_dirs[group_of[i]]) {
                if !under(&pkgs[i].dir, DIRS[d]) { want_place.insert(NAMES[i].to_string()); }
            }
        }
        let got_place: BTreeSet<String> = out.iter().filter(|v| v.kind == "placement").map(|v| v.krate.clone()).collect();
        prop_assert_eq!(got_place, want_place);
    }

    /// Token patterns: hits equal the generated occurrences; comments,
    /// strings, methods and paths do not count where the pattern says so.
    #[test]
    fn token_hits_match_generated_truth(seed in any::<u64>()) {
        let mut s = seed | 1;
        let mut next = move || { s ^= s << 13; s ^= s >> 7; s ^= s << 17; s };
        let pats: Vec<String> = ["ext_body", "verifier::ext", "axiom fn", "assume(", "admit()"].map(String::from).to_vec();
        let mut lines: Vec<String> = Vec::new();
        let mut want: Vec<(String, usize)> = Vec::new();
        let mut n = 0usize;
        for _ in 0..1 + (next() % 12) as usize {
            let l = n + 1;
            match next() % 14 {
                0 => { lines.push("#[verifier::ext_body]".into()); want.push(("ext_body".into(), l)); }
                1 => { lines.push("#[verifier::ext]".into()); want.push(("verifier::ext".into(), l)); }
                2 => { lines.push("verus! { axiom fn lem() {} }".into()); want.push(("axiom fn".into(), l)); }
                3 => { lines.push("fn f() { assume(false); }".into()); want.push(("assume(".into(), l)); }
                4 => { lines.push("fn f() { x.assume(false); }".into()); }
                5 => { lines.push("fn f() { a::assume(false); }".into()); }
                6 => { lines.push("fn f() { admit(); }".into()); want.push(("admit()".into(), l)); }
                7 => { lines.push("fn f() { admit(1); }".into()); }
                8 => { lines.push("// ext_body assume(x) admit()".into()); }
                9 => { lines.push("const S: &str = \"ext_body admit()\";".into()); }
                10 => { lines.push("fn f() { let v = vec![assume(1)]; }".into()); want.push(("assume(".into(), l)); }
                11 => { lines.push("axiom\nfn lem2() {}".into()); want.push(("axiom fn".into(), l)); n += 1; }
                12 => { lines.push("fn assume_x(a: u8) { let ext_body_2 = 1; }".into()); }
                _ => { lines.push("fn g() { assume (true); }".into()); want.push(("assume(".into(), l)); }
            }
            n += 1;
        }
        let src = lines.join("\n");
        let mut got = token_hits(&src, &pats).unwrap();
        got.sort();
        want.sort();
        prop_assert_eq!(got, want, "{}", src);
    }
}
