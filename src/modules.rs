//! Module-level rules inside a crate: path-based, no name resolution.

use crate::config::{Config, CrateRule, ModuleRule};
use crate::resolve::{Res, Table};
use crate::scan::{Scan, is_root_kw, scan_source};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ModViolation {
    pub krate: String,
    pub file: PathBuf,
    pub line: usize,
    pub from: String,
    pub to: String,
    pub reference: String,
    pub reason: String,
}

/// A module name in the config that names no module of the crate.
#[derive(Debug)]
pub struct UnknownModule {
    /// `path` of the rule it appears in.
    pub rule: String,
    /// `"path"`, `"depends_on"` or `"deny"`.
    pub key: &'static str,
    pub name: String,
}

#[derive(Default)]
pub struct Report {
    pub unknown: Vec<UnknownModule>,
    pub violations: Vec<ModViolation>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

pub struct FileScan {
    pub file: PathBuf,
    pub module: Vec<String>,
    pub scan: Scan,
}

fn split(p: &str) -> Vec<String> {
    p.split("::")
        .filter(|s| !s.is_empty() && *s != "crate")
        .map(str::to_string)
        .collect()
}

fn is_prefix(p: &[String], of: &[String]) -> bool {
    of.len() >= p.len() && of[..p.len()] == *p
}

/// `mod r#type;` lives in `type.rs`: the file system name of a module.
pub fn fs_name(module: &str) -> &str {
    module.strip_prefix("r#").unwrap_or(module)
}

/// Directory in which child modules of `file` (module `module`) live.
pub fn child_dir(file: &Path, is_root: bool) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new("."));
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if is_root || stem == "mod" {
        parent.to_path_buf()
    } else {
        parent.join(stem)
    }
}

struct Walker {
    parse_verus: bool,
    skip_cfg_test: bool,
    seen: HashSet<PathBuf>,
    files: Vec<FileScan>,
    rep: Report,
}

impl Walker {
    fn walk(&mut self, file: &Path, module: Vec<String>, is_root: bool) {
        if !self.seen.insert(file.to_path_buf()) {
            return;
        }
        let src = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                self.rep
                    .errors
                    .push(format!("{}: cannot read: {e}", file.display()));
                return;
            }
        };
        let scan = match scan_source(&src, self.parse_verus, self.skip_cfg_test) {
            Ok(s) => s,
            Err(e) => {
                self.rep
                    .errors
                    .push(format!("{}: parse error: {e}", file.display()));
                return;
            }
        };
        let dir = child_dir(file, is_root);
        let decls = scan.decls.clone();
        self.files.push(FileScan {
            file: file.to_path_buf(),
            module: module.clone(),
            scan,
        });
        for d in decls {
            let mut m = module.clone();
            m.extend(d.inline.iter().cloned());
            m.push(d.name.clone());
            let mut base = dir.clone();
            for i in &d.inline {
                base.push(fs_name(i));
            }
            let file_name = fs_name(&d.name);
            let candidates = match &d.path_attr {
                Some(p) => vec![file.parent().unwrap_or(Path::new(".")).join(p)],
                None => vec![
                    base.join(format!("{file_name}.rs")),
                    base.join(file_name).join("mod.rs"),
                ],
            };
            match candidates.into_iter().find(|c| c.is_file()) {
                Some(c) => self.walk(&c, m, false),
                None => self.rep.warnings.push(format!(
                    "{}: `mod {};` has no file (cfg'd out or generated?)",
                    file.display(),
                    d.name
                )),
            }
        }
    }
}

fn resolve(
    segs: &[String],
    in_use: bool,
    cur: &[String],
    known: &HashSet<Vec<String>>,
) -> Option<Vec<String>> {
    match segs[0].as_str() {
        "crate" => Some(segs[1..].to_vec()),
        "self" => Some([cur, &segs[1..]].concat()),
        "super" => {
            let n = segs.iter().take_while(|s| *s == "super").count();
            if n > cur.len() {
                return None;
            }
            Some([&cur[..cur.len() - n], &segs[n..]].concat())
        }
        first if in_use => {
            let mut c = cur.to_vec();
            c.push(first.to_string());
            known.contains(&c).then(|| [cur, segs].concat())
        }
        _ => None,
    }
}

