//! A file the parser rejects (nightly-only syntax) is a warning and the file
//! is skipped, unless `strict`; other files are still checked.

use cargo_strata::{is_syntax_error, run_with};

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let w = |rel: &str, text: &str| {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\"]\nresolver = \"2\"\n",
    );
    w(
        "a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    w("a/src/lib.rs", "mod bad;\nmod m;\nmod n;\n");
    // Default field values are nightly-only; `syn` rejects them.
    w("a/src/bad.rs", "pub struct S {\n    x: u32 = 1,\n}\n");
    w("a/src/m.rs", "use crate::n::f;\n");
    w("a/src/n.rs", "pub fn f() {}\n");
    w(
        "strata.toml",
        "[[crate]]\nname = \"a\"\n[[crate.module]]\npath = \"m\"\ndeny = [\"n\"]\n",
    );
    dir
}

#[test]
fn syntax_error_is_a_warning_by_default() {
    let dir = workspace();
    let o = run_with(Some(&dir.path().join("Cargo.toml")), None, false).unwrap();
    assert_eq!(o.errors, 0, "{:?}", o.lines);
    let w = o.warnings.join("\n");
    assert!(w.contains("bad.rs") && w.contains("parse error: 2:"), "{w}");
    // Other files are still checked.
    assert_eq!(o.violations, 1, "{:?}", o.lines);
    assert!(o.lines[0].contains("src/m.rs:1:"), "{:?}", o.lines);
}

#[test]
fn strict_makes_it_an_error() {
    let dir = workspace();
    let o = run_with(Some(&dir.path().join("Cargo.toml")), None, true).unwrap();
    assert_eq!(o.errors, 1, "{:?}", o.lines);
    assert!(
        o.lines
            .iter()
            .any(|l| l.starts_with("error[parse]:") && l.contains("bad.rs")),
        "{:?}",
        o.lines
    );
    assert!(
        o.warnings.iter().all(|w| !w.contains("bad.rs")),
        "{:?}",
        o.warnings
    );
}

#[test]
fn only_parser_messages_are_syntax_errors() {
    assert!(is_syntax_error("src/x.rs: parse error: 12:5: expected `;`"));
    assert!(is_syntax_error(
        "x.rs: parse error: verus! body: 3:1: unexpected token"
    ));
    assert!(!is_syntax_error(
        "x.rs: parse error: source larger than 2097152 bytes"
    ));
    assert!(!is_syntax_error("x.rs: cannot read: permission denied"));
    assert!(!is_syntax_error(
        "x.rs: parse error: nesting deeper than 256"
    ));
}
