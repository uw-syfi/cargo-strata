//! Source-pattern lints over [`Facts`]: construct authority, re-export
//! policy, unsafe ratchet and forbid, `#[path]`/`include!` reach, verus-only
//! crates. Pure evaluation: facts in, violations out.

use crate::config::{Authority, Config, CrateRule, Forbid, MainRule, UnsafePolicy, glob_match};
use crate::dirs::{dir_glob_matches, is_under};
use crate::facts::{Facts, Site};
use crate::resolve::Table;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

#[derive(Debug, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct LintViolation {
    pub kind: &'static str,
    /// Workspace-relative file (or manifest) the violation is anchored at.
    pub file: String,
    pub line: usize,
    pub msg: String,
}

pub struct FileInfo {
    pub krate: String,
    /// Relative to the workspace root, `/`-separated.
    pub rel: String,
    /// Relative to the crate directory, `/`-separated.
    pub crate_rel: String,
    pub abs: PathBuf,
    pub src: String,
    pub facts: Facts,
    /// `(pattern, line)` hits of the `[[forbid]]` patterns that apply to this crate.
    pub hits: Vec<(String, usize)>,
}

pub struct CrateInfo {
    pub name: String,
    pub manifest_rel: String,
    pub dir_abs: PathBuf,
    /// Lib and bin target roots, relative to the workspace root.
    pub roots: Vec<String>,
    /// Binary target roots (a subset of `roots`).
    pub bins: Vec<String>,
}

fn norm(n: &str) -> String {
    n.replace('-', "_")
}

/// Lexically normalize (`.` and `..` collapsed, no file system access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn in_dirs(crate_rel: &str, dirs: &[String]) -> bool {
    dirs.iter().any(|d| d == "." || is_under(crate_rel, d))
}

fn marker_lines(src: &str, marker: &str) -> Vec<usize> {
    src.lines()
        .enumerate()
        .filter(|(_, l)| l.contains(marker))
        .map(|(i, _)| i + 1)
        .collect()
}

/// Exempt if the line holds the marker, or the innermost fn containing the
/// line holds it somewhere in its text.
fn exempt(markers: &[usize], facts: &Facts, line: usize) -> bool {
    if markers.contains(&line) {
        return true;
    }
    let inner = facts
        .fns
        .iter()
        .filter(|(a, b)| *a <= line && line <= *b)
        .min_by_key(|(a, b)| b - a);
    match inner {
        Some((a, b)) => markers.iter().any(|m| a <= m && m <= b),
        None => false,
    }
}

/// A crate's name table and the module each of its files defines.
pub struct CrateRes {
    table: Table,
    file_mod: HashMap<PathBuf, Vec<String>>,
    /// Files outside the `mod` tree (tests, examples, ...): a table of their own.
    loose: HashMap<PathBuf, Table>,
}

/// The table and module for one file.
#[derive(Clone, Copy)]
struct FileRes<'a> {
    table: &'a Table,
    module: &'a [String],
    /// Tables of all workspace crates by normalized name, for re-exports that
    /// pass through a dependency.
    ws: &'a HashMap<String, CrateRes>,
}

/// Denied `(root, leaf)` pairs a path ends up naming: through this crate's
/// bindings (`res` is already resolved in `tbl`) and, when it leaves for a
/// workspace crate, through that crate's table (up to `depth` crates deep).
/// `star`: the path came from a glob import.
fn denied_targets(
    tbl: &Table,
    res: crate::resolve::Res,
    star: bool,
    deny: &BTreeSet<String>,
    ws: &HashMap<String, CrateRes>,
    depth: u32,
    out: &mut Vec<(String, String)>,
) {
    use crate::resolve::Res;
    match res {
        Res::External(p) if deny.contains(&norm(&p[0])) => {
            let leaf = if star {
                "*".to_string()
            } else {
                p.last().cloned().unwrap_or_default()
            };
            out.push((p[0].clone(), leaf));
        }
        Res::External(p) if depth > 0 => {
            if let Some(dep) = ws.get(&norm(&p[0])) {
                let tail: Vec<String> = ["crate".to_string()]
                    .into_iter()
                    .chain(p[1..].iter().cloned())
                    .collect();
                denied_targets(
                    &dep.table,
                    dep.table.resolve(&[], &tail),
                    star,
                    deny,
                    ws,
                    depth - 1,
                    out,
                );
            }
        }
        Res::Module(gm) if star => {
            for (name, r) in tbl.reexports(&gm) {
                if matches!(r, Res::External(_)) {
                    denied_targets(tbl, r, name.is_none(), deny, ws, depth, out);
                }
            }
        }
        _ => {}
    }
}

