//! `[[forbid]] attributes` against oracles. A generator writes files with
//! attributes in every position (outer, inner, spaced `# [..]`, statements,
//! fields, macro bodies, `verus!`, `macro_rules!` templates, multi-line
//! `cfg_attr`, nested `cfg_attr`, `unsafe(..)`, raw identifiers, doc
//! comments) next to text that must not match (comments, strings, plain
//! identifiers, attribute arguments, `cfg_attr` predicates, `#[$m]`), and
//! records each attribute it applied. `attribute_hits` must recover exactly
//! the forbidden ones; forbid verdicts (`allow`, `require_comment`,
//! `[[forbid.exempt]]`, `crates`, `dirs`) must equal an oracle written from
//! the README.

use cargo_strata::config::{Config, Forbid, ForbidExempt};
use cargo_strata::facts::{attribute_hits, lexical_hits};
use cargo_strata::lints::{CrateInfo, FileInfo, evaluate};
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Names an attribute may be forbidden by.
const NAMES: &[&str] = &[
    "path",
    "no_mangle",
    "export_name",
    "link",
    "link_section",
    "verifier::external_body",
    "inline",
    "doc",
    "cfg_attr",
];
/// Names a plain attribute is written with: `NAMES` minus `cfg_attr` (its
/// arguments are attributes, generated separately), plus near misses.
const PLAIN: &[&str] = &[
    "path",
    "no_mangle",
    "export_name",
    "link",
    "link_section",
    "verifier::external_body",
    "inline",
    "doc",
    "path_buf",
    "link_name",
    "verifier",
    "rustfmt::skip",
    "allow",
];
/// Arguments of a plain attribute; none of the names in them is applied.
const ARGS: &[&str] = &[
    "",
    " = \"x, path\"",
    "(a, path = \"b\")",
    "(no_mangle)",
    " = concat!(\"link\", \"_section\")",
];
/// `cfg_attr` predicates; names in them are not applied.
const PREDS: &[&str] = &[
    "test",
    "doc",
    "path",
    "feature = \"path\"",
    "all(unix, no_mangle)",
    "not(link_section)",
];

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

/// One attribute body and the names it applies.
fn meta(r: &mut Rng, depth: usize) -> (String, Vec<String>) {
    match r.below(if depth < 2 { 5 } else { 3 }) {
        0 | 1 => {
            let n = r.pick(PLAIN);
            let raw = if !n.contains("::") && r.chance(1, 6) {
                "r#"
            } else {
                ""
            };
            (format!("{raw}{n}{}", r.pick(ARGS)), vec![n.to_string()])
        }
        2 => {
            let n = r.pick(PLAIN);
            (format!("unsafe({n}{})", r.pick(ARGS)), vec![n.to_string()])
        }
        _ => {
            let mut text = format!("cfg_attr({}", r.pick(PREDS));
            let mut names = vec!["cfg_attr".to_string()];
            for _ in 0..1 + r.below(3) {
                let (t, n) = meta(r, depth + 1);
                text += &format!(", {t}");
                names.extend(n);
            }
            if r.chance(1, 3) {
                text += ",";
            }
            (text + ")", names)
        }
    }
}

