//! Glob patterns in module rule paths (`path`, `depends_on`, `deny`,
//! `exempt`). A segment `*` (or `?`, `[..]`) matches one module name; a
//! pattern `path` makes one rule per module it matches, each with its own
//! subtree; the most specific rule governs a module, compared segment by
//! segment from the root (a name beats a pattern beats no segment).
//!
//! The layout is the motivating one: a crate of tools, some top-level, some
//! grouped as `<group>::<tool>`, and a `wiring` module that composes them.
//! Each tool may reference only itself; nothing references the wiring.

use cargo_strata::config::ModuleRule;
use cargo_strata::modules::{ModuleUnit, ambiguous_rules, evaluate};
use cargo_strata::scan::scan_source;
use cargo_strata::{Outcome, run};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A workspace with one crate `tools` holding `files` (paths under `src/`)
/// and `toml` as strata.toml.
fn check(files: &[(&str, &str)], toml: &str) -> Result<Outcome, String> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"tools\"]\nresolver = \"2\"\n",
    );
    write(
        root,
        "tools/Cargo.toml",
        "[package]\nname = \"tools\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    for (p, s) in files {
        write(root, &format!("tools/src/{p}"), s);
    }
    write(root, "strata.toml", toml);
    run(Some(&root.join("Cargo.toml")), None)
}

/// The tools crate: `raise_memory` (top-level tool), groups `template`
/// (tools `a`, `b`, `r#type`) and `probes` (tool `c`), and `wiring`
/// (one submodule per group). `extra` replaces files by path.
fn tools<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let base: Vec<(&str, &str)> = vec![
        (
            "lib.rs",
            "mod raise_memory;\nmod template;\nmod probes;\nmod wiring;\n\
             pub use template::b::g;\npub fn root_helper() {}\n",
        ),
        (
            "raise_memory.rs",
            "pub fn raise() { crate::root_helper(); }\n",
        ),
        (
            "template.rs",
            "pub mod a;\npub mod b;\npub mod r#type;\npub fn shared() {}\n",
        ),
        (
            "template/a.rs",
            "mod helpers;\n\
             pub fn f() { self::helpers::h(); crate::template::a::helpers::h(); helpers::h(); }\n\
             pub static NAME: &'static str = \"a\";\n\
             pub fn hold<T: 'static>(t: T) -> T { t }\n",
        ),
        ("template/a/helpers.rs", "pub fn h() {}\n"),
        ("template/b.rs", "pub fn g() {}\n"),
        ("template/type.rs", "pub fn t() {}\n"),
        ("probes.rs", "pub mod c;\n"),
        ("probes/c.rs", "pub fn c() {}\n"),
        (
            "wiring.rs",
            "mod template;\nmod probes;\npub fn wire() { crate::raise_memory::raise(); }\n",
        ),
        (
            "wiring/template.rs",
            "use crate::template::{a, b, r#type};\n\
             pub fn wire() { a::f(); b::g(); r#type::t(); }\n",
        ),
        (
            "wiring/probes.rs",
            "pub fn wire() { crate::probes::c::c(); }\n",
        ),
    ];
    let mut out: Vec<(&str, &str)> = base
        .into_iter()
        .filter(|(p, _)| !extra.iter().any(|(q, _)| q == p))
        .collect();
    out.extend_from_slice(extra);
    out
}

/// Each top-level module and each grouped tool references only itself; the
/// wiring references anything.
const ISOLATED: &str = r#"
[[crate]]
name = "tools"

[[crate.module]]
path = "*"
depends_on = []
deny = ["*"]

[[crate.module]]
path = "*::*"
depends_on = []
deny = ["*"]

[[crate.module]]
path = "wiring"
depends_on = ["*"]
"#;

fn module_errors(o: &Outcome) -> Vec<&String> {
    o.lines
        .iter()
        .filter(|l| l.starts_with("error[module]"))
        .collect()
}