/// Every file reachable by `mod` declarations from the crate's target roots,
/// the module each belongs to, and the set of known modules (the root, file
/// modules and inline modules). Read and parse errors go in the report.
pub fn scan_crate(
    roots: &[PathBuf],
    parse_verus: bool,
    skip_cfg_test: bool,
) -> (Vec<FileScan>, HashSet<Vec<String>>, Report) {
    let mut w = Walker {
        parse_verus,
        skip_cfg_test,
        seen: HashSet::new(),
        files: Vec::new(),
        rep: Report::default(),
    };
    for root in roots {
        w.walk(root, vec![], true);
    }
    let Walker { files, rep, .. } = w;
    let mut known: HashSet<Vec<String>> = HashSet::new();
    known.insert(vec![]);
    for f in &files {
        known.insert(f.module.clone());
        for im in &f.scan.inline_mods {
            known.insert([&f.module[..], &im[..]].concat());
        }
    }
    (files, known, rep)
}

pub fn check_crate(
    krate: &str,
    roots: &[PathBuf],
    rules: &[&CrateRule],
    cfg: &Config,
    verus_feature: bool,
    ws_root: &Path,
) -> Report {
    let modules: Vec<&ModuleRule> = rules.iter().flat_map(|r| r.modules.iter()).collect();
    if modules.is_empty() {
        return Report::default();
    }
    let want_verus = rules.iter().any(|r| r.verus);
    let parse_verus = want_verus && verus_feature;
    let (files, known, mut rep) = scan_crate(roots, parse_verus, cfg.skip_cfg_test);
    let units: Vec<ModuleUnit> = files
        .iter()
        .map(|f| ModuleUnit {
            file: f.file.clone(),
            module: f.module.clone(),
            scan: &f.scan,
        })
        .collect();
    let table = cfg
        .resolve
        .then(|| Table::build(&known, files.iter().map(|f| (&f.module[..], &f.scan.items))));
    let (viol, warns) = evaluate_with(krate, &modules, &known, &units, ws_root, table.as_ref());
    rep.violations = viol;
    // An unknown name is a configuration error (reported by the caller with
    // its config line), not a warning.
    rep.warnings.extend(
        warns
            .into_iter()
            .filter(|w| !w.contains("matches no module")),
    );
    for m in &modules {
        let is_known = |n: &str| known.contains(&split(n));
        if !is_known(&m.path) {
            rep.unknown.push(UnknownModule {
                rule: m.path.clone(),
                key: "path",
                name: m.path.clone(),
            });
        }
        for (key, list) in [
            ("depends_on", m.depends_on.as_deref().unwrap_or(&[])),
            ("deny", &m.deny[..]),
        ] {
            for n in list {
                if !is_known(n) {
                    rep.unknown.push(UnknownModule {
                        rule: m.path.clone(),
                        key,
                        name: n.clone(),
                    });
                }
            }
        }
    }
    rep
}

/// The module graph a crate has now: for each known module, the modules it
/// references as written (the set `depends_on` would have to list). Used by
/// `cargo strata init`.
pub fn observe_crate(
    roots: &[PathBuf],
    cfg: &Config,
    parse_verus: bool,
    ws_root: &Path,
) -> (
    std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    Vec<String>,
) {
    let mut w = Walker {
        parse_verus,
        skip_cfg_test: cfg.skip_cfg_test,
        seen: HashSet::new(),
        files: Vec::new(),
        rep: Report::default(),
    };
    for root in roots {
        w.walk(root, vec![], true);
    }
    let Walker { files, rep, .. } = w;
    let mut known: HashSet<Vec<String>> = HashSet::new();
    known.insert(vec![]);
    for f in &files {
        known.insert(f.module.clone());
        for im in &f.scan.inline_mods {
            known.insert([&f.module[..], &im[..]].concat());
        }
    }
    let rules: Vec<ModuleRule> = known
        .iter()
        .filter(|k| !k.is_empty())
        .map(|k| ModuleRule {
            path: k.join("::"),
            depends_on: Some(vec![]),
            deny: vec![],
        })
        .collect();
    let refs: Vec<&ModuleRule> = rules.iter().collect();
    let units: Vec<ModuleUnit> = files
        .iter()
        .map(|f| ModuleUnit {
            file: f.file.clone(),
            module: f.module.clone(),
            scan: &f.scan,
        })
        .collect();
    let (viol, _) = evaluate_with("", &refs, &known, &units, ws_root, None);
    let mut out: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> = rules
        .iter()
        .map(|r| (r.path.clone(), Default::default()))
        .collect();
    for v in viol {
        let from: Vec<String> = v.from.split("::").map(str::to_string).collect();
        let Some(n) = (1..=from.len()).rev().find(|n| known.contains(&from[..*n])) else {
            continue;
        };
        out.entry(from[..n].join("::")).or_default().insert(v.to);
    }
    (out, rep.errors)
}

pub struct ModuleUnit<'a> {
    pub file: PathBuf,
    pub module: Vec<String>,
    pub scan: &'a Scan,
}

