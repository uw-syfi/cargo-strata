pub mod config;
pub mod crates;
pub mod dirs;
pub mod facts;
pub mod init;
pub mod lints;
pub mod modules;
pub mod resolve;
pub mod scan;
pub mod surface;
pub mod validate;

use cargo_metadata::MetadataCommand;
use std::path::{Path, PathBuf};

pub const VERUS_FEATURE: bool = cfg!(feature = "verus");

#[derive(Debug, Default)]
pub struct Outcome {
    pub lines: Vec<String>,
    pub warnings: Vec<String>,
    pub violations: usize,
    pub errors: usize,
}

/// Run all checks for the workspace whose manifest is `manifest` (or the cwd's).
pub fn run(manifest: Option<&Path>, config_path: Option<&Path>) -> Result<Outcome, String> {
    let mut cmd = MetadataCommand::new();
    if let Some(m) = manifest {
        cmd.manifest_path(m);
    }
    let meta = cmd
        .exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;
    let ws_root: PathBuf = meta.workspace_root.clone().into();
    let cfg_path = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ws_root.join("strata.toml"));
    let text = std::fs::read_to_string(&cfg_path)
        .map_err(|e| format!("cannot read {}: {e}", cfg_path.display()))?;
    let cfg: config::Config =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", cfg_path.display()))?;
    let members: std::collections::HashSet<_> = meta.workspace_members.iter().collect();
    let ws_names: std::collections::BTreeSet<String> = meta
        .packages
        .iter()
        .filter(|p| members.contains(&p.id))
        .map(|p| p.name.to_string())
        .collect();
    let cfg_name = cfg_path.display().to_string();
    let lines = validate::Lines::new(&text);
    let (bad, config_warnings) = validate::validate(&cfg, &text, &ws_names, &cfg_name);
    if !bad.is_empty() {
        return Err(bad.join("\n"));
    }
    let rel = |p: &str| -> String {
        Path::new(p)
            .strip_prefix(&ws_root)
            .map(|r| r.display().to_string())
            .unwrap_or_else(|_| p.to_string())
    };

    let mut out = Outcome::default();
    out.warnings.extend(config_warnings);
    for v in crates::check(&meta, &cfg) {
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

    for v in dirs::check(&meta, &cfg) {
        out.lines.push(format!(
            "error[{}]: {}: `{}` {}",
            v.kind,
            rel(&v.manifest),
            v.krate,
            v.reason
        ));
        out.violations += 1;
    }

    let lint = lints::check(&meta, &cfg, VERUS_FEATURE)?;
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
        out.lines.push(format!("error[parse]: {e}"));
        out.errors += 1;
    }

    if !VERUS_FEATURE && cfg.crates.iter().any(|r| r.verus) {
        out.warnings.push(
            "`verus = true` is set but cargo-strata was built without the `verus` feature; \
             `verus! { }` bodies are not checked (install with `--features verus`)"
                .to_string(),
        );
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
        let rep = modules::check_crate(&pkg.name, &roots, &rules, &cfg, VERUS_FEATURE, &ws_root);
        for v in rep.violations {
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
            out.lines.push(format!("error[parse]: {e}"));
            out.errors += 1;
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
    }
    surface_check(&meta, &cfg, &ws_root, &lines, &cfg_name, &mut out);
    Ok(out)
}

/// `public_modules`: paths from dependent workspace crates into a crate.
fn surface_check(
    meta: &cargo_metadata::Metadata,
    cfg: &config::Config,
    ws_root: &Path,
    lines: &validate::Lines,
    cfg_name: &str,
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
                out.lines.push(format!("error[parse]: {e}"));
                out.errors += 1;
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
    let mut cmd = MetadataCommand::new();
    if let Some(m) = manifest {
        cmd.manifest_path(m);
    }
    let meta = cmd
        .exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))?;
    Ok(init::generate(&meta, verus, VERUS_FEATURE))
}
