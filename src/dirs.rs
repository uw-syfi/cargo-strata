//! Directory rules: where crates live, and which directories their
//! dependencies (and, in `lints`, their source includes) may reach.

use crate::config::{Config, glob_match};
use cargo_metadata::{DependencyKind, Metadata};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

/// A package whose sources live in the checkout (workspace member or path
/// dependency).
#[derive(Debug, Clone)]
pub struct Pkg {
    pub name: String,
    /// Directory relative to the workspace root, `/`-separated; empty for the root.
    pub dir: String,
    pub manifest: String,
    pub member: bool,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DirViolation {
    pub krate: String,
    pub manifest: String,
    pub kind: &'static str,
    pub reason: String,
}

/// `a/b/c` is under `a/b`; also true for equality.
pub fn is_under(dir: &str, base: &str) -> bool {
    let (d, b): (Vec<_>, Vec<_>) = (
        dir.split('/').filter(|s| !s.is_empty()).collect(),
        base.split('/').filter(|s| !s.is_empty()).collect(),
    );
    d.len() >= b.len() && d[..b.len()] == b[..]
}

/// A directory glob matches a directory if it matches that directory or any
/// ancestor of it (`vendor` covers `vendor/crates/x`).
pub fn dir_glob_matches(glob: &str, dir: &str) -> bool {
    let g = glob.trim_end_matches('/');
    let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    (1..=parts.len()).any(|n| glob_match(g, &parts[..n].join("/")))
}

/// Names bound by the `{name}` segments of a directory pattern.
pub type Captures = BTreeMap<String, String>;

/// A directory pattern: `/`-separated segments, each a name, a glob that
/// matches one name (`*`, `?`, `[..]`), or a capture `{name}` that matches
/// any one name and binds it. Empty (or `.`) is the root.
#[derive(Debug, Clone)]
pub struct DirPattern {
    segs: Vec<Seg>,
}

#[derive(Debug, Clone)]
enum Seg {
    Glob(glob::Pattern),
    Capture(String),
}

impl DirPattern {
    pub fn new(s: &str) -> Result<Self, String> {
        let mut segs = Vec::new();
        for part in s.split('/').filter(|p| !p.is_empty() && *p != ".") {
            if part == "**" {
                return Err("`**` is not supported; a segment matches one directory name".into());
            }
            if part == ".." {
                return Err("`..` is not allowed; set `root` to a directory above both".into());
            }
            let seg = match part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) {
                Some(name) => {
                    let ok = name
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphabetic() || c == '_')
                        && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                    if !ok {
                        return Err(format!(
                            "`{part}`: a capture is `{{name}}`, a name of letters, digits and `_`"
                        ));
                    }
                    if segs
                        .iter()
                        .any(|s| matches!(s, Seg::Capture(n) if n == name))
                    {
                        return Err(format!("capture `{{{name}}}` appears twice"));
                    }
                    Seg::Capture(name.to_string())
                }
                None if part.contains(['{', '}']) => {
                    return Err(format!(
                        "`{part}`: a capture is a whole segment, `{{name}}`"
                    ));
                }
                None => Seg::Glob(glob::Pattern::new(part).map_err(|e| format!("`{part}`: {e}"))?),
            };
            segs.push(seg);
        }
        Ok(DirPattern { segs })
    }

    /// The names of its captures, in order.
    pub fn captures(&self) -> impl Iterator<Item = &str> {
        self.segs.iter().filter_map(|s| match s {
            Seg::Capture(n) => Some(n.as_str()),
            Seg::Glob(_) => None,
        })
    }

    fn match_parts(&self, parts: &[&str]) -> Option<Captures> {
        let mut caps = Captures::new();
        for (seg, part) in self.segs.iter().zip(parts) {
            match seg {
                Seg::Glob(g) if g.matches(part) => {}
                Seg::Glob(_) => return None,
                Seg::Capture(n) => {
                    caps.insert(n.clone(), part.to_string());
                }
            }
        }
        Some(caps)
    }

    /// The captures if `dir` (relative, `/`-separated) is a match of this
    /// pattern or lies under one.
    pub fn contains(&self, dir: &str) -> Option<Captures> {
        let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        (parts.len() >= self.segs.len()).then(|| self.match_parts(&parts))?
    }

    /// The captures if `dir` is a match of this pattern.
    pub fn matches(&self, dir: &str) -> Option<Captures> {
        let parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        (parts.len() == self.segs.len()).then(|| self.match_parts(&parts))?
    }

    /// The pattern with each capture replaced by its value in `caps` (as a
    /// literal name). A capture `caps` lacks is an error naming it.
    pub fn substitute(&self, caps: &Captures) -> Result<DirPattern, String> {
        let segs = self
            .segs
            .iter()
            .map(|s| match s {
                Seg::Capture(n) => caps
                    .get(n)
                    .map(|v| Seg::Glob(glob::Pattern::new(&glob::Pattern::escape(v)).unwrap()))
                    .ok_or_else(|| format!("`{{{n}}}` is not captured")),
                Seg::Glob(_) => Ok(s.clone()),
            })
            .collect::<Result<_, _>>()?;
        Ok(DirPattern { segs })
    }

    /// Every existing directory under `root` this pattern matches, as
    /// root-relative paths with their captures, sorted.
    pub fn expand(&self, root: &std::path::Path) -> Vec<(String, Captures)> {
        let mut cur: Vec<Vec<String>> = vec![Vec::new()];
        for seg in &self.segs {
            let mut next = Vec::new();
            for parts in &cur {
                let dir = parts.iter().fold(root.to_path_buf(), |d, p| d.join(p));
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let ok = match seg {
                        Seg::Glob(g) => g.matches(&name),
                        Seg::Capture(_) => true,
                    };
                    if ok && e.file_type().is_ok_and(|t| t.is_dir()) {
                        next.push(parts.iter().cloned().chain([name]).collect());
                    }
                }
            }
            cur = next;
        }
        let mut out: Vec<(String, Captures)> = cur
            .into_iter()
            .map(|parts| {
                let caps = self
                    .match_parts(&parts.iter().map(String::as_str).collect::<Vec<_>>())
                    .unwrap();
                (parts.join("/"), caps)
            })
            .collect();
        out.sort();
        out
    }
}