/// Exactly one module violation, from `file:line`, naming `to` and `why`.
fn one_violation(extra: &[(&str, &str)], at: &str, to: &str, why: &str) {
    let o = check(&tools(extra), ISOLATED).unwrap();
    let errs = module_errors(&o);
    assert_eq!(errs.len(), 1, "{:#?}", o.lines);
    let e = errs[0];
    assert!(
        e.starts_with(&format!("error[module]: tools/src/{at}: ")),
        "{e}"
    );
    assert!(e.contains(&format!("references `{to}`")), "{e}");
    assert!(e.contains(why), "{e}");
}

// ---------- positive ----------

#[test]
fn isolated_layout_is_clean() {
    // Own subtree through `self::`, `crate::` and a bare child path; the
    // wiring through a use group with a raw identifier; root items; lifetimes.
    let o = check(&tools(&[]), ISOLATED).unwrap();
    assert_eq!(o.lines, Vec::<String>::new());
    assert_eq!((o.violations, o.errors), (0, 0));
}

// ---------- negative ----------

#[test]
fn sibling_in_group_via_super() {
    one_violation(
        &[(
            "template/a/helpers.rs",
            "pub fn h() { super::super::b::g(); }\n",
        )],
        "template/a/helpers.rs:1",
        "template::b",
        "`template::a` (rule `*::*`) denies `*`",
    );
}

#[test]
fn sibling_in_other_group_via_crate_path() {
    one_violation(
        &[("template/b.rs", "pub fn g() { crate::probes::c::c(); }\n")],
        "template/b.rs:1",
        "probes::c",
        "`template::b` (rule `*::*`) denies `*`",
    );
}

#[test]
fn sibling_through_use_of_self_relative_path() {
    // `use self::..` from the group module itself is fine (its own subtree),
    // but a tool reaching its group's module is not.
    one_violation(
        &[(
            "template/b.rs",
            "use super::shared;\npub fn g() { shared(); }\n",
        )],
        "template/b.rs:1",
        "template",
        "`template::b` (rule `*::*`) denies `*`",
    );
}

#[test]
fn tool_reaching_the_wiring() {
    one_violation(
        &[(
            "raise_memory.rs",
            "pub fn raise() { crate::wiring::wire(); }\n",
        )],
        "raise_memory.rs:1",
        "wiring",
        "`raise_memory` (rule `*`) denies `*`",
    );
}

#[test]
fn top_level_tool_reaching_a_grouped_tool() {
    one_violation(
        &[(
            "raise_memory.rs",
            "use crate::template::a;\npub fn raise() { a::f(); }\n",
        )],
        "raise_memory.rs:1",
        "template::a",
        "`raise_memory` (rule `*`) denies `*`",
    );
}

#[test]
fn sibling_through_crate_root_reexport() {
    // `crate::g` is a root item as written (ignored), but resolves to the
    // function defined in `template::b`; `deny = ["*"]` catches it.
    one_violation(
        &[("template/type.rs", "pub fn t() { crate::g(); }\n")],
        "template/type.rs:1",
        "template::b",
        "the name resolves to `template::b`",
    );
}

#[test]
fn sibling_through_raw_identifiers() {
    one_violation(
        &[(
            "template/a/helpers.rs",
            "pub fn h() { crate::r#template::r#type::t(); }\n",
        )],
        "template/a/helpers.rs:1",
        "template::type",
        "`template::a` (rule `*::*`) denies `*`",
    );
}

#[test]
fn raw_module_is_matched_by_a_pattern() {
    // `mod r#type;` is module `type`; `*::*` governs it like any tool.
    one_violation(
        &[("template/type.rs", "pub fn t() { super::a::f(); }\n")],
        "template/type.rs:1",
        "template::a",
        "`template::type` (rule `*::*`) denies `*`",
    );
}

#[test]
fn nested_module_of_a_tool_is_governed_by_the_tool_rule() {
    one_violation(
        &[(
            "template/a/helpers.rs",
            "use crate::probes::c::c;\npub fn h() { c(); }\n",
        )],
        "template/a/helpers.rs:1",
        "probes::c",
        "`template::a` (rule `*::*`) denies `*`",
    );
}

