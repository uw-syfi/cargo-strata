//! A raw identifier (`r#name`) names the same thing as `name`. Every rule
//! that compares names must see through the prefix, or writing `r#` is a
//! bypass. One case per rule kind, run end to end where a rule needs a
//! workspace.

use cargo_strata::facts::{attribute_hits, scan_facts, token_hits};
use cargo_strata::scan::scan_source;
use cargo_strata::{Outcome, run};
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A package: name, extra manifest text, `(path, source)` files.
type Pkg<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// A workspace of `pkgs` and a strata.toml.
fn check(pkgs: &[Pkg], toml: &str) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let members: Vec<String> = pkgs.iter().map(|(n, _, _)| format!("\"{n}\"")).collect();
    write(
        root,
        "Cargo.toml",
        &format!(
            "[workspace]\nmembers = [{}]\nresolver = \"2\"\n",
            members.join(", ")
        ),
    );
    for (name, extra, files) in pkgs {
        write(
            root,
            &format!("{name}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n{extra}"
            ),
        );
        for (p, s) in *files {
            write(root, &format!("{name}/{p}"), s);
        }
    }
    write(root, "strata.toml", toml);
    run(Some(&root.join("Cargo.toml")), None).unwrap()
}

fn kind<'a>(o: &'a Outcome, k: &str) -> Vec<&'a String> {
    o.lines
        .iter()
        .filter(|l| l.starts_with(&format!("error[{k}]")))
        .collect()
}