/// `domain = sregym, version = v1`; empty for no captures.
pub fn show_captures(caps: &Captures) -> String {
    caps.iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pure evaluation. `edges` are (from, to, kind, optional) between any
/// packages in `pkgs`; edges from packages not in `pkgs` never matter.
pub fn evaluate(
    cfg: &Config,
    pkgs: &[Pkg],
    edges: &[(String, String, &'static str, bool)],
) -> Vec<DirViolation> {
    let mut out = Vec::new();
    let by_name: HashMap<&str, &Pkg> = pkgs.iter().map(|p| (p.name.as_str(), p)).collect();

    // Placement: a crate a layer or group holds by name lives under its
    // directory (a crate held by directory lives there by definition).
    for p in pkgs.iter().filter(|p| p.member) {
        for (axis, what) in [(&cfg.layers, "layer"), (&cfg.groups, "group")] {
            let Some(m) = crate::axis::member_of(axis, &p.name, Some(&p.dir)) else {
                continue;
            };
            let i = m.index;
            let Some(base) = &axis[i].dir else { continue };
            let placed = DirPattern::new(base).is_ok_and(|d| d.contains(&p.dir).is_some());
            if !placed {
                out.push(DirViolation {
                    krate: p.name.clone(),
                    manifest: p.manifest.clone(),
                    kind: "placement",
                    reason: format!(
                        "{what} `{}` belongs under `{}/`, found in `{}`",
                        axis[i].name,
                        base.trim_end_matches('/'),
                        p.dir
                    ),
                });
            }
        }
    }

    // Reach: BFS over every edge kind being checked, between checkout packages.
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (from, to, kind, optional) in edges {
        match *kind {
            "dev" if !cfg.check_dev => continue,
            "build" if !cfg.check_build => continue,
            _ => {}
        }
        if *optional && !cfg.check_optional {
            continue;
        }
        if by_name.contains_key(from.as_str()) && by_name.contains_key(to.as_str()) {
            adj.entry(from).or_default().push(to);
        }
    }
    let mut seen_pairs: BTreeSet<(String, String)> = BTreeSet::new();
    for p in pkgs.iter().filter(|p| p.member) {
        let rules: Vec<_> = cfg
            .crates
            .iter()
            .filter(|r| glob_match(&r.name, &p.name) && !r.deny_reach_dir.is_empty())
            .collect();
        if rules.is_empty() {
            continue;
        }
        let mut parent: HashMap<&str, &str> = HashMap::new();
        let mut q = VecDeque::from([p.name.as_str()]);
        let mut seen: HashSet<&str> = HashSet::from([p.name.as_str()]);
        while let Some(c) = q.pop_front() {
            for &n in adj.get(c).into_iter().flatten() {
                if seen.insert(n) {
                    parent.insert(n, c);
                    q.push_back(n);
                }
            }
        }
        let mut hits: Vec<&str> = seen.iter().copied().filter(|n| *n != p.name).collect();
        hits.sort();
        for h in hits {
            let hp = by_name[h];
            for r in &rules {
                if let Some(g) = r
                    .deny_reach_dir
                    .iter()
                    .find(|g| dir_glob_matches(g, &hp.dir))
                {
                    if !seen_pairs.insert((p.name.clone(), format!("{h}\0{g}"))) {
                        continue;
                    }
                    let mut path = vec![h];
                    while let Some(&up) = parent.get(path[path.len() - 1]) {
                        path.push(up);
                    }
                    path.reverse();
                    out.push(DirViolation {
                        krate: p.name.clone(),
                        manifest: p.manifest.clone(),
                        kind: "reach",
                        reason: format!(
                            "reaches `{h}` in `{}` ({}), denied by `deny_reach_dir = [\"{g}\"]`",
                            hp.dir,
                            path.join(" -> ")
                        ),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.krate, a.kind, &a.reason).cmp(&(&b.krate, b.kind, &b.reason)));
    out
}

/// Path of `abs` relative to `root`, `/`-separated; empty when outside.
pub fn rel_dir(root: &std::path::Path, abs: &std::path::Path) -> Option<String> {
    let r = abs.strip_prefix(root).ok()?;
    Some(
        r.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

pub fn check(meta: &Metadata, cfg: &Config, root: &std::path::Path) -> Vec<DirViolation> {
    let members: HashSet<_> = meta.workspace_members.iter().collect();
    let active = crate::crates::activated(meta);
    let mut pkgs = Vec::new();
    let mut edges = Vec::new();
    for p in meta.packages.iter().filter(|p| p.source.is_none()) {
        let mdir = p.manifest_path.as_std_path().parent().unwrap();
        let Some(dir) = rel_dir(root, mdir) else {
            continue;
        };
        pkgs.push(Pkg {
            name: p.name.to_string(),
            dir,
            manifest: p.manifest_path.to_string(),
            member: members.contains(&p.id),
        });
        for d in &p.dependencies {
            let kind = match d.kind {
                DependencyKind::Normal => "normal",
                DependencyKind::Development => "dev",
                DependencyKind::Build => "build",
                _ => continue,
            };
            let opt = d.optional && !active.contains(&(p.name.to_string(), d.name.clone(), kind));
            edges.push((p.name.to_string(), d.name.clone(), kind, opt));
        }
    }
    evaluate(cfg, &pkgs, &edges)
}

/// A path dependency of a member: the target's directory relative to the
/// root (`None` outside it) and as written in messages.
#[derive(Debug, Clone)]
pub struct PathDep {
    pub from: String,
    pub manifest: String,
    pub to: String,
    pub dir: Option<String>,
    pub shown: String,
}

/// `[[workspace]] path_deps`: every path dependency of a member of the
/// workspace at `ws_dir` lives under `ws_dir` or one of `allowed` (captures
/// already substituted), unless the member's `path_deps_exempt` names it.
/// `members` lists the workspace's members (name, manifest) for stale
/// exemptions; with `allowed` absent, every exemption is stale.
pub fn path_deps(
    cfg: &Config,
    ws_dir: &str,
    allowed: Option<&[DirPattern]>,
    members: &[(String, String)],
    deps: &[PathDep],
) -> Vec<DirViolation> {
    let mut out = Vec::new();
    let mut used: HashSet<(&str, &str)> = HashSet::new();
    let rules_of = |name: &str| {
        cfg.crates
            .iter()
            .filter(|r| glob_match(&r.name, name))
            .collect::<Vec<_>>()
    };
    if let Some(allowed) = allowed {
        for d in deps {
            let inside = d.dir.as_deref().is_some_and(|dir| {
                is_under(dir, ws_dir) || allowed.iter().any(|a| a.contains(dir).is_some())
            });
            if inside {
                continue;
            }
            let mut exempt = false;
            for r in rules_of(&d.from) {
                for g in r.path_deps_exempt.iter().filter(|g| glob_match(g, &d.to)) {
                    exempt = true;
                    used.insert((d.from.as_str(), g.as_str()));
                }
            }
            if !exempt {
                out.push(DirViolation {
                    krate: d.from.clone(),
                    manifest: d.manifest.clone(),
                    kind: "path-dep",
                    reason: format!(
                        "has a path dependency on `{}` in `{}`, outside its workspace `{}` and the directories its `path_deps` allows",
                        d.to,
                        d.shown,
                        if ws_dir.is_empty() { "." } else { ws_dir }
                    ),
                });
            }
        }
    }
    for (name, manifest) in members {
        for r in rules_of(name) {
            for g in &r.path_deps_exempt {
                if !used.contains(&(name.as_str(), g.as_str())) {
                    out.push(DirViolation {
                        krate: name.clone(),
                        manifest: manifest.clone(),
                        kind: "path-dep",
                        reason: format!(
                            "stale `path_deps_exempt = [\"{g}\"]` (rule `{}`): it suppresses no path dependency; delete it",
                            r.name
                        ),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.krate, &a.reason).cmp(&(&b.krate, &b.reason)));
    out.dedup();
    out
}

/// [`path_deps`] over `cargo metadata`: every dependency with a `path`,
/// any kind, of every member.
pub fn check_path_deps(
    meta: &Metadata,
    cfg: &Config,
    root: &std::path::Path,
    ws_dir: &str,
    allowed: Option<&[DirPattern]>,
) -> Vec<DirViolation> {
    let ids: HashSet<_> = meta.workspace_members.iter().collect();
    let mut members = Vec::new();
    let mut deps = Vec::new();
    for p in meta.packages.iter().filter(|p| ids.contains(&p.id)) {
        members.push((p.name.to_string(), p.manifest_path.to_string()));
        for d in &p.dependencies {
            let Some(path) = &d.path else { continue };
            let abs = crate::lints::normalize(path.as_std_path());
            deps.push(PathDep {
                from: p.name.to_string(),
                manifest: p.manifest_path.to_string(),
                to: d.name.clone(),
                dir: rel_dir(root, &abs),
                shown: rel_dir(root, &abs).unwrap_or_else(|| abs.display().to_string()),
            });
        }
    }
    path_deps(cfg, ws_dir, allowed, &members, &deps)
}
