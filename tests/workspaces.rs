//! Several workspaces under one config (`root`, `[[workspace]]`), layers and
//! groups held by directory with `{name}` captures, `path_packages`, and
//! `path_deps`, end to end on generated checkouts.

use cargo_strata::{Outcome, run_with};
use std::path::{Path, PathBuf};

/// A checkout with a library workspace `lib` (an SDK crate, an example and
/// one crate per domain) and one workspace per codebase version under
/// `versions/<domain>/<version>`, each depending on the SDK and its domain.
struct Checkout {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

const CONFIG: &str = r#"
path_packages = true
require_group = true

[[workspace]]
dir = "lib"
path_deps = []

[[workspace]]
dir = "versions/{domain}/{version}"
path_deps = ["lib/sdk", "lib/domains/{domain}"]

[[group]]
name = "sdk"
dir = "lib/sdk/core"
may_depend_on = ["sdk"]

[[group]]
name = "examples"
dir = "lib/sdk/examples"
may_depend_on = ["sdk", "examples"]

[[group]]
name = "domain"
dir = "lib/domains/{domain}"
may_depend_on = ["sdk", "domain"]

[[group]]
name = "version"
dir = "versions/{domain}/{version}"
may_depend_on = ["sdk", "domain", "version"]
"#;

impl Checkout {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let c = Checkout { _dir: dir, root };
        c.write(
            "lib/Cargo.toml",
            "[workspace]\nresolver = \"2\"\nmembers = [\"sdk/core\", \"sdk/examples/*\", \"domains/*/*\"]\n",
        );
        c.package("lib/sdk/core", "core", &[]);
        c.package("lib/sdk/examples/toy", "toy", &[("core", "../../core")]);
        for d in ["a", "b"] {
            c.package(
                &format!("lib/domains/{d}/{d}-dom"),
                &format!("{d}-dom"),
                &[("core", "../../../sdk/core")],
            );
        }
        for (d, v) in [("a", "v1"), ("a", "v2"), ("b", "v1")] {
            let ws = format!("versions/{d}/{v}");
            c.write(
                &format!("{ws}/Cargo.toml"),
                "[workspace]\nresolver = \"2\"\nmembers = [\"lib\"]\n",
            );
            c.package(
                &format!("{ws}/lib"),
                &format!("{d}-{v}-lib"),
                &[
                    ("core", "../../../../lib/sdk/core"),
                    (
                        &format!("{d}-dom"),
                        &format!("../../../../lib/domains/{d}/{d}-dom"),
                    ),
                ],
            );
        }
        c.package("outside/ext", "ext", &[]);
        c.write("strata.toml", CONFIG);
        c
    }

    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A package `name` in `dir` with path dependencies (name, relative path).
    fn package(&self, dir: &str, name: &str, deps: &[(&str, &str)]) {
        let mut text = format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\n"
        );
        for (d, p) in deps {
            text.push_str(&format!("{d} = {{ path = \"{p}\" }}\n"));
        }
        self.write(&format!("{dir}/Cargo.toml"), &text);
        self.write(&format!("{dir}/src/lib.rs"), "");
    }

    /// Append a path dependency to the manifest of the package in `dir`.
    fn depend(&self, dir: &str, name: &str, path: &str) {
        let p = self.root.join(dir).join("Cargo.toml");
        let mut text = std::fs::read_to_string(&p).unwrap();
        text.push_str(&format!("{name} = {{ path = \"{path}\" }}\n"));
        std::fs::write(p, text).unwrap();
    }

    fn config(&self, text: &str) {
        self.write("strata.toml", text);
    }

    fn run(&self) -> Result<Outcome, String> {
        run_with(None, Some(&self.root.join("strata.toml")), false)
    }

    fn run_one(&self, ws: &str) -> Result<Outcome, String> {
        let m = self.root.join(ws).join("Cargo.toml");
        run_with(Some(&m), Some(&self.root.join("strata.toml")), false)
    }
}

fn text(o: &Outcome) -> String {
    o.lines.join("\n")
}

fn errors(o: &Outcome, kind: &str) -> Vec<String> {
    let tag = format!("error[{kind}]");
    o.lines
        .iter()
        .filter(|l| l.starts_with(&tag))
        .cloned()
        .collect()
}

#[test]
fn every_workspace_checked_clean() {
    let c = Checkout::new();
    let o = c.run().unwrap();
    assert_eq!(o.violations, 0, "{}", text(&o));
    assert_eq!(o.errors, 0, "{}", text(&o));
}

#[test]
fn domain_may_not_use_another_domain() {
    let c = Checkout::new();
    c.depend("lib/domains/b/b-dom", "a-dom", "../../a/a-dom");
    let o = c.run().unwrap();
    let e = errors(&o, "crate");
    assert_eq!(e.len(), 1, "{}", text(&o));
    assert!(
        e[0].contains("lib/domains/b/b-dom/Cargo.toml:")
            && e[0].contains("`b-dom` -> `a-dom` (normal dependency): group `domain` (domain = b) may depend on `domain` only where their captures agree, not on `domain` (domain = a)"),
        "{}",
        e[0]
    );
}

