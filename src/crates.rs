//! Crate-level rules over `cargo metadata`.

use crate::config::{Config, Layer, glob_match};
use cargo_metadata::{DependencyKind, Metadata};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug)]
pub struct CrateViolation {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub manifest: String,
    pub line: Option<usize>,
    pub reason: String,
}

/// One dependency edge between a workspace crate and any crate.
/// `kind` is `"normal"`, `"dev"` or `"build"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: &'static str,
    pub manifest: String,
    pub optional: bool,
}

fn axis_of(axis: &[Layer], name: &str) -> Option<usize> {
    axis.iter()
        .position(|l| l.crates.iter().any(|g| glob_match(g, name)))
}

/// Why `from -> to` breaks the axis, if it does. Both must be members.
fn axis_violation(axis: &[Layer], what: &str, from: &str, to: &str, kind: &str) -> Option<String> {
    let a = axis_of(axis, from)?;
    let l = &axis[a];
    if let Some(g) = l.deny.iter().find(|g| glob_match(g, to)) {
        return Some(format!("{what} `{}` denies `{g}`", l.name));
    }
    if let Some(g) = l
        .deny_normal
        .iter()
        .find(|g| glob_match(g, to))
        .filter(|_| kind == "normal")
    {
        return Some(format!(
            "{what} `{}` denies `{g}` as a normal dependency",
            l.name
        ));
    }
    let b = axis_of(axis, to)?;
    match &axis[a].may_depend_on {
        Some(list) => (!list.contains(&axis[b].name)).then(|| {
            format!(
                "{what} `{}` may depend on {} only, not `{}`",
                axis[a].name,
                quote_list(list),
                axis[b].name
            )
        }),
        None => (b > a).then(|| {
            format!(
                "{what} `{}` may not depend on higher {what} `{}`",
                axis[a].name, axis[b].name
            )
        }),
    }
}

