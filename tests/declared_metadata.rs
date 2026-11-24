//! The unresolved read (`metadata::Reader::declared`) lists exactly the local
//! packages a full `cargo metadata` lists, each with the same declared
//! dependencies, on a checkout of two workspaces joined by path dependencies.

use cargo_metadata::{DependencyKind, Metadata};
use cargo_strata::metadata::{Reader, full};
use std::collections::BTreeMap;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// `name` at `dir` with `deps` (`[section] line`) and an empty library.
fn package(root: &Path, dir: &str, name: &str, deps: &[(&str, &str)]) {
    let mut text =
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    for (section, line) in deps {
        text.push_str(&format!("\n[{section}]\n{line}\n"));
    }
    write(root, &format!("{dir}/Cargo.toml"), &text);
    write(root, &format!("{dir}/src/lib.rs"), "");
}

/// A declared dependency: name, kind, optional, path.
type Declared = (String, DependencyKind, bool, Option<String>);

/// Each local package's manifest, mapped to its declared dependencies.
fn locals(meta: &Metadata) -> BTreeMap<String, Vec<Declared>> {
    meta.packages
        .iter()
        .filter(|p| p.source.is_none())
        .map(|p| {
            let mut deps: Vec<_> = p
                .dependencies
                .iter()
                .map(|d| {
                    (
                        d.name.clone(),
                        d.kind,
                        d.optional,
                        d.path.as_ref().map(|x| x.to_string()),
                    )
                })
                .collect();
            deps.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
            (p.manifest_path.to_string(), deps)
        })
        .collect()
}

#[test]
fn the_declared_read_lists_the_local_packages_of_a_full_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "lib/Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"core\", \"facade\", \"tool\", \"helper\"]\n",
    );
    package(root, "lib/core", "core", &[]);
    // A non-member reached by path: its dev-dependency is not resolved, so `helper` stays out of the version.
    package(
        root,
        "lib/facade",
        "facade",
        &[
            ("dependencies", "core = { path = \"../core\" }"),
            ("dev-dependencies", "helper = { path = \"../helper\" }"),
        ],
    );
    package(
        root,
        "lib/tool",
        "tool",
        &[("build-dependencies", "core = { path = \"../core\" }")],
    );
    package(root, "lib/helper", "helper", &[]);
    write(
        root,
        "v1/Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"app\"]\n",
    );
    package(
        root,
        "v1/app",
        "app",
        &[
            (
                "dependencies",
                "renamed = { package = \"facade\", path = \"../../lib/facade\" }",
            ),
            ("dev-dependencies", "tool = { path = \"../../lib/tool\" }"),
            (
                "target.'cfg(windows)'.dependencies",
                "core = { path = \"../../lib/core\" }",
            ),
        ],
    );

    let mut reader = Reader::default();
    for ws in ["lib", "v1"] {
        let manifest = root.join(ws).join("Cargo.toml");
        let (want, got) = (
            full(Some(&manifest)).unwrap(),
            reader.declared(Some(&manifest)).unwrap(),
        );
        assert_eq!(locals(&got), locals(&want), "{ws}");
        assert_eq!(got.workspace_members, want.workspace_members, "{ws}");
        assert!(got.resolve.is_none());
    }
    let v1 = reader.declared(Some(&root.join("v1/Cargo.toml"))).unwrap();
    assert!(
        !locals(&v1).keys().any(|m| m.contains("helper")),
        "a non-member's dev-dependency is not followed"
    );

    // A package inside v1's directory that v1 does not list: cargo reads it only as lib's dependency.
    package(root, "v1/stray", "stray", &[]);
    package(
        root,
        "lib/core",
        "core",
        &[("dev-dependencies", "stray = { path = \"../../v1/stray\" }")],
    );
    let manifest = root.join("lib/Cargo.toml");
    let (want, got) = (
        full(Some(&manifest)).unwrap(),
        Reader::default().declared(Some(&manifest)).unwrap(),
    );
    assert_eq!(
        locals(&got),
        locals(&want),
        "a path package cargo reads only through its dependent"
    );
}
