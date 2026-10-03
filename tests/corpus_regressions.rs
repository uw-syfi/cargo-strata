//! Regressions found by running strata on real external workspaces
//! (`tests/corpus.toml`), minimized to temporary workspaces.

/// Write `files` under a fresh single-package workspace named `w` and run
/// strata with `config` on it.
fn run_ws(files: &[(&str, &str)], config: &str) -> cargo_strata::Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write(
        "Cargo.toml",
        "[package]\nname = \"w\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    for (rel, text) in files {
        write(rel, text);
    }
    write("strata.toml", config);
    cargo_strata::run(Some(&root.join("Cargo.toml")), None).expect("run")
}

fn text(o: &cargo_strata::Outcome) -> String {
    format!("{:?}\n{}", o.warnings, o.lines.join("\n"))
}

/// rust-analyzer has `mod r#type;` in `type.rs`. The raw-identifier prefix
/// is not part of the file name: the file was reported missing and its
/// references never checked.
#[test]
fn raw_identifier_module_is_found_by_file_name() {
    let o = run_ws(
        &[
            ("src/lib.rs", "pub mod other;\nmod r#type;\n"),
            ("src/other.rs", "pub fn f() {}\n"),
            ("src/type.rs", "use crate::other::f;\npub fn g() { f() }\n"),
        ],
        "[[crate]]\nname = \"w\"\n[[crate.module]]\npath = \"r#type\"\ndeny = [\"other\"]\n",
    );
    let t = text(&o);
    assert!(!t.contains("has no file"), "{t}");
    assert_eq!(o.violations, 1, "{t}");
    assert!(t.contains("src/type.rs:1:"), "{t}");
}

#[test]
fn raw_identifier_inline_parent_and_dir_module() {
    let o = run_ws(
        &[
            (
                "src/lib.rs",
                "pub mod other;\nmod r#mod { pub mod inner; }\n",
            ),
            ("src/other.rs", "pub fn f() {}\n"),
            (
                "src/mod/inner.rs",
                "use crate::other::f;\npub fn g() { f() }\n",
            ),
        ],
        "[[crate]]\nname = \"w\"\n[[crate.module]]\npath = \"r#mod::inner\"\ndeny = [\"other\"]\n",
    );
    let t = text(&o);
    assert!(!t.contains("has no file"), "{t}");
    assert_eq!(o.violations, 1, "{t}");
}

/// A reference inside a multi-line `use crate::{ .. }` is reported on the line
/// of the leaf, not on the `use` keyword (found on wasmtime/winch).
#[test]
fn use_group_reference_is_reported_on_its_own_line() {
    let o = run_ws(
        &[
            ("src/lib.rs", "pub mod a;\npub mod b;\n"),
            ("src/b.rs", "pub struct S;\n"),
            ("src/a.rs", "use crate::{\n    a,\n    b::S,\n};\n"),
        ],
        "[[crate]]\nname = \"w\"\n[[crate.module]]\npath = \"a\"\ndeny = [\"b\"]\n",
    );
    let t = text(&o);
    assert_eq!(o.violations, 1, "{t}");
    assert!(t.contains("src/a.rs:3:"), "{t}");
}

/// A dependency renamed in the manifest (`key = { package = "real" }`) is
/// reported at the key's line, not without a line (found on wasmtime).
#[test]
fn renamed_dependency_has_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let w = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "Cargo.toml",
        "[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n",
    );
    w(
        "a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\nshort-name = { path = \"../b\", package = \"b\" }\n",
    );
    w("a/src/lib.rs", "");
    w(
        "b/Cargo.toml",
        "[package]\nname = \"b\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    w("b/src/lib.rs", "");
    w("strata.toml", "[[crate]]\nname = \"a\"\nallow = []\n");
    let o = cargo_strata::run(Some(&root.join("Cargo.toml")), None).unwrap();
    let t = text(&o);
    assert_eq!(o.violations, 1, "{t}");
    assert!(t.contains("a/Cargo.toml:7:"), "{t}");
}