fn pats(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

// ---------- forbid patterns ----------

#[test]
fn forbid_pattern_matches_raw_identifiers() {
    let src = "fn f() {\n    let _ = r#include!(\"x.txt\");\n    let _: r#Justifies = todo!();\n    r#spawn(1);\n    std::r#process::exit(0);\n}\n";
    let hits = token_hits(
        src,
        &pats(&["include", "Justifies", "spawn(", "std::process::exit"]),
    )
    .unwrap();
    let mut got: Vec<(&str, usize)> = hits.iter().map(|(p, l)| (p.as_str(), *l)).collect();
    got.sort();
    assert_eq!(
        got,
        [
            ("Justifies", 3),
            ("include", 2),
            ("spawn(", 4),
            ("std::process::exit", 5)
        ]
    );
}

#[test]
fn forbid_pattern_written_raw_matches_both_spellings() {
    let hits = token_hits("fn f() { r#match(); match_(); }\n", &pats(&["r#match("])).unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
}

#[test]
fn forbid_end_to_end_raw_include_and_name() {
    let o = check(
        &[(
            "app",
            "",
            &[(
                "src/lib.rs",
                "pub fn f() -> &'static str { r#include_str!(\"lib.rs\") }\npub struct r#Justifies;\n",
            )],
        )],
        "[[forbid]]\nname = \"inc\"\npatterns = [\"include_str\"]\n\
         [[forbid]]\nname = \"just\"\npatterns = [\"Justifies\"]\n",
    );
    let l = kind(&o, "forbid");
    assert_eq!(l.len(), 2, "{:?}", o.lines);
}

// ---------- forbid attributes ----------

#[test]
fn forbid_attribute_configured_raw() {
    let src = "#[path = \"x.rs\"] mod a;\n#[r#path = \"y.rs\"] mod b;\n#[r#verifier::r#external_body] fn f() {}\n";
    let names = pats(&["r#path", "verifier::external_body"]);
    let mut hits = attribute_hits(src, &names).unwrap();
    hits.sort();
    assert_eq!(
        hits,
        [
            ("r#path".to_string(), 1),
            ("r#path".to_string(), 2),
            ("verifier::external_body".to_string(), 3)
        ]
    );
}

// ---------- AST facts: includes, path attributes, constructs, cfg(test) ----------

#[test]
fn facts_see_through_raw_identifiers() {
    let src = "#[r#path = \"x.rs\"] mod m;\n\
               fn f() {\n\
               let _ = r#include!(\"a.txt\");\n\
               let _ = r#Permit { a: 1 };\n\
               let _ = gate::r#Permit::r#new();\n\
               x.r#grant();\n\
               }\n";
    let f = scan_facts(src, false, true).unwrap();
    assert_eq!(f.path_attrs.len(), 1, "{:?}", f.path_attrs);
    assert_eq!(f.includes.len(), 1, "{:?}", f.includes);
    assert_eq!(f.includes[0].mac, "include");
    assert!(
        f.struct_lits.iter().any(|(n, _)| n == "Permit"),
        "{:?}",
        f.struct_lits
    );
    assert!(
        f.calls.iter().any(|(s, _)| s == &["gate", "Permit", "new"]),
        "{:?}",
        f.calls
    );
    assert!(
        f.methods.iter().any(|(n, _)| n == "grant"),
        "{:?}",
        f.methods
    );
}

#[test]
fn raw_cfg_test_is_skipped_like_cfg_test() {
    let src = "#[r#cfg(r#test)] use crate::x::T;\nuse crate::y::U;\n";
    let s = scan_source(src, false, true).unwrap();
    let segs: Vec<_> = s.refs.iter().map(|r| r.segs.join("::")).collect();
    assert_eq!(segs, ["crate::y::U"]);
}

#[test]
fn raw_unsafe_is_an_identifier_not_the_keyword() {
    let f = scan_facts(
        "fn f() { m!(r#unsafe { 1 }); m!(unsafe { 1 }); }\n",
        false,
        true,
    )
    .unwrap();
    assert_eq!(f.unsafe_sites.len(), 1, "{:?}", f.unsafe_sites);
}

// ---------- module rules ----------

fn modules_case(memory: &str, resolve: bool) -> Outcome {
    check(
        &[(
            "app",
            "",
            &[
                (
                    "src/lib.rs",
                    "pub mod raise_memory;\npub mod mem;\nmod r#other;\n",
                ),
                ("src/raise_memory.rs", "pub fn f() {}\n"),
                ("src/other.rs", "pub fn g() {}\n"),
                ("src/mem.rs", memory),
            ],
        )],
        &format!(
            "resolve = {resolve}\n[[crate]]\nname = \"app\"\n[[crate.module]]\npath = \"mem\"\ndeny = [\"raise_memory\", \"other\"]\n"
        ),
    )
}

#[test]
fn module_rule_sees_raw_paths() {
    for resolve in [false, true] {
        for src in [
            "pub fn h() { crate::r#raise_memory::f() }\n",
            "use crate::r#raise_memory::f;\n",
            "use crate::{r#raise_memory};\n",
            "use super::r#raise_memory as r;\n",
            "pub fn h() { m!(crate::r#raise_memory::f()) }\n",
            "pub fn h() { crate::other::g() }\n",
            "pub fn h() { crate::r#other::g() }\n",
        ] {
            let o = modules_case(src, resolve);
            assert_eq!(
                kind(&o, "module").len(),
                1,
                "resolve={resolve} {src}: {:?}",
                o.lines
            );
        }
    }
}

#[test]
fn module_config_names_may_be_written_raw() {
    let o = check(
        &[(
            "app",
            "",
            &[
                ("src/lib.rs", "pub mod other;\nmod r#type;\n"),
                ("src/other.rs", "pub fn f() {}\n"),
                ("src/type.rs", "use crate::other::f;\n"),
            ],
        )],
        "[[crate]]\nname = \"app\"\n[[crate.module]]\npath = \"type\"\ndeny = [\"r#other\"]\n",
    );
    assert_eq!(kind(&o, "module").len(), 1, "{:?}", o.lines);
}

// ---------- crate names in paths: public surface and re-exports ----------

#[test]
fn surface_and_reexport_see_raw_crate_names() {
    let o = check(
        &[
            (
                "base",
                "",
                &[
                    ("src/lib.rs", "pub mod api;\npub mod slab;\n"),
                    ("src/api.rs", "pub struct Slot;\n"),
                    ("src/slab.rs", "pub struct Slot;\n"),
                ],
            ),
            (
                "app",
                "[dependencies]\nbase = { path = \"../base\" }\n",
                &[(
                    "src/lib.rs",
                    "use r#base::r#slab::Slot;\npub use r#base::api::Slot as S;\n",
                )],
            ),
        ],
        "[[crate]]\nname = \"base\"\npublic_modules = [\"api\"]\n\
         [[crate]]\nname = \"app\"\ndeny_reexport = [\"base\"]\n",
    );
    assert_eq!(kind(&o, "surface").len(), 1, "{:?}", o.lines);
    assert_eq!(kind(&o, "reexport").len(), 1, "{:?}", o.lines);
}

#[cfg(feature = "verus")]
#[test]
fn module_rule_sees_raw_paths_in_verus() {
    let o = check(
        &[(
            "app",
            "",
            &[
                ("src/lib.rs", "pub mod raise_memory;\npub mod mem;\n"),
                ("src/raise_memory.rs", "pub fn f() {}\n"),
                ("src/mem.rs", "verus! { use crate::r#raise_memory::f; }\n"),
            ],
        )],
        "[[crate]]\nname = \"app\"\nverus = true\n[[crate.module]]\npath = \"mem\"\ndeny = [\"raise_memory\"]\n",
    );
    assert_eq!(kind(&o, "module").len(), 1, "{:?}", o.lines);
}
