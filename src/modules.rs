//! Module-level rules inside a crate: path-based, with optional name
//! resolution. Module paths in rules may hold per-segment glob patterns
//! ([`ModulePattern`]).

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
    /// A stale `exempt` entry (`to` is the exempted module, `from` the rule's
    /// module, `line` and `reference` are unset).
    pub stale: bool,
}

/// A module name in the config that names no module of the crate.
#[derive(Debug)]
pub struct UnknownModule {
    /// `path` of the rule it appears in.
    pub rule: String,
    /// `"path"`, `"depends_on"`, `"deny"` or `"exempt"`.
    pub key: &'static str,
    pub name: String,
}

/// A module that two module rules with different paths match equally
/// specifically (a configuration error: neither rule can be chosen).
#[derive(Debug, PartialEq, Eq)]
pub struct AmbiguousRule {
    /// The module both rules match, `a::b`.
    pub module: String,
    /// The two rules' `path`s, in config order.
    pub first: String,
    pub second: String,
}

#[derive(Default)]
pub struct Report {
    pub unknown: Vec<UnknownModule>,
    pub ambiguous: Vec<AmbiguousRule>,
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
        .map(|s| crate::scan::unraw(&s))
        .collect()
}

fn is_prefix(p: &[String], of: &[String]) -> bool {
    of.len() >= p.len() && of[..p.len()] == *p
}

/// Does a module path segment use glob syntax? Module names are identifiers,
/// so `*`, `?` and `[` never occur in a literal name.
fn is_glob_segment(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

#[derive(Debug, Clone)]
enum Segment {
    Name(String),
    Glob(glob::Pattern),
}

/// A module path as written in a module rule (`path`, `depends_on`, `deny`,
/// `exempt`): segments separated by `::`, a leading `crate::` allowed. Each
/// segment is a module name (raw or not) or a glob pattern (`*`, `?`,
/// `[..]`) that matches exactly one module name: `tools::*` names every
/// child of `tools`, `*` every top-level module, never the crate root.
#[derive(Debug, Clone)]
pub struct ModulePattern {
    segs: Vec<Segment>,
}

impl ModulePattern {
    /// Why `path` is not a valid module pattern, if it is not. A segment
    /// with glob syntax must be a valid glob without `**` (a pattern matches
    /// one module name; there is no recursive form).
    pub fn check(path: &str) -> Result<(), String> {
        for seg in path.split("::").filter(|s| is_glob_segment(s)) {
            let seg = crate::scan::unraw(&seg);
            if seg.contains("**") {
                return Err(format!(
                    "`{seg}`: `**` is not supported; `*` matches one module name"
                ));
            }
            glob::Pattern::new(&seg).map_err(|e| format!("`{seg}`: {e}"))?;
        }
        Ok(())
    }

    /// Parse `path`. A segment that fails [`ModulePattern::check`] is kept
    /// as a literal name, which matches no module.
    pub fn new(path: &str) -> Self {
        let segs = split(path)
            .into_iter()
            .map(|s| match glob::Pattern::new(&s) {
                Ok(p) if is_glob_segment(&s) && !s.contains("**") => Segment::Glob(p),
                _ => Segment::Name(s),
            })
            .collect();
        ModulePattern { segs }
    }

    /// Number of segments: the depth of the modules it names.
    pub fn len(&self) -> usize {
        self.segs.len()
    }

    /// Does it name the crate root (no segments)?
    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// Does it name the module `m[..self.len()]`, that is, is `m` that
    /// module or inside it? For a literal path this is the prefix test.
    pub fn covers(&self, m: &[String]) -> bool {
        m.len() >= self.segs.len()
            && self.segs.iter().zip(m).all(|(p, s)| match p {
                Segment::Name(n) => n == s,
                Segment::Glob(g) => g.matches(s),
            })
    }

    /// Does it name at least one module in `known`?
    pub fn matches_any(&self, known: &HashSet<Vec<String>>) -> bool {
        known
            .iter()
            .any(|k| k.len() == self.segs.len() && self.covers(k))
    }

    /// Specificity, compared segment by segment from the root: a name beats
    /// a pattern, and a pattern beats no segment. Among rules that cover a
    /// module the greatest wins; for literal paths that is the longest.
    pub fn specificity(&self) -> Vec<u8> {
        self.segs
            .iter()
            .map(|s| match s {
                Segment::Name(_) => 2,
                Segment::Glob(_) => 1,
            })
            .collect()
    }
}

/// The rule that governs module `cur`: of the rules whose path covers it,
/// the most specific (the last one in config order among equals).
fn source_rule<'r, 'm>(
    parsed: &'r [(ModulePattern, &'m ModuleRule)],
    cur: &[String],
) -> Option<(usize, &'r (ModulePattern, &'m ModuleRule))> {
    parsed
        .iter()
        .enumerate()
        .filter(|(_, (p, _))| p.covers(cur))
        .max_by_key(|(_, (p, _))| p.specificity())
}