#[test]
fn lifetimes_and_static_items_are_not_references() {
    // A module named `r#static` beside `'static` lifetimes and `static`
    // items: only the real path is a reference.
    let o = check(
        &tools(&[
            ("probes.rs", "pub mod c;\npub mod r#static;\n"),
            (
                "probes/static.rs",
                "pub static S: &'static str = \"s\";\n\
                 pub fn s<T: 'static>() -> &'static str { S }\n\
                 pub fn bad() { crate::probes::c::c(); }\n",
            ),
        ]),
        ISOLATED,
    )
    .unwrap();
    let errs = module_errors(&o);
    assert_eq!(errs.len(), 1, "{:#?}", o.lines);
    assert!(
        errs[0].starts_with("error[module]: tools/src/probes/static.rs:3: module `probes::static` references `probes::c`"),
        "{}",
        errs[0]
    );
}

// ---------- specificity ----------

#[test]
fn a_literal_rule_beats_a_deeper_pattern() {
    // `wiring::template` matches `*::*` (two segments) and `wiring` (one);
    // `wiring` is a name where `*::*` has a pattern, so it governs. Without
    // it, the wiring's submodules would be tools.
    let without_wiring = ISOLATED.replace(
        "[[crate.module]]\npath = \"wiring\"\ndepends_on = [\"*\"]\n",
        "",
    );
    let o = check(&tools(&[]), &without_wiring).unwrap();
    let errs = module_errors(&o);
    assert!(
        errs.iter().any(|e| e.contains(
            "module `wiring::template` references `template::a` (via `crate::template::a`): `wiring::template` (rule `*::*`) denies `*`"
        )),
        "{errs:#?}"
    );
    let o = check(&tools(&[]), ISOLATED).unwrap();
    assert_eq!(module_errors(&o), Vec::<&String>::new());
}

#[test]
fn a_longer_rule_with_the_same_names_wins() {
    // `template::*` beats `template` on `template::a`; `template` still
    // governs `template` itself.
    let toml = r#"
[[crate]]
name = "tools"

[[crate.module]]
path = "template"
deny = ["probes"]

[[crate.module]]
path = "template::*"
deny = ["template::*"]
"#;
    let o = check(
        &tools(&[
            ("template.rs", "pub mod a;\npub mod b;\npub mod r#type;\npub fn shared() { crate::probes::c::c(); }\n"),
            ("template/b.rs", "pub fn g() { super::a::f(); crate::probes::c::c(); }\n"),
        ]),
        toml,
    )
    .unwrap();
    let errs: Vec<&str> = module_errors(&o).into_iter().map(String::as_str).collect();
    assert_eq!(
        errs,
        [
            "error[module]: tools/src/template/b.rs:1: module `template::b` references `template::a` (via `super::a::f`): `template::b` (rule `template::*`) denies `template::*`",
            "error[module]: tools/src/template.rs:4: module `template` references `probes::c` (via `crate::probes::c::c`): `template` denies `probes`",
        ],
        "{:#?}",
        o.lines
    );
}

#[test]
fn equally_specific_patterns_are_a_config_error() {
    let toml = r#"
[[crate]]
name = "tools"

[[crate.module]]
path = "t*::*"
deny = ["probes"]

[[crate.module]]
path = "*e*::*"
deny = ["wiring"]
"#;
    let o = check(&tools(&[]), toml).unwrap();
    let errs: Vec<&String> = o
        .lines
        .iter()
        .filter(|l| l.starts_with("error[config]"))
        .collect();
    assert_eq!(errs.len(), 1, "{:#?}", o.lines);
    assert!(
        errs[0].ends_with(":10: crate `tools`: module `template::a` matches module rules `t*::*` and `*e*::*` equally specifically; make one of them more specific or merge them"),
        "{}",
        errs[0]
    );
    assert_eq!(o.errors, 1);
}

// ---------- lists ----------