fn quote_list(l: &[String]) -> String {
    if l.is_empty() {
        return "nothing".into();
    }
    l.iter()
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn any_glob(globs: &[String], name: &str) -> bool {
    globs.iter().any(|g| glob_match(g, name))
}

fn manifest_line(manifest: &str, dep: &str) -> Option<usize> {
    let text = std::fs::read_to_string(manifest).ok()?;
    text.lines()
        .position(|l| {
            let t = l.trim_start();
            t.starts_with(dep) && t[dep.len()..].trim_start().starts_with(['=', '.', ']'])
                || t.contains(&format!("[dependencies.{dep}]"))
                || t.contains(&format!("[dev-dependencies.{dep}]"))
                || t.contains(&format!("[build-dependencies.{dep}]"))
        })
        .map(|i| i + 1)
}

/// Pure rule evaluation: workspace crates (name -> manifest path) plus edges
/// in, `(edge, reason)` per violation out, sorted. An edge may yield several
/// reasons. Stale exemptions and missing axis membership use `to == ""`.
pub fn evaluate(
    cfg: &Config,
    workspace: &BTreeMap<String, String>,
    edges: &[Edge],
) -> Vec<(Edge, String)> {
    let mut out: Vec<(Edge, String)> = Vec::new();
    // (crate, exemption glob, axis) that suppressed a real violation.
    let mut used: HashSet<(String, String, &'static str)> = HashSet::new();

    for e in edges {
        match e.kind {
            "dev" if !cfg.check_dev => continue,
            "build" if !cfg.check_build => continue,
            _ => {}
        }
        if e.optional && !cfg.check_optional {
            continue;
        }
        let rules: Vec<_> = cfg
            .crates
            .iter()
            .filter(|r| glob_match(&r.name, &e.from))
            .collect();
        let internal = workspace.contains_key(&e.to);
        let mut push = |reason: String| out.push((e.clone(), reason));

        for r in &rules {
            if let Some(g) = r.deny.iter().find(|g| glob_match(g, &e.to)) {
                push(format!("denied by `deny = [\"{g}\"]` for `{}`", r.name));
            }
            if e.kind == "normal"
                && let Some(g) = r.deny_normal.iter().find(|g| glob_match(g, &e.to))
            {
                push(format!(
                    "denied by `deny_normal = [\"{g}\"]` for `{}`",
                    r.name
                ));
            }
        }
        if internal {
            for r in &rules {
                let Some(allow) = &r.allow else { continue };
                if !(any_glob(allow, &e.to) || e.kind == "dev" && any_glob(&r.allow_dev, &e.to)) {
                    push(format!("not in the `allow` list of `{}`", r.name));
                }
            }
            for (axis, what, tag, exempt_of, on) in [
                (
                    &cfg.layers,
                    "layer",
                    "layer",
                    0,
                    e.kind != "dev" || cfg.layer_dev,
                ),
                (
                    &cfg.groups,
                    "group",
                    "group",
                    1,
                    e.kind != "dev" || cfg.group_dev,
                ),
            ] {
                if !on {
                    continue;
                }
                let Some(reason) = axis_violation(axis, what, &e.from, &e.to, e.kind) else {
                    continue;
                };
                // Every matching exemption is marked used, so which of two
                // overlapping globs is reported stale never depends on order.
                let mut exempt = false;
                for r in &rules {
                    let l = if exempt_of == 0 {
                        &r.layer_exempt
                    } else {
                        &r.group_exempt
                    };
                    for g in l.iter().filter(|g| glob_match(g, &e.to)) {
                        exempt = true;
                        used.insert((e.from.clone(), g.clone(), tag));
                    }
                }
                if !exempt {
                    push(reason);
                }
            }
        } else if e.kind == "normal"
            && let Some(ax) = axis_of(&cfg.layers, &e.from).map(|i| &cfg.layers[i])
        {
            if let Some(g) = ax.external_deny.iter().find(|g| glob_match(g, &e.to)) {
                push(format!(
                    "layer `{}` may not depend on external `{g}`",
                    ax.name
                ));
            } else if ax
                .external_allow
                .as_ref()
                .is_some_and(|l| !any_glob(l, &e.to))
            {
                push(format!(
                    "layer `{}` may depend on external crates {} only",
                    ax.name,
                    quote_list(ax.external_allow.as_deref().unwrap_or(&[]))
                ));
            }
        }
    }

    // Transitive reachability over normal edges between workspace crates.
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in edges
        .iter()
        .filter(|e| e.kind == "normal" && workspace.contains_key(&e.to))
        .filter(|e| cfg.check_optional || !e.optional)
    {
        adj.entry(&e.from).or_default().push(&e.to);
    }
    for name in workspace.keys() {
        for r in cfg
            .crates
            .iter()
            .filter(|r| glob_match(&r.name, name) && !r.deny_reach.is_empty())
        {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut stack = vec![name.as_str()];
            while let Some(c) = stack.pop() {
                for &n in adj.get(c).into_iter().flatten() {
                    if seen.insert(n) {
                        stack.push(n);
                    }
                }
            }
            let mut hits: Vec<&&str> = seen.iter().filter(|n| any_glob(&r.deny_reach, n)).collect();
            hits.sort();
            for h in hits {
                let manifest = workspace[name].clone();
                out.push((
                    Edge {
                        from: name.clone(),
                        to: (*h).to_string(),
                        kind: "normal",
                        manifest,
                        optional: false,
                    },
                    format!(
                        "`{h}` is reachable through normal dependencies (`deny_reach` for `{}`)",
                        r.name
                    ),
                ));
            }
        }
    }

    // Stale exemptions: only judged for edge kinds being checked.
    for r in &cfg.crates {
        for (tag, list) in [("layer", &r.layer_exempt), ("group", &r.group_exempt)] {
            for g in list {
                for name in workspace.keys().filter(|n| glob_match(&r.name, n)) {
                    if !used.contains(&(name.clone(), g.clone(), tag)) {
                        out.push((
                            Edge {
                                from: name.clone(),
                                to: g.clone(),
                                kind: "normal",
                                manifest: workspace[name].clone(),
                                optional: false,
                            },
                            format!("stale `{tag}_exempt`: no existing {tag} violation to this dependency"),
                        ));
                    }
                }
            }
        }
    }

    for (axis, key, required) in [
        (&cfg.layers, "layer", cfg.require_layer),
        (&cfg.groups, "group", cfg.require_group),
    ] {
        if required {
            for (name, manifest) in workspace {
                if axis_of(axis, name).is_none() {
                    out.push((
                        Edge {
                            from: name.clone(),
                            to: String::new(),
                            kind: "normal",
                            manifest: manifest.clone(),
                            optional: false,
                        },
                        format!("crate belongs to no `{key}`"),
                    ));
                }
            }
        }
    }

    out.sort_by(|a, b| {
        (&a.0.from, &a.0.to, a.0.kind, &a.1).cmp(&(&b.0.from, &b.0.to, b.0.kind, &b.1))
    });
    out.dedup();
    out
}

/// `(from, to, kind)` for every dependency present in the resolve graph, so an
/// optional dependency switched on by a default feature is not treated as
/// off. Empty when the metadata has no resolve section.
pub fn activated(meta: &Metadata) -> HashSet<(String, String, &'static str)> {
    let mut out = HashSet::new();
    let Some(res) = &meta.resolve else { return out };
    let name: std::collections::HashMap<_, _> = meta
        .packages
        .iter()
        .map(|p| (&p.id, p.name.to_string()))
        .collect();
    for node in &res.nodes {
        let Some(from) = name.get(&node.id) else {
            continue;
        };
        for d in &node.deps {
            let Some(to) = name.get(&d.pkg) else { continue };
            for k in &d.dep_kinds {
                let kind = match k.kind {
                    DependencyKind::Normal => "normal",
                    DependencyKind::Development => "dev",
                    DependencyKind::Build => "build",
                    _ => continue,
                };
                out.insert((from.clone(), to.clone(), kind));
            }
        }
    }
    out
}

/// Line of the dependency `dep` in `manifest`, trying each spelling of its
/// key (the renamed key, `-` and `_` forms) and then `package = "dep"`.
fn dep_line(manifest: &str, dep: &str, rename: Option<&str>) -> Option<usize> {
    let mut keys: Vec<String> = Vec::new();
    for k in rename.into_iter().chain([dep]) {
        for v in [k.to_string(), k.replace('-', "_"), k.replace('_', "-")] {
            if !keys.contains(&v) {
                keys.push(v);
            }
        }
    }
    keys.iter()
        .find_map(|k| manifest_line(manifest, k))
        .or_else(|| {
            let text = std::fs::read_to_string(manifest).ok()?;
            text.lines()
                .position(|l| l.contains(&format!("package = \"{dep}\"")))
                .map(|i| i + 1)
        })
}

pub fn check(meta: &Metadata, cfg: &Config) -> Vec<CrateViolation> {
    let active = activated(meta);
    let members: HashSet<_> = meta.workspace_members.iter().collect();
    let workspace: BTreeMap<String, String> = meta
        .packages
        .iter()
        .filter(|p| members.contains(&p.id))
        .map(|p| (p.name.to_string(), p.manifest_path.to_string()))
        .collect();
    let mut edges = Vec::new();
    let mut renames: std::collections::HashMap<(String, String), String> = Default::default();
    for pkg in meta.packages.iter().filter(|p| members.contains(&p.id)) {
        for dep in &pkg.dependencies {
            if let Some(r) = &dep.rename {
                renames.insert((pkg.name.to_string(), dep.name.clone()), r.clone());
            }
            let kind = match dep.kind {
                DependencyKind::Normal => "normal",
                DependencyKind::Development => "dev",
                DependencyKind::Build => "build",
                _ => continue,
            };
            edges.push(Edge {
                from: pkg.name.to_string(),
                to: dep.name.clone(),
                kind,
                manifest: pkg.manifest_path.to_string(),
                optional: dep.optional
                    && !active.contains(&(pkg.name.to_string(), dep.name.clone(), kind)),
            });
        }
    }
    evaluate(cfg, &workspace, &edges)
        .into_iter()
        .map(|(e, reason)| CrateViolation {
            line: dep_line(
                &e.manifest,
                &e.to,
                renames
                    .get(&(e.from.clone(), e.to.clone()))
                    .map(String::as_str),
            ),
            from: e.from,
            to: e.to,
            kind: e.kind,
            manifest: e.manifest,
            reason,
        })
        .collect()
}
