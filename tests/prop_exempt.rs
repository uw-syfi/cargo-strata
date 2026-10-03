//! `exempt` on a module rule: a reference that `deny` rejects is allowed when
//! an exempt entry is a prefix of its target, and an entry that allows nothing
//! is reported as stale.
//!
//! Module `p` references random modules (`use crate::<m>::Item;`, one per
//! line) under a random `deny` list and a random `exempt` list. The oracle is
//! written from the definition: a reference is denied if a deny entry is a
//! prefix of its target, exempt if an exempt entry is; violations are denied
//! and not exempt, and an exempt entry is stale unless it covers some denied
//! reference.

use cargo_strata::config::ModuleRule;
use cargo_strata::modules::{ModuleUnit, evaluate};
use cargo_strata::scan::scan_source;
use proptest::prelude::*;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

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

const MODS: &[&str] = &["a", "a::x", "a::y", "b", "b::x", "b::x::z", "c"];

fn is_prefix(p: &str, of: &str) -> bool {
    of == p || of.starts_with(&format!("{p}::"))
}

proptest! {
    #![proptest_config(cases(400))]

    #[test]
    fn exempt_matches_oracle(
        refs in proptest::collection::vec(0..MODS.len(), 0..10),
        deny in proptest::collection::vec(0..MODS.len(), 0..4),
        exempt in proptest::collection::vec(0..MODS.len(), 0..4),
    ) {
        let src: String = refs.iter().map(|&i| format!("use crate::{}::Item;\n", MODS[i])).collect();
        let scan = scan_source(&src, false, true).unwrap();
        let units = [ModuleUnit { file: PathBuf::from("src/p.rs"), module: vec!["p".into()], scan: &scan }];
        let mut known: HashSet<Vec<String>> = HashSet::new();
        known.insert(vec![]);
        known.insert(vec!["p".into()]);
        for m in MODS {
            let segs: Vec<String> = m.split("::").map(String::from).collect();
            for n in 1..=segs.len() {
                known.insert(segs[..n].to_vec());
            }
        }
        let rule = ModuleRule {
            path: "p".into(),
            depends_on: None,
            deny: deny.iter().map(|&i| MODS[i].to_string()).collect(),
            exempt: exempt.iter().map(|&i| MODS[i].to_string()).collect(),
        };
        let (viol, _) = evaluate("k", &[&rule], &known, &units, Path::new("."));

        let denied = |m: &str| rule.deny.iter().any(|d| is_prefix(d, m));
        let exempted = |m: &str| rule.exempt.iter().any(|e| is_prefix(e, m));
        let want: BTreeSet<(usize, String)> = refs.iter().enumerate()
            .filter(|(_, i)| denied(MODS[**i]) && !exempted(MODS[**i]))
            .map(|(k, i)| (k + 1, MODS[*i].to_string()))
            .collect();
        let got: BTreeSet<(usize, String)> = viol.iter().filter(|v| !v.stale)
            .map(|v| (v.line, v.to.clone())).collect();
        prop_assert_eq!(got, want);

        let want_stale: BTreeSet<String> = rule.exempt.iter()
            .filter(|e| !refs.iter().any(|&i| denied(MODS[i]) && is_prefix(e, MODS[i])))
            .cloned().collect();
        let got_stale: BTreeSet<String> = viol.iter().filter(|v| v.stale).map(|v| v.to.clone()).collect();
        prop_assert_eq!(got_stale, want_stale);
    }
}