#[test]
fn patterns_in_depends_on_and_exempt() {
    // Tools may use any group module (`depends_on = ["*"]` would allow all
    // top-level modules; `"t*"` only `template`); one grandfathered edge is
    // exempt by pattern, and a pattern exemption that covers nothing is stale.
    let toml = r#"
[[crate]]
name = "tools"

[[crate.module]]
path = "*::*"
depends_on = ["t*"]
exempt = ["probes::?", "wiring::*"]
"#;
    let o = check(
        &tools(&[
            (
                "template/b.rs",
                "pub fn g() { super::shared(); crate::probes::c::c(); }\n",
            ),
            (
                "probes/c.rs",
                "pub fn c() { crate::raise_memory::raise(); }\n",
            ),
        ]),
        toml,
    )
    .unwrap();
    let errs: Vec<&str> = module_errors(&o).into_iter().map(String::as_str).collect();
    assert_eq!(errs.len(), 2, "{:#?}", o.lines);
    assert!(
        errs.iter().any(|e| e.starts_with("error[module]: tools/src/probes/c.rs:1: module `probes::c` references `raise_memory` (via `crate::raise_memory::raise`): `probes::c` (rule `*::*`) does not list it in depends_on")),
        "{errs:#?}"
    );
    assert!(
        errs.iter().any(|e| e.contains(":8: crate `tools`: module `*::*`: stale exemption of `wiring::*`")),
        "{errs:#?}"
    );
}

// ---------- configuration errors ----------

fn config_error(toml: &str) -> String {
    match check(&tools(&[]), toml) {
        Err(e) => e,
        Ok(o) => o.lines.join("\n"),
    }
}

#[test]
fn pattern_matching_no_module_is_an_error() {
    let e = config_error(
        "[[crate]]\nname = \"tools\"\n\n[[crate.module]]\npath = \"*::*::*::*\"\ndeny = [\"x*\"]\n",
    );
    assert!(
        e.contains(":5: crate `tools`: module rule `*::*::*::*` matches no module"),
        "{e}"
    );
    assert!(
        e.contains(
            ":6: crate `tools`: module rule `*::*::*::*`: `deny` names `x*`, which is not a module"
        ),
        "{e}"
    );
}

#[test]
fn malformed_patterns_are_errors() {
    let e = config_error(
        "[[crate]]\nname = \"tools\"\n\n[[crate.module]]\npath = \"**\"\ndeny = [\n  \"a\",\n  \"[ab\",\n]\n",
    );
    assert!(
        e.contains(":5: module rule `**`: `path` entry `**` is not a module pattern: `**`: `**` is not supported; `*` matches one module name"),
        "{e}"
    );
    assert!(
        e.contains(":8: module rule `**`: `deny` entry `[ab` is not a module pattern: `[ab`: "),
        "{e}"
    );
}

#[test]
fn crate_prefix_and_raw_names_in_patterns() {
    // `crate::*::*` is `*::*`; `r#template::*` names template's children.
    let toml = r#"
[[crate]]
name = "tools"

[[crate.module]]
path = "crate::r#template::*"
deny = ["crate::*::c"]
"#;
    let o = check(
        &tools(&[("template/b.rs", "pub fn g() { crate::probes::c::c(); }\n")]),
        toml,
    )
    .unwrap();
    let errs: Vec<&str> = module_errors(&o).into_iter().map(String::as_str).collect();
    assert_eq!(
        errs,
        [
            "error[module]: tools/src/template/b.rs:1: module `template::b` references `probes::c` (via `crate::probes::c::c`): `template::b` (rule `crate::r#template::*`) denies `crate::*::c`"
        ],
        "{:#?}",
        o.lines
    );
}

// ---------- exhaustive oracle over the pure evaluator ----------

const MODS: &[&str] = &["a", "a::x", "a::y", "a::x::z", "b", "b::x", "c"];
const PATS: &[&str] = &[
    "*", "a", "a::*", "*::x", "*::*", "[ab]::*", "?", "b", "a::x", "*::*::*",
];