/// A generated file and every attribute application `(name, line)`.
fn gen_file(r: &mut Rng) -> (String, Vec<(String, usize)>) {
    let mut lines: Vec<String> = Vec::new();
    let mut truth: Vec<(String, usize)> = Vec::new();
    for k in 0..1 + r.below(10) {
        let l = lines.len() + 1;
        let (m, names) = meta(r, 0);
        let mut at = |line: usize, names: &[String]| {
            truth.extend(names.iter().map(|n| (n.clone(), line)));
        };
        let hash = if r.chance(1, 4) { "# [" } else { "#[" };
        match r.below(15) {
            0 => {
                lines.push(format!("{hash}{m}]"));
                lines.push(format!("fn f{k}() {{}}"));
                at(l, &names);
            }
            1 => {
                let bang = if r.chance(1, 4) { "# ! [" } else { "#![" };
                lines.push(format!("mod m{k} {{"));
                lines.push(format!("    {bang}{m}]"));
                lines.push("}".into());
                at(l + 1, &names);
            }
            2 => {
                lines.push(format!("fn g{k}() {{ {hash}{m}] let x = {hash}{m}] 1; }}"));
                at(l, &names);
                at(l, &names);
            }
            3 => {
                lines.push(format!("struct S{k} {{"));
                lines.push(format!("    {hash}{m}]"));
                lines.push("    a: u8,".into());
                lines.push("}".into());
                at(l + 1, &names);
            }
            4 => {
                lines.push(format!("some_macro! {{ {hash}{m}] fn h{k}() {{}} }}"));
                at(l, &names);
            }
            5 => {
                lines.push("verus! {".into());
                lines.push(format!("    {hash}{m}]"));
                lines.push(format!("    fn v{k}() ensures true {{}}"));
                lines.push("}".into());
                at(l + 1, &names);
            }
            6 => {
                lines.push("#[cfg_attr(".into());
                lines.push(format!("    {},", r.pick(PREDS)));
                lines.push(format!("    {m}"));
                lines.push(")]".into());
                lines.push(format!("fn w{k}() {{}}"));
                at(l, &["cfg_attr".to_string()]);
                at(l, &names);
            }
            7 => {
                lines.push("/// text #[path = \"x\"] no_mangle".into());
                lines.push(format!("fn d{k}() {{}}"));
                at(l, &["doc".to_string()]);
            }
            8 => {
                lines.push(format!("mod n{k} {{"));
                lines.push("    //! #[link_section] inner doc".into());
                lines.push("}".into());
                at(l + 1, &["doc".to_string()]);
            }
            9 => {
                lines.push(format!("// #[{m}] #![path]"));
                lines.push("/* #[no_mangle] */ /** */".into());
                // `/** */` is an empty outer doc comment: a `doc` attribute.
                at(l + 1, &["doc".to_string()]);
            }
            10 => lines.push(format!(
                "const C{k}: &str = \"#[path = \\\"x\\\"] #![no_mangle]\"; const R{k}: &str = r#\"#[{m}]\"#;"
            )),
            11 => lines.push(format!(
                "fn path{k}() {{ let no_mangle = x.link(); verifier::external_body(); quote!(#path [x]); }}"
            )),
            12 => {
                lines.push(format!(
                    "macro_rules! mr{k} {{ ($(#[$m:meta])* $i:item) => {{ $(#[$m])* {hash}{m}] $i }}; }}"
                ));
                at(l, &names);
            }
            13 => {
                let (m2, names2) = meta(r, 0);
                lines.push(format!("{hash}{m}] #[{m2}] fn two{k}() {{}}"));
                at(l, &names);
                at(l, &names2);
            }
            _ => lines.push(format!("fn plain{k}() {{ let path = 1; }}")),
        }
    }
    (lines.join("\n"), truth)
}

fn subset(r: &mut Rng, xs: &[&str]) -> Vec<String> {
    xs.iter()
        .filter(|_| r.chance(1, 2))
        .map(|s| s.to_string())
        .collect()
}