/// Pure rule evaluation over scanned files and the set of known modules, with
/// names taken as written.
pub fn evaluate(
    krate: &str,
    modules: &[&ModuleRule],
    known: &HashSet<Vec<String>>,
    files: &[ModuleUnit],
    ws_root: &Path,
) -> (Vec<ModViolation>, Vec<String>) {
    evaluate_with(krate, modules, known, files, ws_root, None)
}

/// Defining modules a reference reaches beyond the path as written: the
/// module that defines the resolved target, and for a glob import the
/// defining modules of everything its public re-exports bring in.
fn resolved_targets(table: &Table, cur: &[String], r: &crate::scan::RawRef) -> Vec<Vec<String>> {
    let res = table.resolve(cur, &r.segs);
    let mut out: Vec<Vec<String>> = Vec::new();
    if r.glob
        && let Res::Module(g) = &res
    {
        out.extend(table.glob_sources(g));
    }
    if let Some(m) = res.module() {
        out.push(m.to_vec());
    }
    out.retain(|m| !m.is_empty());
    out.sort();
    out.dedup();
    out
}

/// As [`evaluate`]; with a name-resolution `table`, each reference is also
/// attributed to the modules that define what it names (through re-exports,
/// globs, renames and type aliases), and relative paths outside `use` that
/// reach a child module are seen.
pub fn evaluate_with(
    krate: &str,
    modules: &[&ModuleRule],
    known: &HashSet<Vec<String>>,
    files: &[ModuleUnit],
    ws_root: &Path,
    table: Option<&Table>,
) -> (Vec<ModViolation>, Vec<String>) {
    let mut rep = Report::default();
    let parsed: Vec<(Vec<String>, &ModuleRule)> =
        modules.iter().map(|m| (split(&m.path), *m)).collect();
    for (p, m) in &parsed {
        if !known.contains(p) {
            rep.warnings.push(format!(
                "crate `{krate}`: module rule `{}` matches no module",
                m.path
            ));
        }
    }
    for f in files {
        let bare: &[crate::scan::RawRef] = if table.is_some() { &f.scan.bare } else { &[] };
        for r in f.scan.refs.iter().chain(bare) {
            let cur = [&f.module[..], &r.inline[..]].concat();
            let Some((_, src_rule)) = parsed
                .iter()
                .filter(|(p, _)| is_prefix(p, &cur))
                .max_by_key(|(p, _)| p.len())
            else {
                continue;
            };
            let sp = split(&src_rule.path);
            // `(target, as_written)`: depends_on judges only the path as
            // written (reaching an allowed facade is allowed); deny judges both.
            let mut targets: Vec<(Vec<String>, bool)> = Vec::new();
            let is_bare = !r.in_use && !is_root_kw(&r.segs[0]);
            if !is_bare
                && let Some(path) = resolve(&r.segs, r.in_use, &cur, known)
                && let Some(tlen) = (1..=path.len()).rev().find(|n| known.contains(&path[..*n]))
            {
                targets.push((path[..tlen].to_vec(), true));
            }
            if let Some(t) = table
                && !(is_bare && t.is_explicit(&cur, &r.segs[0]))
            {
                for m in resolved_targets(t, &cur, r) {
                    if !targets.iter().any(|(x, _)| *x == m) {
                        targets.push((m, false));
                    }
                }
            }
            for (target, as_written) in targets {
                if is_prefix(&sp, &target) {
                    continue;
                }
                let reason =
                    if let Some(d) = src_rule.deny.iter().find(|d| is_prefix(&split(d), &target)) {
                        Some(format!("`{}` denies `{d}`", src_rule.path))
                    } else if let (true, Some(allow)) = (as_written, &src_rule.depends_on) {
                        (!allow.iter().any(|d| is_prefix(&split(d), &target)))
                            .then(|| format!("`{}` does not list it in depends_on", src_rule.path))
                    } else {
                        None
                    };
                if let Some(reason) = reason {
                    let rel = f
                        .file
                        .strip_prefix(ws_root)
                        .unwrap_or(&f.file)
                        .to_path_buf();
                    rep.violations.push(ModViolation {
                        krate: krate.to_string(),
                        file: rel,
                        line: r.line,
                        from: cur.join("::"),
                        to: target.join("::"),
                        reference: r.segs.join("::"),
                        reason: if as_written {
                            reason
                        } else {
                            format!("{reason} (the name resolves to `{}`)", target.join("::"))
                        },
                    });
                }
            }
        }
    }
    rep.violations.sort_by(|a, b| {
        (&a.file, a.line, &a.reference, &a.to).cmp(&(&b.file, b.line, &b.reference, &b.to))
    });
    rep.violations.dedup_by(|a, b| {
        a.file == b.file && a.line == b.line && a.reference == b.reference && a.to == b.to
    });
    (rep.violations, rep.warnings)
}