fn attach(
    files: &HashMap<&Path, &FileInfo>,
    file: &Path,
    module: Vec<String>,
    is_root: bool,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(PathBuf, Vec<String>)>,
) {
    let Some(info) = files.get(file) else { return };
    if !seen.insert(file.to_path_buf()) {
        return;
    }
    out.push((file.to_path_buf(), module.clone()));
    let dir = crate::modules::child_dir(file, is_root);
    for d in &info.facts.items.decls {
        let mut m = module.clone();
        m.extend(d.inline.iter().cloned());
        m.push(d.name.clone());
        let mut base = dir.clone();
        for i in &d.inline {
            base.push(crate::modules::fs_name(i));
        }
        let name = crate::modules::fs_name(&d.name);
        let candidates = match &d.path_attr {
            Some(p) => vec![file.parent().unwrap_or(Path::new(".")).join(p)],
            None => vec![
                base.join(format!("{name}.rs")),
                base.join(name).join("mod.rs"),
            ],
        };
        if let Some(c) = candidates.iter().find(|c| files.contains_key(c.as_path())) {
            attach(files, c, m, false, seen, out);
        }
    }
}

fn known_modules(mods: &[(&[String], &crate::resolve::Items)]) -> HashSet<Vec<String>> {
    let mut known: HashSet<Vec<String>> = HashSet::new();
    known.insert(vec![]);
    for (m, items) in mods {
        known.insert(m.to_vec());
        for im in &items.inline_mods {
            known.insert([&m[..], &im[..]].concat());
        }
    }
    known
}

impl CrateRes {
    fn build(ws_root: &Path, c: &CrateInfo, cfiles: &[&FileInfo]) -> CrateRes {
        let by_abs: HashMap<&Path, &FileInfo> =
            cfiles.iter().map(|f| (f.abs.as_path(), *f)).collect();
        let mut attached = Vec::new();
        let mut seen = HashSet::new();
        for r in &c.roots {
            attach(
                &by_abs,
                &ws_root.join(r),
                vec![],
                true,
                &mut seen,
                &mut attached,
            );
        }
        let units: Vec<(&[String], &crate::resolve::Items)> = attached
            .iter()
            .map(|(p, m)| (&m[..], &by_abs[p.as_path()].facts.items))
            .collect();
        let known = known_modules(&units);
        let table = Table::build(&known, units);
        let file_mod: HashMap<PathBuf, Vec<String>> = attached.into_iter().collect();
        let mut loose = HashMap::new();
        for f in cfiles {
            if !file_mod.contains_key(&f.abs) {
                let root: &[String] = &[];
                let known = known_modules(&[(root, &f.facts.items)]);
                loose.insert(
                    f.abs.clone(),
                    Table::build(&known, [(root, &f.facts.items)]),
                );
            }
        }
        CrateRes {
            table,
            file_mod,
            loose,
        }
    }

    fn of<'a>(&'a self, f: &FileInfo, ws: &'a HashMap<String, CrateRes>) -> FileRes<'a> {
        match self.file_mod.get(&f.abs) {
            Some(m) => FileRes {
                table: &self.table,
                module: m,
                ws,
            },
            None => FileRes {
                table: self.loose.get(&f.abs).unwrap_or(&self.table),
                module: &[],
                ws,
            },
        }
    }
}