#[test]
fn version_uses_only_its_own_domain() {
    let c = Checkout::new();
    c.depend(
        "versions/a/v1/lib",
        "b-dom",
        "../../../../lib/domains/b/b-dom",
    );
    let o = c.run().unwrap();
    let t = text(&o);
    let crate_errs = errors(&o, "crate");
    assert_eq!(crate_errs.len(), 1, "{t}");
    assert!(
        crate_errs[0].contains("`a-v1-lib` -> `b-dom`")
            && crate_errs[0].contains("group `version` (domain = a, version = v1) may depend on `domain` only where their captures agree, not on `domain` (domain = b)"),
        "{t}"
    );
    let path_errs = errors(&o, "path-dep");
    assert_eq!(path_errs.len(), 1, "{t}");
    assert!(
        path_errs[0].contains("versions/a/v1/lib/Cargo.toml: `a-v1-lib` has a path dependency on `b-dom` in `lib/domains/b/b-dom`, outside its workspace `versions/a/v1`"),
        "{t}"
    );
}

#[test]
fn version_may_not_use_an_example() {
    let c = Checkout::new();
    c.depend(
        "versions/b/v1/lib",
        "toy",
        "../../../../lib/sdk/examples/toy",
    );
    let o = c.run().unwrap();
    let e = errors(&o, "crate");
    assert_eq!(e.len(), 1, "{}", text(&o));
    assert!(
        e[0].contains("`b-v1-lib` -> `toy` (normal dependency): group `version` may depend on `sdk`, `domain`, `version` only, not `examples`"),
        "{}",
        e[0]
    );
    // lib/sdk is in its path_deps, so only the group rule fires.
    assert!(errors(&o, "path-dep").is_empty(), "{}", text(&o));
}

#[test]
fn version_may_not_use_another_version() {
    let c = Checkout::new();
    c.depend("versions/a/v2/lib", "a-v1-lib", "../../v1/lib");
    let o = c.run().unwrap();
    let t = text(&o);
    assert!(
        t.contains("`a-v2-lib` -> `a-v1-lib` (normal dependency): group `version` (domain = a, version = v2) may depend on `version` only where their captures agree, not on `version` (domain = a, version = v1)"),
        "{t}"
    );
    assert_eq!(errors(&o, "path-dep").len(), 1, "{t}");
}

#[test]
fn closed_workspace_with_named_exception() {
    let c = Checkout::new();
    c.depend("lib/sdk/core", "ext", "../../../outside/ext");
    let o = c.run().unwrap();
    let e = errors(&o, "path-dep");
    assert_eq!(e.len(), 1, "{}", text(&o));
    assert!(
        e[0].contains("lib/sdk/core/Cargo.toml: `core` has a path dependency on `ext` in `outside/ext`, outside its workspace `lib`"),
        "{}",
        e[0]
    );

    c.config(&format!(
        "{CONFIG}\n[[crate]]\nname = \"core\"\npath_deps_exempt = [\"ext\"]\n"
    ));
    let o = c.run().unwrap();
    assert!(errors(&o, "path-dep").is_empty(), "{}", text(&o));
}

#[test]
fn stale_path_exemption() {
    let c = Checkout::new();
    c.config(&format!(
        "{CONFIG}\n[[crate]]\nname = \"core\"\npath_deps_exempt = [\"ext\"]\n"
    ));
    let o = c.run().unwrap();
    let e = errors(&o, "path-dep");
    assert_eq!(e.len(), 1, "{}", text(&o));
    assert!(
        e[0].contains("`core` stale `path_deps_exempt = [\"ext\"]`"),
        "{}",
        e[0]
    );
}

#[test]
fn manifest_path_checks_one_workspace() {
    let c = Checkout::new();
    c.depend(
        "versions/a/v1/lib",
        "b-dom",
        "../../../../lib/domains/b/b-dom",
    );
    assert!(c.run_one("versions/a/v1").unwrap().violations > 0);
    let o = c.run_one("versions/b/v1").unwrap();
    assert_eq!(o.violations, 0, "{}", text(&o));
    let e = c.run_one("outside/ext").unwrap_err();
    assert!(e.contains("matches no `[[workspace]]` entry"), "{e}");
}

#[test]
fn a_new_domain_joins_its_group_by_directory() {
    let c = Checkout::new();
    c.package(
        "lib/domains/c/c-dom",
        "c-dom",
        &[("core", "../../../sdk/core")],
    );
    let o = c.run().unwrap();
    assert_eq!(o.violations, 0, "{}", text(&o));
    c.depend("lib/domains/c/c-dom", "b-dom", "../../b/b-dom");
    assert!(text(&c.run().unwrap()).contains("not on `domain` (domain = b)"));
}