proptest! {
    #![proptest_config(cases(1500))]

    /// Extraction: the hits are exactly the generated applications of the
    /// forbidden names, with the line of the `#`.
    #[test]
    fn attribute_hits_match_generated_truth(seed in any::<u64>()) {
        let mut r = Rng(seed | 1);
        let (src, truth) = gen_file(&mut r);
        let forbidden = subset(&mut r, NAMES);
        let mut want: Vec<(String, usize)> =
            truth.into_iter().filter(|(n, _)| forbidden.contains(n)).collect();
        let mut got = attribute_hits(&src, &forbidden).unwrap();
        want.sort();
        got.sort();
        prop_assert_eq!(got, want, "forbidden {:?}\n{}", forbidden, src);
    }

    /// Verdicts: `allow`, `require_comment`, exemptions (pattern and
    /// attribute exemptions are distinct even for the same word), stale
    /// exemptions, `crates` and `dirs`, against an oracle.
    #[test]
    fn forbid_attribute_verdicts_match_oracle(seed in any::<u64>()) {
        let mut r = Rng(seed | 1);
        const ATTRS: &[&str] = &["path", "no_mangle", "link_section"];
        const TAG: &str = "TAG:";
        let nfiles = 1 + r.below(3);
        let rels: Vec<String> = (0..nfiles).map(|i| format!("c/src/f{i}.rs")).collect();
        let patterns: Vec<String> = if r.chance(1, 2) { vec!["path".into()] } else { vec![] };
        let mut attributes = subset(&mut r, ATTRS);
        if patterns.is_empty() && attributes.is_empty() {
            attributes.push("path".into());
        }
        let allow: Vec<String> = match r.below(3) {
            0 => vec![],
            1 => vec![rels[0].clone()],
            _ => vec!["c/src/f[01].rs".into()],
        };
        let allowed = |rel: &str| match allow.first().map(String::as_str) {
            None => false,
            Some("c/src/f[01].rs") => rel == "c/src/f0.rs" || rel == "c/src/f1.rs",
            Some(x) => rel == x,
        };
        let tag = r.chance(1, 2).then(|| TAG.to_string());
        let crate_hit = r.below(5) != 0;
        let dir_hit = r.below(5) != 0;

        // (file, is attribute, word, line, tagged)
        let mut hits: Vec<(String, bool, String, usize, bool)> = Vec::new();
        let mut files = Vec::new();
        for rel in &rels {
            let mut lines: Vec<String> = Vec::new();
            for k in 0..r.below(7) {
                lines.push(String::new());
                let l = lines.len() + 1;
                let a = r.pick(ATTRS);
                match r.below(5) {
                    0 => {
                        let t = r.chance(1, 2);
                        lines.push(format!("#[{a}] fn x{k}() {{}}{}", if t { " // TAG: ok" } else { "" }));
                        hits.push((rel.clone(), true, a.into(), l, t));
                        if a == "path" {
                            // The pattern `path` sees the identifier in `#[path]`.
                            hits.push((rel.clone(), false, a.into(), l, t));
                        }
                    }
                    1 => {
                        lines.push("// TAG: reason".into());
                        lines.push(format!("#[cfg_attr(test, unsafe({a}))]"));
                        lines.push(format!("fn y{k}() {{}}"));
                        hits.push((rel.clone(), true, a.into(), l + 1, true));
                        if a == "path" {
                            hits.push((rel.clone(), false, a.into(), l + 1, true));
                        }
                    }
                    2 => {
                        lines.push(format!("#[{a}] fn z{k}() {{}} /* TAG: */"));
                        hits.push((rel.clone(), true, a.into(), l, false));
                        if a == "path" {
                            hits.push((rel.clone(), false, a.into(), l, false));
                        }
                    }
                    3 => {
                        let t = r.chance(1, 2);
                        lines.push(format!("fn p{k}() {{ let path = 1; }}{}", if t { " // TAG:" } else { "" }));
                        hits.push((rel.clone(), false, "path".into(), l, t));
                    }
                    _ => lines.push("// TAG: a comment with nothing below".into()),
                }
            }
            let src = lines.join("\n");
            let (pat_hits, attr_hits) = lexical_hits(&src, &patterns, &attributes).unwrap();
            files.push(FileInfo {
                krate: "c".into(),
                rel: rel.clone(),
                crate_rel: rel.trim_start_matches("c/").into(),
                abs: PathBuf::from(format!("/w/{rel}")),
                src,
                facts: Default::default(),
                hits: pat_hits,
                attr_hits,
            });
        }
        hits.retain(|(_, attr, w, _, _)| if *attr { attributes.contains(w) } else { patterns.contains(w) });

        // Exemptions: listed words in random files (some stale).
        let mut exempt = Vec::new();
        for _ in 0..r.below(4) {
            let file = rels[r.below(rels.len())].clone();
            let attr = patterns.is_empty() || (!attributes.is_empty() && r.chance(1, 2));
            let e = if attr {
                ForbidExempt { file, pattern: None, attribute: Some(attributes[r.below(attributes.len())].clone()) }
            } else {
                ForbidExempt { file, pattern: Some(patterns[0].clone()), attribute: None }
            };
            exempt.push(e);
        }

        // Oracle.
        let mut want: BTreeSet<(String, usize, bool, String, &str)> = BTreeSet::new();
        let mut used: BTreeSet<(String, bool, String)> = BTreeSet::new();
        if crate_hit && dir_hit {
            for (file, attr, w, line, tagged) in &hits {
                let ex = |e: &ForbidExempt| {
                    &e.file == file && if *attr { e.attribute.as_ref() == Some(w) } else { e.pattern.as_ref() == Some(w) }
                };
                if !allowed(file) {
                    if exempt.iter().any(ex) {
                        used.insert((file.clone(), *attr, w.clone()));
                    } else {
                        want.insert((file.clone(), *line, *attr, w.clone(), "outside"));
                    }
                } else if tag.is_some() && !tagged {
                    want.insert((file.clone(), *line, *attr, w.clone(), "comment"));
                }
            }
        }
        for e in &exempt {
            let (attr, w) = match (&e.pattern, &e.attribute) {
                (Some(p), _) => (false, p.clone()),
                (_, Some(a)) => (true, a.clone()),
                _ => unreachable!(),
            };
            if !used.contains(&(e.file.clone(), attr, w.clone())) {
                want.insert((e.file.clone(), 0, attr, w, "stale"));
            }
        }

        let cfg = Config {
            forbids: vec![Forbid {
                name: "fb".into(),
                patterns: patterns.clone(),
                attributes: attributes.clone(),
                allow: allow.clone(),
                crates: vec![if crate_hit { "c".into() } else { "other".into() }],
                dirs: vec![if dir_hit { "src".into() } else { "tests".into() }],
                require_comment: tag.clone(),
                exempt: exempt.clone(),
            }],
            ..Default::default()
        };
        let crates = vec![CrateInfo { name: "c".into(), manifest_rel: "c/Cargo.toml".into(), dir_abs: PathBuf::from("/w/c"), roots: vec![], bins: vec![] }];
        let out = evaluate(&cfg, Path::new("/w"), &crates, &files, &BTreeMap::new(), "r.json");
        let mut got: BTreeSet<(String, usize, bool, String, &str)> = BTreeSet::new();
        for v in out.iter().filter(|v| v.kind == "forbid") {
            let attr = v.msg.contains("attribute `#[");
            let word = v.msg.split('`').nth(1).unwrap().trim_start_matches("#[").trim_end_matches(']').to_string();
            let class = if v.msg.starts_with("stale") { "stale" } else if v.msg.contains("outside") { "outside" } else { "comment" };
            got.insert((v.file.clone(), v.line, attr, word, class));
        }
        prop_assert_eq!(&got, &want, "{:#?}\n{:?}", files.iter().map(|f| f.src.clone()).collect::<Vec<_>>(), cfg.forbids);
    }
}

/// Worked examples from the README.
#[test]
fn attribute_examples() {
    let names: Vec<String> = [
        "path",
        "no_mangle",
        "export_name",
        "verifier::external_body",
    ]
    .map(String::from)
    .to_vec();
    let src = r##"#![cfg_attr(feature = "path", cfg_attr(unix, path = "x.rs"))]
#[unsafe(export_name = "f")] // path
fn f() {}
verus! { #[verifier::external_body] fn g() {} }
#[verifier(external_body)] fn h() {} // the name is `verifier`
const S: &str = "#[no_mangle]";
#[no_mangle_ish] fn path() {}
"##;
    let mut got = attribute_hits(src, &names).unwrap();
    got.sort();
    let want: Vec<(String, usize)> = vec![
        ("export_name".into(), 2),
        ("path".into(), 1),
        ("verifier::external_body".into(), 4),
    ];
    assert_eq!(got, want);
}