impl FileRes<'_> {
    /// Resolve a site's path (with `Self` replaced by the impl's type).
    fn site_path(&self, s: &Site, segs: &[String]) -> Option<Vec<String>> {
        let scope = [self.module, &s.inline[..]].concat();
        let full = if segs.first().is_some_and(|x| x == "Self") {
            [&s.self_ty.clone()?[..], &segs[1..]].concat()
        } else {
            segs.to_vec()
        };
        Some(self.table.resolve(&scope, &full).path())
    }

    /// Could name resolution change what the last segment of `segs` is?
    fn may_rename(&self, segs: &[String]) -> bool {
        segs.first().is_some_and(|x| x == "Self")
            || segs.last().is_some_and(|l| self.table.is_renamed(l))
    }
}

fn authority(a: &Authority, f: &FileInfo, rs: Option<FileRes>, out: &mut Vec<LintViolation>) {
    if a.allow.iter().any(|g| glob_match(g, &f.rel)) {
        return;
    }
    let markers = a
        .exempt_marker
        .as_deref()
        .map(|m| marker_lines(&f.src, m))
        .unwrap_or_default();
    let mut push = |what: String, line: usize| {
        if !exempt(&markers, &f.facts, line) {
            out.push(LintViolation {
                kind: "authority",
                file: f.rel.clone(),
                line,
                msg: format!(
                    "`{what}`: only {} may construct {}",
                    if a.allow.is_empty() {
                        "no file".to_string()
                    } else {
                        a.allow
                            .iter()
                            .map(|g| format!("`{g}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    a.name
                ),
            });
        }
    };
    if let Some(ty) = &a.ty {
        if a.literal {
            for (n, line) in &f.facts.struct_lits {
                if n == ty {
                    push(format!("{ty} {{ .. }}"), *line);
                }
            }
        }
        for (segs, line) in &f.facts.calls {
            if segs.len() >= 2
                && &segs[segs.len() - 2] == ty
                && a.assoc.contains(&segs[segs.len() - 1])
            {
                push(format!("{ty}::{}(..)", segs[segs.len() - 1]), *line);
            }
        }
    }
    for (segs, line) in &f.facts.calls {
        let last = &segs[segs.len() - 1];
        if a.calls.contains(last) {
            push(format!("{}(..)", segs.join("::")), *line);
        }
    }
    // Names that resolve to a listed type or function through a rename, a type
    // alias or `Self`; the as-written checks above already fired for the rest.
    let Some(rs) = rs else { return };
    if let Some(ty) = &a.ty {
        if a.literal {
            for s in &f.facts.lit_sites {
                if s.segs.last() != Some(ty)
                    && rs.may_rename(&s.segs)
                    && let Some(p) = rs.site_path(s, &s.segs)
                    && p.last() == Some(ty)
                {
                    push(
                        format!("{ty} {{ .. }} (written `{}`)", s.segs.join("::")),
                        s.line,
                    );
                }
            }
        }
        for s in &f.facts.call_sites {
            let n = s.segs.len();
            if n >= 2
                && &s.segs[n - 2] != ty
                && a.assoc.contains(&s.segs[n - 1])
                && rs.may_rename(&s.segs[..n - 1])
                && let Some(p) = rs.site_path(s, &s.segs[..n - 1])
                && p.last() == Some(ty)
            {
                push(
                    format!(
                        "{ty}::{}(..) (written `{}`)",
                        s.segs[n - 1],
                        s.segs.join("::")
                    ),
                    s.line,
                );
            }
        }
    }
    for s in &f.facts.call_sites {
        let last = &s.segs[s.segs.len() - 1];
        if !a.calls.contains(last)
            && rs.may_rename(&s.segs)
            && let Some(p) = rs.site_path(s, &s.segs)
            && p.last().is_some_and(|l| a.calls.contains(l))
        {
            push(
                format!("{}(..) (written `{}`)", p.join("::"), s.segs.join("::")),
                s.line,
            );
        }
    }
}

fn has_tag(src_lines: &[&str], line: usize, tag: &str) -> bool {
    let has = |s: &str| s.find("//").is_some_and(|i| s[i..].contains(tag));
    let idx = line - 1;
    let mut j = idx as isize;
    while j >= 0 {
        let s = src_lines.get(j as usize).map(|s| s.trim()).unwrap_or("");
        if j as usize != idx
            && !(s.starts_with("//") || s.starts_with("#[") || s.starts_with("#!["))
        {
            break;
        }
        if has(s) {
            return true;
        }
        j -= 1;
    }
    false
}

fn forbid(
    fb: &Forbid,
    f: &FileInfo,
    used: &mut BTreeSet<(String, String)>,
    out: &mut Vec<LintViolation>,
) {
    let allowed = fb.allow.iter().any(|g| glob_match(g, &f.rel));
    let lines: Vec<&str> = f.src.lines().collect();
    for (pat, line) in f.hits.iter().filter(|(p, _)| fb.patterns.contains(p)) {
        if !allowed {
            if fb
                .exempt
                .iter()
                .any(|e| e.file == f.rel && &e.pattern == pat)
            {
                used.insert((f.rel.clone(), pat.clone()));
                continue;
            }
            out.push(LintViolation {
                kind: "forbid",
                file: f.rel.clone(),
                line: *line,
                msg: format!("`{pat}` outside the files `{}` allows", fb.name),
            });
        } else if let Some(tag) = &fb.require_comment
            && !has_tag(&lines, *line, tag)
        {
            out.push(LintViolation {
                kind: "forbid",
                file: f.rel.clone(),
                line: *line,
                msg: format!("`{pat}` has no `// {tag}` comment (`{}`)", fb.name),
            });
        }
    }
}

fn main_rule(m: &MainRule, f: &FileInfo, out: &mut Vec<LintViolation>) {
    let mut push = |line: usize, msg: String| {
        out.push(LintViolation {
            kind: "main",
            file: f.rel.clone(),
            line,
            msg,
        })
    };
    if !f.facts.has_main {
        push(1, "no top-level `fn main`".into());
        return;
    }
    let mut entry_calls: BTreeMap<&str, usize> = BTreeMap::new();
    for (segs, line) in &f.facts.main_calls {
        let path = segs.join("::");
        if let Some(e) = m.entries.iter().find(|e| **e == path) {
            *entry_calls.entry(e).or_default() += 1;
        } else if !m.calls.contains(&path) {
            push(
                *line,
                format!(
                    "main calls `{path}`: it may call only {} and entries {}",
                    quote(&m.calls),
                    quote(&m.entries)
                ),
            );
        }
    }
    for (name, line) in &f.facts.main_methods {
        if !m.methods.contains(name) {
            push(
                *line,
                format!(
                    "main calls method `.{name}()`: it may call only {}",
                    quote(&m.methods)
                ),
            );
        }
    }
    if !m.entries.is_empty() && entry_calls.is_empty() {
        push(
            1,
            format!("main calls none of the entries {}", quote(&m.entries)),
        );
    }
    for (e, k) in entry_calls {
        if k > 1 {
            push(1, format!("main calls entry `{e}` {k} times; at most once"));
        }
    }
}

fn quote(l: &[String]) -> String {
    l.iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn reexports(
    krate: &str,
    deny: &BTreeSet<String>,
    allow: &BTreeSet<String>,
    f: &FileInfo,
    rs: Option<FileRes>,
    out: &mut Vec<LintViolation>,
) {
    let mut by_line: BTreeMap<usize, (BTreeSet<String>, String)> = BTreeMap::new();
    for u in &f.facts.pub_uses {
        let Some(root) = u.segs.first() else { continue };
        if !deny.contains(&norm(root)) {
            // Not a denied crate as written: it may still be one through a
            // local alias (`use denied as d`) or a module that re-exports it.
            if let Some(rs) = rs {
                let scope = [rs.module, &u.inline[..]].concat();
                let mut hits: Vec<(String, String)> = Vec::new(); // (root, leaf)
                let res = rs.table.resolve(&scope, &u.segs);
                denied_targets(rs.table, res, u.glob, deny, rs.ws, 8, &mut hits);
                for (root, leaf) in hits {
                    if leaf != "*" && allow.contains(&leaf) {
                        continue;
                    }
                    by_line
                        .entry(u.line)
                        .or_insert_with(|| (BTreeSet::new(), root))
                        .0
                        .insert(leaf);
                }
            }
            continue;
        }
        let leaf = if u.glob {
            "*".to_string()
        } else {
            u.segs.last().cloned().unwrap_or_default()
        };
        if !u.glob && allow.contains(&leaf) {
            continue;
        }
        let e = by_line
            .entry(u.line)
            .or_insert_with(|| (BTreeSet::new(), root.clone()));
        e.0.insert(leaf);
    }
    for (line, (extra, root)) in by_line {
        out.push(LintViolation {
            kind: "reexport",
            file: f.rel.clone(),
            line,
            msg: format!(
                "`{krate}` re-exports `{root}` items ({}); allowed: {}",
                extra.into_iter().collect::<Vec<_>>().join(", "),
                if allow.is_empty() {
                    "none".to_string()
                } else {
                    allow.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            ),
        });
    }
}

fn source_reach(
    r: &CrateRule,
    c: &CrateInfo,
    f: &FileInfo,
    ws_root: &Path,
    roots: &BTreeSet<&str>,
    out: &mut Vec<LintViolation>,
) {
    let fdir = f.abs.parent().unwrap_or(Path::new("."));
    let stem = f.abs.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let child = if stem == "mod" || roots.contains(f.rel.as_str()) {
        fdir.to_path_buf()
    } else {
        fdir.join(stem)
    };
    let mut check = |target: PathBuf, line: usize, how: String| {
        let t = normalize(&target);
        let Ok(rel) = t.strip_prefix(ws_root) else {
            return;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if let Some(g) = r.deny_source_dir.iter().find(|g| dir_glob_matches(g, &rel)) {
            out.push(LintViolation {
                kind: "source-reach",
                file: f.rel.clone(),
                line,
                msg: format!(
                    "{how} pulls in `{rel}`, denied by `deny_source_dir = [\"{g}\"]` for `{}`",
                    c.name
                ),
            });
        }
    };
    for p in &f.facts.path_attrs {
        let base = if p.inline.is_empty() {
            fdir.to_path_buf()
        } else {
            let mut b = child.clone();
            b.extend(p.inline.iter().map(|i| crate::modules::fs_name(i)));
            b
        };
        check(
            base.join(&p.path),
            p.line,
            format!("`#[path = \"{}\"]`", p.path),
        );
    }
    for i in &f.facts.includes {
        if i.dynamic {
            continue;
        }
        let target = if i.manifest_dir {
            PathBuf::from(format!("{}{}", c.dir_abs.display(), i.text))
        } else {
            fdir.join(&i.text)
        };
        check(target, i.line, format!("`{}!(\"{}\")`", i.mac, i.text));
    }
}

/// Evaluate every source lint. `ratchet` is crate -> allowed unsafe sites.
pub fn evaluate(
    cfg: &Config,
    ws_root: &Path,
    crates: &[CrateInfo],
    files: &[FileInfo],
    ratchet: &BTreeMap<String, u64>,
    ratchet_file: &str,
) -> Vec<LintViolation> {
    let mut out = Vec::new();
    let mut ratcheted: BTreeSet<&str> = BTreeSet::new();
    let mut exempt_used: BTreeSet<(String, String)> = BTreeSet::new();
    // Name tables for every scanned crate (a re-export may pass through a
    // workspace dependency), built once when some lint needs them.
    let needs_tables = cfg.resolve
        && (!cfg.authority.is_empty()
            || cfg.crates.iter().any(|r| {
                !r.deny_reexport.is_empty() && !r.allow_reexport.iter().any(|a| a == "*")
            }));
    let tables: HashMap<String, CrateRes> = if needs_tables {
        crates
            .iter()
            .map(|c| {
                let cf: Vec<&FileInfo> = files.iter().filter(|f| f.krate == c.name).collect();
                (norm(&c.name), CrateRes::build(ws_root, c, &cf))
            })
            .collect()
    } else {
        HashMap::new()
    };
    for c in crates {
        let rules: Vec<&CrateRule> = cfg
            .crates
            .iter()
            .filter(|r| glob_match(&r.name, &c.name))
            .collect();
        let cfiles: Vec<&FileInfo> = files.iter().filter(|f| f.krate == c.name).collect();
        let cres = tables.get(&norm(&c.name));
        let roots: BTreeSet<&str> = c.roots.iter().map(String::as_str).collect();

        let deny: BTreeSet<String> = rules
            .iter()
            .flat_map(|r| r.deny_reexport.iter().map(|s| norm(s)))
            .collect();
        let allow: BTreeSet<String> = rules
            .iter()
            .flat_map(|r| r.allow_reexport.iter().cloned())
            .collect();
        let reexport_on = !deny.is_empty() && !allow.contains("*");

        for a in &cfg.authority {
            if !a.crates.iter().any(|g| glob_match(g, &c.name)) {
                continue;
            }
            for f in cfiles.iter().filter(|f| in_dirs(&f.crate_rel, &a.dirs)) {
                authority(a, f, cres.map(|r| r.of(f, &tables)), &mut out);
            }
        }

        for fb in &cfg.forbids {
            if !fb.crates.iter().any(|g| glob_match(g, &c.name)) {
                continue;
            }
            for f in cfiles.iter().filter(|f| in_dirs(&f.crate_rel, &fb.dirs)) {
                forbid(fb, f, &mut exempt_used, &mut out);
            }
        }

        if reexport_on {
            for f in cfiles
                .iter()
                .filter(|f| in_dirs(&f.crate_rel, &["src".into()]))
            {
                reexports(
                    &c.name,
                    &deny,
                    &allow,
                    f,
                    cres.map(|r| r.of(f, &tables)),
                    &mut out,
                );
            }
        }

        if rules.iter().any(|r| !r.deny_source_dir.is_empty()) {
            for r in rules.iter().filter(|r| !r.deny_source_dir.is_empty()) {
                for f in &cfiles {
                    source_reach(r, c, f, ws_root, &roots, &mut out);
                }
            }
        }

        for r in &rules {
            let Some(m) = &r.main else { continue };
            for b in &c.bins {
                if let Some(f) = cfiles.iter().find(|f| &f.rel == b) {
                    main_rule(m, f, &mut out);
                }
            }
        }

        if rules.iter().any(|r| r.verus_only) {
            for f in cfiles
                .iter()
                .filter(|f| in_dirs(&f.crate_rel, &["src".into()]))
            {
                for (line, what) in &f.facts.plain_items {
                    out.push(LintViolation {
                        kind: "verus-only",
                        file: f.rel.clone(),
                        line: *line,
                        msg: format!("`{what}` item outside `verus!` in `{}`", c.name),
                    });
                }
            }
        }

        let policies: BTreeSet<UnsafePolicy> =
            rules.iter().filter_map(|r| r.unsafe_policy).collect();
        if policies.contains(&UnsafePolicy::Ratchet) {
            ratcheted.insert(&c.name);
            let n: usize = cfiles
                .iter()
                .filter(|f| in_dirs(&f.crate_rel, &["src".into()]))
                .map(|f| f.facts.unsafe_sites.len())
                .sum();
            let mk = |msg: String| LintViolation {
                kind: "unsafe",
                file: c.manifest_rel.clone(),
                line: 0,
                msg,
            };
            match ratchet.get(&c.name) {
                None => out.push(mk(format!("`{}` has no entry in `{ratchet_file}` (it has {n} unsafe sites)", c.name))),
                Some(&w) if (n as u64) > w => out.push(mk(format!("`{}` has {n} unsafe sites, ratchet allows {w}; justify and raise `{ratchet_file}` in the same commit", c.name))),
                Some(&w) if (n as u64) < w => out.push(mk(format!("`{}` has {n} unsafe sites, ratchet says {w}; lower `{ratchet_file}` to {n}", c.name))),
                _ => {}
            }
        }
        if policies.contains(&UnsafePolicy::Forbid) {
            for r in &c.roots {
                let ok = files
                    .iter()
                    .find(|f| &f.rel == r && f.krate == c.name)
                    .map(|f| f.facts.forbid_unsafe);
                if ok != Some(true) {
                    out.push(LintViolation {
                        kind: "unsafe",
                        file: r.clone(),
                        line: 1,
                        msg: format!("`{}` lacks `#![forbid(unsafe_code)]` (crate `{}` has `unsafe = \"forbid\"`)", r, c.name),
                    });
                }
            }
        }
    }
    for fb in &cfg.forbids {
        for e in &fb.exempt {
            if !exempt_used.contains(&(e.file.clone(), e.pattern.clone())) {
                out.push(LintViolation {
                    kind: "forbid",
                    file: e.file.clone(),
                    line: 0,
                    msg: format!(
                        "stale exemption of `{}` in `{}`: no such hit; delete it",
                        e.pattern, fb.name
                    ),
                });
            }
        }
    }
    for k in ratchet.keys() {
        if !ratcheted.contains(k.as_str()) {
            out.push(LintViolation {
                kind: "unsafe",
                file: ratchet_file.to_string(),
                line: 0,
                msg: format!(
                    "entry for `{k}`, which has no `unsafe = \"ratchet\"` crate rule; delete it"
                ),
            });
        }
    }
    out.sort();
    out.dedup();
    out
}

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let name = e.file_name();
        if p.is_dir() {
            if name != "target" && name != ".git" {
                rs_files(&p, out);
            }
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Which directories of a crate the configured lints need parsed.
fn scan_dirs(cfg: &Config, name: &str) -> Vec<&'static str> {
    let rules: Vec<&CrateRule> = cfg
        .crates
        .iter()
        .filter(|r| glob_match(&r.name, name))
        .collect();
    let whole = rules.iter().any(|r| !r.deny_source_dir.is_empty())
        || cfg.authority.iter().any(|a| {
            a.crates.iter().any(|g| glob_match(g, name)) && a.dirs.iter().any(|d| d == ".")
        });
    if whole {
        return vec!["."];
    }
    let whole = whole
        || cfg.forbids.iter().any(|f| {
            f.crates.iter().any(|g| glob_match(g, name)) && f.dirs.iter().any(|d| d == ".")
        });
    if whole {
        return vec!["."];
    }
    let src = cfg
        .forbids
        .iter()
        .any(|f| f.crates.iter().any(|g| glob_match(g, name)))
        || rules.iter().any(|r| {
            !r.deny_reexport.is_empty()
                || r.unsafe_policy.is_some()
                || r.verus_only
                || r.main.is_some()
        })
        || cfg
            .authority
            .iter()
            .any(|a| a.crates.iter().any(|g| glob_match(g, name)));
    if src { vec!["src"] } else { vec![] }
}

struct Job {
    krate: String,
    dir_abs: PathBuf,
    abs: PathBuf,
    parse_verus: bool,
    pats: Vec<String>,
}

fn scan_job(j: &Job, ws_root: &Path, skip: bool) -> Result<FileInfo, String> {
    let rel = crate::dirs::rel_dir(ws_root, &j.abs).unwrap_or_default();
    let src = std::fs::read_to_string(&j.abs)
        .map_err(|e| format!("{}: cannot read: {e}", j.abs.display()))?;
    let facts = crate::facts::scan_facts(&src, j.parse_verus, skip)
        .map_err(|e| format!("{rel}: parse error: {e}"))?;
    let hits = if j.pats.is_empty() {
        Vec::new()
    } else {
        crate::facts::token_hits(&src, &j.pats).map_err(|e| format!("{rel}: parse error: {e}"))?
    };
    let crate_rel = j
        .abs
        .strip_prefix(&j.dir_abs)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    Ok(FileInfo {
        krate: j.krate.clone(),
        rel,
        crate_rel,
        abs: j.abs.clone(),
        src,
        facts,
        hits,
    })
}

/// Map `f` over `items` on up to eight threads, keeping order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(8);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<(usize, R)>> = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|sc| {
        for _ in 0..n {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    out.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

pub struct Report {
    pub violations: Vec<LintViolation>,
    pub errors: Vec<String>,
}

pub fn check(
    meta: &cargo_metadata::Metadata,
    cfg: &Config,
    verus_feature: bool,
) -> Result<Report, String> {
    let ws_root: PathBuf = meta.workspace_root.clone().into();
    let members: std::collections::HashSet<_> = meta.workspace_members.iter().collect();
    let mut crates = Vec::new();
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let mut jobs: Vec<Job> = Vec::new();
    for pkg in meta.packages.iter().filter(|p| members.contains(&p.id)) {
        let dirs = scan_dirs(cfg, &pkg.name);
        if dirs.is_empty() {
            continue;
        }
        let rel = |p: &Path| crate::dirs::rel_dir(&ws_root, p).unwrap_or_default();
        let dir_abs: PathBuf = pkg
            .manifest_path
            .as_std_path()
            .parent()
            .unwrap()
            .to_path_buf();
        let parse_verus = verus_feature
            && cfg
                .crates
                .iter()
                .any(|r| glob_match(&r.name, &pkg.name) && r.verus);
        let roots: Vec<String> = pkg
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
            .map(|t| rel(t.src_path.as_std_path()))
            .collect();
        let bins: Vec<String> = pkg
            .targets
            .iter()
            .filter(|t| t.kind.iter().any(|k| k.to_string() == "bin"))
            .map(|t| rel(t.src_path.as_std_path()))
            .collect();
        crates.push(CrateInfo {
            bins,
            name: pkg.name.to_string(),
            manifest_rel: rel(pkg.manifest_path.as_std_path()),
            dir_abs: dir_abs.clone(),
            roots,
        });
        let mut paths = Vec::new();
        for d in &dirs {
            rs_files(
                &if *d == "." {
                    dir_abs.clone()
                } else {
                    dir_abs.join(d)
                },
                &mut paths,
            );
        }
        let pats: Vec<String> = cfg
            .forbids
            .iter()
            .filter(|f| f.crates.iter().any(|g| glob_match(g, &pkg.name)))
            .flat_map(|f| f.patterns.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for abs in paths {
            let r = rel(&abs);
            if cfg.lint_exclude.iter().any(|g| glob_match(g, &r)) {
                continue;
            }
            jobs.push(Job {
                krate: pkg.name.to_string(),
                dir_abs: dir_abs.clone(),
                abs,
                parse_verus,
                pats: pats.clone(),
            });
        }
    }
    let skip = cfg.lint_skip_cfg_test;
    for r in par_map(&jobs, |j| scan_job(j, &ws_root, skip)) {
        match r {
            Ok(f) => files.push(f),
            Err(e) => errors.push(e),
        }
    }
    let (ratchet, ratchet_file) = match &cfg.unsafe_ratchet {
        Some(p) => {
            let text = std::fs::read_to_string(ws_root.join(p))
                .map_err(|e| format!("cannot read unsafe_ratchet `{p}`: {e}"))?;
            let m: BTreeMap<String, u64> =
                serde_json::from_str(&text).map_err(|e| format!("{p}: {e}"))?;
            (m, p.clone())
        }
        None => (BTreeMap::new(), "unsafe_ratchet".to_string()),
    };
    let violations = evaluate(cfg, &ws_root, &crates, &files, &ratchet, &ratchet_file);
    Ok(Report { violations, errors })
}
