pub mod axis;
pub mod config;
pub mod crates;
pub mod dirs;
pub mod facts;
pub mod init;
pub mod lints;
pub mod metadata;
pub mod modules;
pub mod resolve;
pub mod scan;
pub mod surface;
pub mod validate;

use std::path::{Path, PathBuf};

pub const VERUS_FEATURE: bool = cfg!(feature = "verus");

#[derive(Debug, Default)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub violations: usize,
    pub errors: usize,
}

/// True for a message from the Rust parser (`... parse error: 12:5: ...`, or
/// a `verus!` body), as opposed to a size or nesting limit or an I/O error.
pub fn is_syntax_error(msg: &str) -> bool {
    let Some((_, rest)) = msg.split_once("parse error: ") else {
        return false;
    };
    let rest = rest.strip_prefix("verus! body: ").unwrap_or(rest);
    let mut it = rest.splitn(3, ':');
    let num =
        |s: Option<&str>| s.is_some_and(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()));
    num(it.next()) && num(it.next())
}

impl Outcome {
    /// Record a read or parse failure. A syntax error is a warning that the
    /// file was skipped (nightly-only syntax that `syn` rejects must not stop
    /// a run) unless `strict`; limits and read errors are always errors.
    fn parse_failure(&mut self, msg: String, strict: bool) {
        if !strict && is_syntax_error(&msg) {
            let w = format!("{msg} (file skipped; `--strict` makes this an error)");
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        } else {
            self.lines.push(format!("error[parse]: {msg}"));
            self.errors += 1;
        }
    }
}

/// Run all checks for the workspace whose manifest is `manifest` (or the cwd's).
pub fn run(manifest: Option<&Path>, config_path: Option<&Path>) -> Result<Outcome, String> {
    run_with(manifest, config_path, false)
}

/// One workspace to check: its metadata, its directory relative to the
/// root, and its `path_deps` rule with captures substituted.
struct Target {
    meta: cargo_metadata::Metadata,
    dir: String,
    path_deps: Option<Vec<dirs::DirPattern>>,
}

/// What the rules read of a workspace: its declared dependencies, unless
/// `check_optional = false` asks the resolve graph which are switched on.
fn read(
    reader: &mut metadata::Reader,
    cfg: &config::Config,
    manifest: Option<&Path>,
) -> Result<cargo_metadata::Metadata, String> {
    if cfg.check_optional {
        reader.declared(manifest)
    } else {
        metadata::full(manifest)
    }
}

fn canonical(p: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(p).map_err(|e| format!("cannot resolve {}: {e}", p.display()))
}

/// As [`run`]; with `strict`, a file the parser rejects is an error
/// (`error[parse]`, exit 2) instead of a warning.
///
/// With `[[workspace]]` entries in the config, every workspace they match is
/// checked, or only `manifest`'s when it is given (it must match one).
pub fn run_with(
    manifest: Option<&Path>,
    config_path: Option<&Path>,
    strict: bool,
) -> Result<Outcome, String> {
    let cfg_path = match config_path {
        Some(p) => p.to_path_buf(),
        None => metadata::members(manifest)?
            .workspace_root
            .as_std_path()
            .join("strata.toml"),
    };
    let text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| format!("cannot read {}: {e}", cfg_path.display()))?;
    let cfg: config::Config =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", cfg_path.display()))?;
    let cfg_name = cfg_path.display().to_string();
    let lines = validate::Lines::new(&text);
    let cfg_dir = canonical(match cfg_path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    })?;
    let (root, targets) = targets(&cfg, &cfg_dir, &lines, &cfg_name, manifest)?;

    let mut known: Vec<(String, Option<String>)> = Vec::new();
    for t in &targets {
        for (name, l) in crates::locals(&t.meta, &cfg, &root) {
            known.push((name, l.dir));
        }
    }
    known.sort();
    known.dedup();
    // Checking one of several workspaces sees only its crates, so a name
    // that matches only in another is not unknown; the full run judges it.
    let one_of_several = manifest.is_some() && !cfg.workspaces.is_empty();
    let judged = config::Config {
        allow_unknown_names: cfg.allow_unknown_names || one_of_several,
        ..cfg.clone()
    };
    let (bad, config_warnings) = validate::validate(&judged, &text, &known, &cfg_name);
    if !bad.is_empty() {
        return Err(bad.join("\n"));
    }
    let mut out = Outcome::default();
    out.warnings.extend(config_warnings);
    for t in &targets {
        check_workspace(t, &cfg, &root, &lines, &cfg_name, strict, &mut out)?;
    }
    if !VERUS_FEATURE && cfg.crates.iter().any(|r| r.verus) {
        out.warnings.push(
            "`verus = true` is set but cargo-strata was built without the `verus` feature; \
             `verus! { }` bodies are not checked (install with `--features verus`)"
                .to_string(),
        );
    }
    Ok(out)
}