#[test]
fn a_crate_outside_every_group_is_reported() {
    let c = Checkout::new();
    c.package("lib/sdk/stray", "stray", &[]);
    c.write(
        "lib/Cargo.toml",
        "[workspace]\nresolver = \"2\"\nmembers = [\"sdk/core\", \"sdk/stray\", \"sdk/examples/*\", \"domains/*/*\"]\n",
    );
    let o = c.run().unwrap();
    assert!(text(&o).contains("`stray` -> ``"), "{}", text(&o));
    assert!(
        text(&o).contains("crate belongs to no `group`"),
        "{}",
        text(&o)
    );
}

#[test]
fn path_packages_off_compares_members_only() {
    let c = Checkout::new();
    c.config(&CONFIG.replace("path_packages = true", ""));
    c.depend(
        "versions/a/v1/lib",
        "b-dom",
        "../../../../lib/domains/b/b-dom",
    );
    let o = c.run().unwrap();
    // b-dom is not a member of the version's workspace, so groups do not
    // compare the edge; path_deps still holds the version to its domain.
    assert!(errors(&o, "crate").is_empty(), "{}", text(&o));
    assert_eq!(errors(&o, "path-dep").len(), 1, "{}", text(&o));
}

#[test]
fn root_moves_every_path_in_the_config() {
    let c = Checkout::new();
    std::fs::create_dir_all(c.root.join("conf")).unwrap();
    c.write("conf/strata.toml", &format!("root = \"..\"\n{CONFIG}"));
    c.depend("lib/domains/b/b-dom", "a-dom", "../../a/a-dom");
    let o = run_with(None, Some(&c.root.join("conf/strata.toml")), false).unwrap();
    assert!(
        text(&o).contains("lib/domains/b/b-dom/Cargo.toml:"),
        "{}",
        text(&o)
    );
}

#[test]
fn config_errors_name_their_line() {
    let c = Checkout::new();
    let bad = |cfg: &str| match run_with(None, Some(&c.root.join("strata.toml")), false) {
        Err(e) => e,
        Ok(o) => panic!("{cfg}: expected a configuration error, got {}", text(&o)),
    };
    c.config("[[workspace]]\ndir = \"versions/{domain}/*\"\npath_deps = [\"lib/{other}\"]\n");
    let e = bad("uncaptured");
    assert!(e.contains(":3: workspace `versions/{domain}/*`: path_deps `lib/{other}` uses `{other}`, which its `dir` does not capture"), "{e}");

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[workspace]]\ndir = \"nowhere/*\"\n");
    let e = bad("unmatched");
    assert!(
        e.contains(":5: workspace `nowhere/*` matches no directory with a Cargo.toml"),
        "{e}"
    );

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[workspace]]\ndir = \"l*\"\n");
    let e = bad("twice");
    assert!(
        e.contains(":5: workspace `lib` matches entries `lib` and `l*`"),
        "{e}"
    );

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[group]]\nname = \"g\"\n");
    let e = bad("no members");
    assert!(
        e.contains(":5: group `g`: needs `crates`, `dir` or both"),
        "{e}"
    );

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[group]]\nname = \"g\"\ndir = \"lib/nothing\"\n");
    let e = bad("empty dir");
    assert!(
        e.contains(":6: group `g`: `dir = \"lib/nothing\"` holds no workspace crate"),
        "{e}"
    );

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[group]]\nname = \"g\"\ndir = \"lib/{a}/{a}\"\n");
    let e = bad("duplicate capture");
    assert!(e.contains(":6: group `g`: `dir = \"lib/{a}/{a}\"` is not a directory pattern: capture `{a}` appears twice"), "{e}");

    c.config("[[workspace]]\ndir = \"lib\"\n\n[[group]]\nname = \"one\"\ndir = \"lib/sdk\"\n\n[[group]]\nname = \"two\"\ndir = \"lib/sdk/core\"\n");
    let e = bad("overlap");
    assert!(
        e.contains(":10: crate `core` is in groups `one` (line 6) and `two`"),
        "{e}"
    );
}

#[test]
fn single_workspace_configs_unchanged() {
    // Without `[[workspace]]` the config beside the manifest governs that
    // workspace alone, with paths relative to it, as before.
    let c = Checkout::new();
    c.write(
        "lib/strata.toml",
        "[[group]]\nname = \"s\"\ncrates = [\"core\", \"toy\"]\ndir = \"sdk\"\n\n[[group]]\nname = \"d\"\ncrates = [\"*-dom\"]\ndir = \"domains\"\n",
    );
    let lib = c.root.join("lib/Cargo.toml");
    let o = cargo_strata::run(Some(&lib), None).unwrap();
    assert_eq!(o.violations, 0, "{}", text(&o));
    c.depend("lib/sdk/examples/toy", "a-dom", "../../../domains/a/a-dom");
    let o = cargo_strata::run(Some(Path::new(&lib)), None).unwrap();
    assert!(
        text(&o).contains("error[crate]: sdk/examples/toy/Cargo.toml:"),
        "{}",
        text(&o)
    );
    assert!(!text(&o).contains("lib/sdk"), "{}", text(&o));
}