/// Minimal glob for one segment: `*`, `?`, `[..]` of single characters.
fn seg_match(p: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') => (0..=s.len()).any(|i| go(&p[1..], &s[i..])),
            Some(b'?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some(b'[') => {
                let end = p.iter().position(|&c| c == b']').unwrap();
                !s.is_empty() && p[1..end].contains(&s[0]) && go(&p[end + 1..], &s[1..])
            }
            Some(&c) => s.first() == Some(&c) && go(&p[1..], &s[1..]),
        }
    }
    go(p.as_bytes(), s.as_bytes())
}

fn segs(m: &str) -> Vec<&str> {
    m.split("::").collect()
}

/// Pattern `p` covers module `m`: it matches `m`'s first segments.
fn covers(p: &str, m: &str) -> bool {
    let (p, m) = (segs(p), segs(m));
    m.len() >= p.len() && p.iter().zip(&m).all(|(a, b)| seg_match(a, b))
}

fn rank(p: &str) -> Vec<u8> {
    segs(p)
        .iter()
        .map(|s| if s.contains(['*', '?', '[']) { 1 } else { 2 })
        .collect()
}

#[test]
fn every_pattern_pair_matches_the_oracle() {
    let mut known: HashSet<Vec<String>> = HashSet::new();
    known.insert(vec![]);
    for m in MODS {
        known.insert(segs(m).into_iter().map(String::from).collect());
    }
    // Each module's file references every module.
    let src: String = MODS
        .iter()
        .map(|m| format!("use crate::{m}::Item;\n"))
        .collect();
    let scan = scan_source(&src, false, true).unwrap();
    let units: Vec<ModuleUnit> = MODS
        .iter()
        .map(|m| ModuleUnit {
            file: PathBuf::from(format!("src/{}.rs", m.replace("::", "/"))),
            module: segs(m).into_iter().map(String::from).collect(),
            scan: &scan,
        })
        .collect();
    let mut cases = 0;
    for p1 in PATS {
        for d1 in PATS {
            for p2 in PATS {
                for d2 in PATS {
                    let rules = [
                        ModuleRule {
                            path: p1.to_string(),
                            depends_on: None,
                            deny: vec![d1.to_string()],
                            exempt: vec![],
                        },
                        ModuleRule {
                            path: p2.to_string(),
                            depends_on: None,
                            deny: vec![d2.to_string()],
                            exempt: vec![],
                        },
                    ];
                    let refs: Vec<&ModuleRule> = rules.iter().collect();
                    let (viol, _) = evaluate("k", &refs, &known, &units, Path::new("."));
                    let got: BTreeSet<(String, String)> = viol
                        .iter()
                        .map(|v| (v.from.clone(), v.to.clone()))
                        .collect();

                    let mut want: BTreeSet<(String, String)> = BTreeSet::new();
                    let mut tie = false;
                    for cur in MODS {
                        // Governing rule: most specific; the later one on a tie.
                        let cands: Vec<&ModuleRule> =
                            rules.iter().filter(|r| covers(&r.path, cur)).collect();
                        let Some(best) = cands.iter().map(|r| rank(&r.path)).max() else {
                            continue;
                        };
                        let top: Vec<&&ModuleRule> =
                            cands.iter().filter(|r| rank(&r.path) == best).collect();
                        if top.len() > 1 && top[0].path != top[1].path {
                            tie = true;
                        }
                        let rule = top[top.len() - 1];
                        let own = segs(cur)[..best.len()].join("::");
                        for t in MODS {
                            if covers(&own, t) {
                                continue;
                            }
                            if covers(&rule.deny[0], t) {
                                want.insert((cur.to_string(), t.to_string()));
                            }
                        }
                    }
                    assert_eq!(got, want, "rules {p1}/{d1}, {p2}/{d2}");
                    assert_eq!(
                        !ambiguous_rules(&refs, &known).is_empty(),
                        tie,
                        "rules {p1}, {p2}"
                    );
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, PATS.len().pow(4));
}