/// The root and the workspaces to check. Without `[[workspace]]`: the one
/// workspace of `manifest` (or the current directory). With it: each
/// directory under the root an entry matches, or only `manifest`'s.
fn targets(
    cfg: &config::Config,
    cfg_dir: &Path,
    lines: &validate::Lines,
    cfg_name: &str,
    manifest: Option<&Path>,
) -> Result<(PathBuf, Vec<Target>), String> {
    let mut reader = metadata::Reader::default();
    let explicit_root = cfg
        .root
        .as_ref()
        .map(|r| canonical(&cfg_dir.join(r)))
        .transpose()?;
    if cfg.workspaces.is_empty() {
        let mut meta = read(&mut reader, cfg, manifest)?;
        // Paths are compared with the root as cargo prints them; an explicit
        // root is canonical, so read the workspace through its canonical path.
        let root = match explicit_root {
            Some(r) => {
                let ws = canonical(meta.workspace_root.as_std_path())?;
                if ws != meta.workspace_root.as_std_path() {
                    meta = read(&mut reader, cfg, Some(&ws.join("Cargo.toml")))?;
                }
                r
            }
            None => meta.workspace_root.clone().into(),
        };
        let dir = dirs::rel_dir(&root, meta.workspace_root.as_std_path()).unwrap_or_default();
        return Ok((
            root,
            vec![Target {
                meta,
                dir,
                path_deps: None,
            }],
        ));
    }
    let root = explicit_root.unwrap_or_else(|| cfg_dir.to_path_buf());
    let mut errors = Vec::new();
    // (directory, entry index, captures) of every workspace an entry matches.
    let mut matched: Vec<(String, usize, dirs::Captures)> = Vec::new();
    for (i, w) in cfg.workspaces.iter().enumerate() {
        let tbl = lines.nth("workspace", i);
        let at = |key: &str| lines.key_line_of(tbl, key);
        let pat = match dirs::DirPattern::new(&w.dir) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!(
                    "{cfg_name}:{}: workspace `{}`: {e}",
                    at("dir"),
                    w.dir
                ));
                continue;
            }
        };
        let caps: Vec<&str> = pat.captures().collect();
        for d in w.path_deps.iter().flatten() {
            let line = lines.item_line(tbl, "path_deps", d);
            match dirs::DirPattern::new(d) {
                Err(e) => errors.push(format!(
                    "{cfg_name}:{line}: workspace `{}`: path_deps `{d}`: {e}",
                    w.dir
                )),
                Ok(p) => {
                    for c in p.captures().filter(|c| !caps.contains(c)) {
                        errors.push(format!(
                            "{cfg_name}:{line}: workspace `{}`: path_deps `{d}` uses `{{{c}}}`, which its `dir` does not capture",
                            w.dir
                        ));
                    }
                }
            }
        }
        let hits: Vec<_> = pat
            .expand(&root)
            .into_iter()
            .filter(|(d, _)| root.join(d).join("Cargo.toml").is_file())
            .collect();
        if hits.is_empty() && !cfg.allow_unknown_names {
            errors.push(format!(
                "{cfg_name}:{}: workspace `{}` matches no directory with a Cargo.toml under {}",
                at("dir"),
                w.dir,
                root.display()
            ));
        }
        for (d, c) in hits {
            if let Some((_, j, _)) = matched.iter().find(|(m, _, _)| *m == d) {
                errors.push(format!(
                    "{cfg_name}:{}: workspace `{d}` matches entries `{}` and `{}`; each workspace may match one",
                    at("dir"),
                    cfg.workspaces[*j].dir,
                    w.dir
                ));
                continue;
            }
            matched.push((d, i, c));
        }
    }
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    if let Some(m) = manifest {
        let meta = metadata::members(Some(m))?;
        let dir = dirs::rel_dir(&root, &canonical(meta.workspace_root.as_std_path())?);
        let Some(entry) = matched.iter().find(|(d, _, _)| Some(d) == dir.as_ref()) else {
            return Err(format!(
                "{}: the workspace at {} matches no `[[workspace]]` entry",
                cfg_name, meta.workspace_root
            ));
        };
        matched = vec![entry.clone()];
    }
    let mut out = Vec::new();
    for (dir, i, caps) in matched {
        let meta = read(&mut reader, cfg, Some(&root.join(&dir).join("Cargo.toml")))?;
        if canonical(meta.workspace_root.as_std_path())? != canonical(&root.join(&dir))? {
            return Err(format!(
                "{cfg_name}: `{dir}` matches workspace `{}` but is not a workspace root (its workspace is {})",
                cfg.workspaces[i].dir, meta.workspace_root
            ));
        }
        let path_deps = cfg.workspaces[i]
            .path_deps
            .as_ref()
            .map(|l| {
                l.iter()
                    .map(|d| dirs::DirPattern::new(d)?.substitute(&caps))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        out.push(Target {
            meta,
            dir,
            path_deps,
        });
    }
    Ok((root, out))
}

/// Every rule over one workspace, appended to `out`.
fn check_workspace(
    t: &Target,
    cfg: &config::Config,
    ws_root: &Path,
    lines: &validate::Lines,
    cfg_name: &str,
    strict: bool,
    out: &mut Outcome,
) -> Result<(), String> {
    let meta = &t.meta;
    let rel = |p: &str| -> String {
        Path::new(p)
            .strip_prefix(ws_root)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.to_string())
    };
    for v in crates::check(meta, cfg, ws_root) {
        let loc = match v.line {
            Some(l) => format!("{}:{l}", rel(&v.manifest)),
            None => rel(&v.manifest),
        };
        out.lines.push(format!(
            "error[crate]: {loc}: `{}` -> `{}` ({} dependency): {}",
            v.from, v.to, v.kind, v.reason
        ));
        out.violations += 1;
    }

    let path_deps = dirs::check_path_deps(meta, cfg, ws_root, &t.dir, t.path_deps.as_deref());
    for v in dirs::check(meta, cfg, ws_root).into_iter().chain(path_deps) {
        out.lines.push(format!(
            "error[{}]: {}: `{}` {}",
            v.kind,
            rel(&v.manifest),
            v.krate,
            v.reason
        ));
        out.violations += 1;
    }

    let lint = lints::check(meta, cfg, ws_root, VERUS_FEATURE)?;
    for v in lint.violations {
        let loc = if v.line > 0 {
            format!("{}:{}", v.file, v.line)
        } else {
            v.file
        };
        out.lines
            .push(format!("error[{}]: {loc}: {}", v.kind, v.msg));
        out.violations += 1;
    }
    for e in lint.errors {
        out.parse_failure(e, strict);
    }

    let members: std::collections::HashSet<_> = meta.workspace_members.iter().collect();
    for pkg in meta.packages.iter().filter(|p| members.contains(&p.id)) {
        let rules: Vec<&config::CrateRule> = cfg
            .crates
            .iter()
            .filter(|r| config::glob_match(&r.name, &pkg.name))
            .collect();
        if rules.iter().all(|r| r.modules.is_empty()) {
            continue;
        }
        let roots: Vec<PathBuf> = pkg
            .targets
            .iter()
            .filter(|t| {
                t.kind.iter().any(|k| {
                    matches!(
                        k.to_string().as_str(),
                        "lib" | "proc-macro" | "bin" | "rlib"
                    )
                })
            })
            .map(|t| t.src_path.clone().into())
            .collect();
        let rep = modules::check_crate(&pkg.name, &roots, &rules, cfg, VERUS_FEATURE, ws_root);
        for v in rep.violations {
            if v.stale {
                let line = lines
                    .find_module(&pkg.name, &v.from)
                    .or_else(|| {
                        rules
                            .iter()
                            .find_map(|r| lines.find_module(&r.name, &v.from))
                    })
                    .map(|t| lines.item_line(t, "exempt", &v.to))
                    .unwrap_or(0);
                out.lines.push(format!(
                    "error[module]: {cfg_name}:{line}: crate `{}`: module `{}`: stale exemption of `{}`: it suppresses no reference; delete it",
                    pkg.name, v.from, v.to
                ));
                out.violations += 1;
                continue;
            }
            out.lines.push(format!(
                "error[module]: {}:{}: module `{}` references `{}` (via `{}`): {}",
                v.file.display(),
                v.line,
                if v.from.is_empty() { "crate" } else { &v.from },
                v.to,
                v.reference,
                v.reason
            ));
            out.violations += 1;
        }
        for e in rep.errors {
            out.parse_failure(e, strict);
        }
        out.warnings.extend(rep.warnings);
        for u in rep.unknown.into_iter().filter(|_| !cfg.allow_unknown_names) {
            let line = lines
                .find_module(&pkg.name, &u.rule)
                .or_else(|| {
                    // The rule may sit under a glob `[[crate]] name`.
                    rules
                        .iter()
                        .find_map(|r| lines.find_module(&r.name, &u.rule))
                })
                .map(|t| lines.item_line(t, u.key, &u.name))
                .unwrap_or(0);
            out.lines.push(if u.key == "path" {
                format!(
                    "error[config]: {cfg_name}:{line}: crate `{}`: module rule `{}` matches no module \
                     (modules from macros or cfg'd-out `mod` declarations are not visible)",
                    pkg.name, u.rule
                )
            } else {
                format!(
                    "error[config]: {cfg_name}:{line}: crate `{}`: module rule `{}`: `{}` names `{}`, which is not a module",
                    pkg.name, u.rule, u.key, u.name
                )
            });
            out.errors += 1;
        }
        for a in rep.ambiguous {
            let line = lines
                .find_module(&pkg.name, &a.second)
                .or_else(|| {
                    rules
                        .iter()
                        .find_map(|r| lines.find_module(&r.name, &a.second))
                })
                .map(|t| lines.item_line(t, "path", &a.second))
                .unwrap_or(0);
            out.lines.push(format!(
                "error[config]: {cfg_name}:{line}: crate `{}`: module `{}` matches module rules `{}` and `{}` equally specifically; \
                 make one of them more specific or merge them",
                pkg.name, a.module, a.first, a.second
            ));
            out.errors += 1;
        }
    }
    surface_check(meta, cfg, ws_root, lines, cfg_name, strict, out);
    Ok(())
}