/// Modules in `known` that two rules with different paths govern equally
/// specifically, each pair reported once at the shallowest such module.
pub fn ambiguous_rules(
    modules: &[&ModuleRule],
    known: &HashSet<Vec<String>>,
) -> Vec<AmbiguousRule> {
    let parsed: Vec<(ModulePattern, &ModuleRule)> = modules
        .iter()
        .map(|m| (ModulePattern::new(&m.path), *m))
        .collect();
    let mut mods: Vec<&Vec<String>> = known.iter().collect();
    mods.sort_by(|a, b| (a.len(), a).cmp(&(b.len(), b)));
    let mut out: Vec<AmbiguousRule> = Vec::new();
    for m in mods {
        let Some((_, (best, _))) = source_rule(&parsed, m) else {
            continue;
        };
        let top: Vec<&str> = parsed
            .iter()
            .filter(|(p, _)| p.covers(m) && p.specificity() == best.specificity())
            .map(|(_, r)| r.path.as_str())
            .collect();
        for (i, a) in top.iter().enumerate() {
            for b in &top[i + 1..] {
                if a != b
                    && !out
                        .iter()
                        .any(|x| (x.first.as_str(), x.second.as_str()) == (*a, *b))
                {
                    out.push(AmbiguousRule {
                        module: m.join("::"),
                        first: a.to_string(),
                        second: b.to_string(),
                    });
                }
            }
        }
    }
    out
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
    rep.ambiguous = ambiguous_rules(&modules, &known);
    // An unknown name is a configuration error (reported by the caller with
    // its config line), not a warning.
    rep.warnings.extend(
        warns
            .into_iter()
            .filter(|w| !w.contains("matches no module")),
    );
    for m in &modules {
        let is_known = |n: &str| ModulePattern::new(n).matches_any(&known);
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
            ("exempt", &m.exempt[..]),
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
            exempt: vec![],
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
    let parsed: Vec<(ModulePattern, &ModuleRule)> = modules
        .iter()
        .map(|m| (ModulePattern::new(&m.path), *m))
        .collect();
    let lists: Vec<[Vec<ModulePattern>; 3]> = modules
        .iter()
        .map(|m| {
            let pats = |l: &[String]| l.iter().map(|d| ModulePattern::new(d)).collect();
            [
                pats(m.depends_on.as_deref().unwrap_or(&[])),
                pats(&m.deny),
                pats(&m.exempt),
            ]
        })
        .collect();
    for (p, m) in &parsed {
        if !p.matches_any(known) {
            rep.warnings.push(format!(
                "crate `{krate}`: module rule `{}` matches no module",
                m.path
            ));
        }
    }
    let mut used: HashSet<(usize, usize)> = HashSet::new();
    for f in files {
        let bare: &[crate::scan::RawRef] = if table.is_some() { &f.scan.bare } else { &[] };
        for r in f.scan.refs.iter().chain(bare) {
            let cur = [&f.module[..], &r.inline[..]].concat();
            let Some((src_idx, (src_pat, src_rule))) = source_rule(&parsed, &cur) else {
                continue;
            };
            let [allow_pats, deny_pats, exempt_pats] = &lists[src_idx];
            // The module the rule governs here: for a pattern, the module it
            // matched, so `tools::*` keeps each tool's own subtree its own.
            let sp = &cur[..src_pat.len()];
            let label = if src_pat.specificity().iter().all(|&s| s == 2) {
                format!("`{}`", src_rule.path)
            } else {
                format!("`{}` (rule `{}`)", sp.join("::"), src_rule.path)
            };
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
                if is_prefix(sp, &target) {
                    continue;
                }
                let reason = if let Some(i) = deny_pats.iter().position(|d| d.covers(&target)) {
                    Some(format!("{label} denies `{}`", src_rule.deny[i]))
                } else if let (true, Some(_)) = (as_written, &src_rule.depends_on) {
                    (!allow_pats.iter().any(|d| d.covers(&target)))
                        .then(|| format!("{label} does not list it in depends_on"))
                } else {
                    None
                };
                if reason.is_some() {
                    // An exemption covers the reference; every matching entry
                    // counts as used so staleness never depends on order.
                    let mut exempted = false;
                    for (i, e) in exempt_pats.iter().enumerate() {
                        if e.covers(&target) {
                            used.insert((src_idx, i));
                            exempted = true;
                        }
                    }
                    if exempted {
                        continue;
                    }
                }
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
                        stale: false,
                    });
                }
            }
        }
    }
    for (idx, (_, m)) in parsed.iter().enumerate() {
        for (i, e) in m.exempt.iter().enumerate() {
            if !used.contains(&(idx, i)) {
                rep.violations.push(ModViolation {
                    krate: krate.to_string(),
                    file: PathBuf::new(),
                    line: 0,
                    from: m.path.clone(),
                    to: e.clone(),
                    reference: String::new(),
                    reason: "stale exemption: no reference it suppresses".to_string(),
                    stale: true,
                });
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
