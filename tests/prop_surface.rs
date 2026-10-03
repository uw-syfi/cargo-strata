//! `public_modules`: paths from a dependent crate into a target crate.
//!
//! A generator builds a random module tree for the target and a random public
//! subset, then writes paths into the target in several spellings (`use`
//! leaves, groups, globs, expression, type and pattern paths, inside and
//! outside `verus!`). Each path is built as module + trailing item segments
//! (capitalized, or a lowercase name that is never a module), so the module it
//! reaches is known by construction, not recomputed. The expected violations
//! are the paths whose module is neither the root nor public. Paths through
//! another crate or `crate::` must never be reported.

use cargo_strata::modules::ModuleUnit;
use cargo_strata::scan::scan_source;
use cargo_strata::surface::evaluate;
use proptest::prelude::*;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

const VERUS: bool = cfg!(feature = "verus");
const NAMES: &[&str] = &["a", "b", "c", "d"];

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

/// Trailing segments after a module: never a module name.
const TAILS: &[&[&str]] = &[&[], &["Item"], &["Item", "new"], &["f"], &["Kind", "V"]];

fn join(m: &[String], tail: &[&str]) -> String {
    m.iter()
        .map(String::as_str)
        .chain(tail.iter().copied())
        .collect::<Vec<_>>()
        .join("::")
}

/// One generated source with the (line, module) pairs it reaches in the target.
struct Generated {
    mods: Vec<Vec<String>>,
    public: Vec<Vec<String>>,
    src: String,
    expect: BTreeSet<(usize, String)>,
}

fn generate(seed: u64, wrap_verus: bool) -> Generated {
    let mut rng = Rng(seed | 1);
    // Module tree: each module's parent is the root or an earlier module.
    let mut mods: Vec<Vec<String>> = Vec::new();
    for _ in 0..rng.below(9) {
        let parent = if mods.is_empty() || rng.chance(1, 3) {
            vec![]
        } else {
            mods[rng.below(mods.len())].clone()
        };
        if parent.len() >= 3 {
            continue;
        }
        let mut m = parent;
        m.push(NAMES[rng.below(NAMES.len())].to_string());
        if !mods.contains(&m) {
            mods.push(m);
        }
    }
    let public: Vec<Vec<String>> = mods.iter().filter(|_| rng.chance(1, 2)).cloned().collect();
    let allowed = |m: &[String]| m.is_empty() || public.iter().any(|p| p == m);

    let mut lines: Vec<String> = Vec::new();
    let mut expect: BTreeSet<(usize, String)> = BTreeSet::new();
    let base = usize::from(wrap_verus); // `verus! {` takes the first line
    let pick = |rng: &mut Rng| -> Vec<String> {
        // The module a path reaches: root or a known module.
        if mods.is_empty() || rng.chance(1, 5) {
            vec![]
        } else {
            mods[rng.below(mods.len())].clone()
        }
    };
    for k in 0..rng.below(14) {
        let m = pick(&mut rng);
        let tail = TAILS[rng.below(TAILS.len())];
        let mut path = join(&m, tail);
        // A use of the module itself, with no tail, is a leaf at that module.
        let line_no = base + lines.len() + 1;
        let mut hits: Vec<Vec<String>> = vec![m.clone()];
        let text = match rng.below(8) {
            0 => format!(
                "use T::{};",
                if path.is_empty() {
                    "Root".to_string()
                } else {
                    path.clone()
                }
            ),
            1 => {
                // A group with a second leaf in another module.
                let m2 = pick(&mut rng);
                let p2 = join(&m2, &["Other"]);
                hits.push(m2);
                if path.is_empty() {
                    path = "Root".into();
                }
                format!("use T::{{{path}, {p2}}};")
            }
            2 => {
                // Glob: the module globbed (no tail).
                let p = join(&m, &[]);
                if p.is_empty() {
                    "use T::*;".to_string()
                } else {
                    format!("use T::{p}::*;")
                }
            }
            3 => format!("fn g{k}() {{ let _ = T::{}(); }}", join(&m, &["f"])),
            4 => format!("fn g{k}(x: T::{}) {{}}", join(&m, &["Item"])),
            5 => format!("fn g{k}() {{ let _ = T::{}::new(); }}", join(&m, &["Item"])),
            6 => format!("use U::{};", join(&m, &["Item"])),
            _ => format!("use crate::{};", join(&m, &["Item"])),
        };
        // Spellings 6 and 7 name another crate or `crate::`: never reported.
        let reaches = !text.starts_with("use U::") && !text.starts_with("use crate::");
        if reaches {
            for h in hits {
                if !allowed(&h) {
                    expect.insert((line_no, h.join("::")));
                }
            }
        }
        lines.push(text);
    }
    let src = if wrap_verus {
        format!("verus! {{\n{}\n}}\n", lines.join("\n"))
    } else {
        format!("{}\n", lines.join("\n"))
    };
    Generated {
        mods,
        public,
        src,
        expect,
    }
}

fn run(seed: u64, wrap_verus: bool) {
    let Generated {
        mods,
        public,
        src,
        expect,
    } = generate(seed, wrap_verus);
    let scan = scan_source(&src, wrap_verus && VERUS, true).expect("generated source parses");
    let units = [ModuleUnit {
        file: PathBuf::from("src/lib.rs"),
        module: vec![],
        scan: &scan,
    }];
    let mut known: HashSet<Vec<String>> = mods.iter().cloned().collect();
    known.insert(vec![]);
    let public: Vec<String> = public.iter().map(|p| p.join("::")).collect();
    let got: BTreeSet<(usize, String)> = evaluate("T", &public, &known, &units, Path::new("."))
        .into_iter()
        .map(|v| (v.line, v.module))
        .collect();
    assert_eq!(got, expect, "seed {seed}\n{src}\npublic {public:?}");
}

proptest! {
    #![proptest_config(cases(300))]

    #[test]
    fn surface_matches_construction(seed in any::<u64>()) {
        run(seed, false);
    }

    #[test]
    fn surface_matches_construction_in_verus(seed in any::<u64>()) {
        if VERUS {
            run(seed, true);
        }
    }
}