/// `public_modules`: paths from dependent workspace crates into a crate.
fn surface_check(
    meta: &cargo_metadata::Metadata,
    cfg: &config::Config,
    ws_root: &Path,
    lines: &validate::Lines,
    cfg_name: &str,
    strict: bool,
    out: &mut Outcome,
) {
    use std::collections::{BTreeSet, HashMap};
    let members: std::collections::HashSet<_> = meta.workspace_members.iter().collect();
    let pkgs: Vec<&cargo_metadata::Package> = meta
        .packages
        .iter()
        .filter(|p| members.contains(&p.id))
        .collect();
    let roots_of = |pkg: &cargo_metadata::Package| -> Vec<PathBuf> {
        pkg.targets
            .iter()
            .filter(|t| {
                t.kind.iter().any(|k| {
                    matches!(
                        k.to_string().as_str(),
                        "lib" | "proc-macro" | "bin" | "rlib"
                    )
                })
            })
            .map(|t| t.src_path.clone().into())
            .collect()
    };
    let rules_of = |name: &str| -> Vec<&config::CrateRule> {
        cfg.crates
            .iter()
            .filter(|r| config::glob_match(&r.name, name))
            .collect()
    };
    type Scans = HashMap<
        String,
        (
            Vec<modules::FileScan>,
            std::collections::HashSet<Vec<String>>,
        ),
    >;
    let mut scans: Scans = HashMap::new();
    let mut seen_errors: BTreeSet<String> = BTreeSet::new();
    let mut scan = |pkg: &cargo_metadata::Package, scans: &mut Scans, out: &mut Outcome| {
        if scans.contains_key(&*pkg.name) {
            return;
        }
        let verus = VERUS_FEATURE && rules_of(&pkg.name).iter().any(|r| r.verus);
        let (files, known, rep) = modules::scan_crate(&roots_of(pkg), verus, cfg.skip_cfg_test);
        for e in rep.errors {
            if seen_errors.insert(e.clone()) {
                out.parse_failure(e, strict);
            }
        }
        scans.insert(pkg.name.to_string(), (files, known));
    };
    for target in &pkgs {
        let rules = rules_of(&target.name);
        let mut public: Vec<String> = Vec::new();
        let mut any = false;
        let mut only: Option<Vec<String>> = None;
        for r in &rules {
            if let Some(p) = &r.public_modules {
                any = true;
                public.extend(p.iter().cloned());
                if let Some(f) = &r.public_modules_for {
                    only.get_or_insert_with(Vec::new).extend(f.iter().cloned());
                }
            }
        }
        if !any {
            continue;
        }
        scan(target, &mut scans, out);
        let known = scans[&*target.name].1.clone();
        if !cfg.allow_unknown_names {
            for r in rules.iter().filter(|r| r.public_modules.is_some()) {
                for n in r.public_modules.iter().flatten() {
                    let path: Vec<String> = n
                        .split("::")
                        .filter(|s| !s.is_empty() && *s != "crate")
                        .map(str::to_string)
                        .collect();
                    if !known.contains(&path) {
                        let line = lines
                            .find("crate", "name", &r.name)
                            .map(|t| lines.item_line(t, "public_modules", n))
                            .unwrap_or(0);
                        out.lines.push(format!(
                            "error[config]: {cfg_name}:{line}: crate `{}`: public_modules names `{n}`, which is not a module",
                            target.name
                        ));
                        out.errors += 1;
                    }
                }
            }
        }
        let ident = target.name.replace('-', "_");
        for user in &pkgs {
            if user.id == target.id
                || !user.dependencies.iter().any(|d| d.name == *target.name)
                || only
                    .as_ref()
                    .is_some_and(|o| !o.iter().any(|g| config::glob_match(g, &user.name)))
            {
                continue;
            }
            scan(user, &mut scans, out);
            let (files, _) = &scans[&*user.name];
            let units: Vec<modules::ModuleUnit> = files
                .iter()
                .map(|f| modules::ModuleUnit {
                    file: f.file.clone(),
                    module: f.module.clone(),
                    scan: &f.scan,
                })
                .collect();
            for v in surface::evaluate(&ident, &public, &known, &units, ws_root) {
                out.lines.push(format!(
                    "error[surface]: {}:{}: `{}` names `{}` of `{}` (via `{}`), which is not in its public_modules",
                    v.file.display(),
                    v.line,
                    user.name,
                    v.module,
                    target.name,
                    v.reference
                ));
                out.violations += 1;
            }
        }
    }
}

/// Config text describing the workspace as it is now (`cargo strata init`).
pub fn init(manifest: Option<&Path>, verus: bool) -> Result<(String, Vec<String>), String> {
    let meta = metadata::full(manifest)?;
    Ok(init::generate(&meta, verus, VERUS_FEATURE))
}
