//! Directory rules: where crates live, and which directories their
//! dependencies (and, in `lints`, their source includes) may reach.

use crate::config::{Config, Layer, glob_match};
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

fn axis_index(axis: &[Layer], name: &str) -> Option<usize> {
    axis.iter()
        .position(|l| l.crates.iter().any(|g| glob_match(g, name)))
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

    // Placement: a crate lives under its layer's / group's directory.
    for p in pkgs.iter().filter(|p| p.member) {
        for (axis, what) in [(&cfg.layers, "layer"), (&cfg.groups, "group")] {
            let Some(i) = axis_index(axis, &p.name) else {
                continue;
            };
            let Some(base) = &axis[i].dir else { continue };
            if !is_under(&p.dir, base) {
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

pub fn check(meta: &Metadata, cfg: &Config) -> Vec<DirViolation> {
    let root: &std::path::Path = meta.workspace_root.as_std_path();
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
