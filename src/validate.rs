//! Configuration checks that need the workspace: names that match nothing,
//! overlapping layers or groups, impossible `may_depend_on`. Every message
//! starts with `file:line:`.

use crate::config::{Config, Layer, glob_match};
use std::collections::BTreeSet;

/// Where each table and key sits in the config text (serde drops spans).
pub struct Lines<'a> {
    lines: Vec<&'a str>,
    /// (header, header line index, parent `[[crate]]` header line for modules)
    tables: Vec<(String, usize, Option<usize>)>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Self {
        let lines: Vec<&str> = text.lines().collect();
        let mut tables = Vec::new();
        let mut cur_crate = None;
        for (i, l) in lines.iter().enumerate() {
            let t = l.trim();
            if let Some(h) = t.strip_prefix("[[").and_then(|x| x.split("]]").next()) {
                let h = h.trim();
                if h == "crate" {
                    cur_crate = Some(i);
                }
                let parent = (h == "crate.module").then_some(cur_crate).flatten();
                tables.push((h.to_string(), i, parent));
            }
        }
        Lines { lines, tables }
    }

    fn end(&self, tbl: usize) -> usize {
        let start = self.tables[tbl].1;
        // The next table header of any kind ends this one.
        (start + 1..self.lines.len())
            .find(|&i| self.lines[i].trim_start().starts_with('['))
            .unwrap_or(self.lines.len())
    }

    /// 1-based line of `key = ...` in table `tbl`, else the header line.
    fn key_line(&self, tbl: usize, key: &str) -> usize {
        let (start, end) = (self.tables[tbl].1, self.end(tbl));
        (start + 1..end)
            .find(|&i| {
                let t = self.lines[i].trim_start();
                t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
            })
            .unwrap_or(start)
            + 1
    }

    /// Line of the quoted `item` in the array `key` of `tbl` (else the key line).
    pub fn item_line(&self, tbl: usize, key: &str, item: &str) -> usize {
        let k = self.key_line(tbl, key);
        let end = self.end(tbl);
        let needle = format!("\"{item}\"");
        (k - 1..end)
            .find(|&i| self.lines[i].contains(&needle))
            .map(|i| i + 1)
            .unwrap_or(k)
    }

    /// Table of `header` whose string key `key` equals `val`.
    pub fn find(&self, header: &str, key: &str, val: &str) -> Option<usize> {
        let needle = format!("\"{val}\"");
        (0..self.tables.len()).find(|&t| {
            self.tables[t].0 == header && {
                let l = self.key_line(t, key);
                l > self.tables[t].1 + 1 && self.lines[l - 1].contains(&needle)
            }
        })
    }

    /// Module rule `path` inside the `[[crate]]` whose name is `krate`.
    pub fn find_module(&self, krate: &str, path: &str) -> Option<usize> {
        let needle = format!("\"{path}\"");
        (0..self.tables.len()).find(|&t| {
            self.tables[t].0 == "crate.module"
                && self.tables[t].2.is_some_and(|p| {
                    self.tables.iter().position(|x| x.1 == p).is_some_and(|ci| {
                        let l = self.key_line(ci, "name");
                        self.lines[l - 1].contains(&format!("\"{krate}\""))
                    })
                })
                && self.lines[self.key_line(t, "path") - 1].contains(&needle)
        })
    }
}

fn is_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

#[allow(clippy::too_many_arguments)]
fn axis(
    out: &mut Vec<String>,
    warn: &mut Vec<String>,
    unknown_ok: bool,
    f: &str,
    lines: &Lines,
    header: &str,
    what: &str,
    layers: &[Layer],
    ws: &BTreeSet<String>,
) {
    let tbl_of = |l: &Layer| lines.find(header, "name", &l.name).unwrap_or(0);
    let at = |t: usize, key: &str, item: &str| lines.item_line(t, key, item);
    // Duplicate names.
    for (i, l) in layers.iter().enumerate() {
        if layers[..i].iter().any(|p| p.name == l.name) {
            let t = (0..lines.tables.len())
                .filter(|&t| lines.tables[t].0 == header)
                .nth(i)
                .unwrap_or(0);
            out.push(format!(
                "{f}:{}: {what} `{}` is defined twice",
                lines.key_line(t, "name"),
                l.name
            ));
        }
    }
    // Position-based table lookup (names may repeat).
    let tables: Vec<usize> = (0..lines.tables.len())
        .filter(|&t| lines.tables[t].0 == header)
        .collect();
    let tbl = |i: usize| tables.get(i).copied().unwrap_or_else(|| tbl_of(&layers[i]));
    for (i, l) in layers.iter().enumerate() {
        for g in l.crates.iter().filter(|_| !unknown_ok) {
            if !ws.iter().any(|n| glob_match(g, n)) {
                out.push(format!(
                    "{f}:{}: {what} `{}`: `{g}` matches no workspace crate",
                    at(tbl(i), "crates", g),
                    l.name
                ));
            }
        }
        for dep in l.may_depend_on.iter().flatten() {
            if !layers.iter().any(|o| &o.name == dep) {
                out.push(format!(
                    "{f}:{}: {what} `{}`: may_depend_on names unknown {what} `{dep}`",
                    at(tbl(i), "may_depend_on", dep),
                    l.name
                ));
            }
        }
    }
    // A crate in two layers/groups: the first would silently win.
    for n in ws {
        let hits: Vec<usize> = (0..layers.len())
            .filter(|&i| layers[i].crates.iter().any(|g| glob_match(g, n)))
            .collect();
        if hits.len() > 1 {
            let (a, b) = (hits[0], hits[1]);
            let g = layers[b].crates.iter().find(|g| glob_match(g, n)).unwrap();
            out.push(format!(
                "{f}:{}: crate `{n}` is in {what}s `{}` (line {}) and `{}`: overlapping {what}s; \
                 each crate may be in one",
                at(tbl(b), "crates", g),
                layers[a].name,
                lines.key_line(tbl(a), "crates"),
                layers[b].name
            ));
        }
    }
    // Cycle through may_depend_on (self edges are fine).
    let idx = |name: &str| layers.iter().position(|l| l.name == name);
    let edges: Vec<Vec<usize>> = layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            l.may_depend_on
                .iter()
                .flatten()
                .filter_map(|d| idx(d))
                .filter(|&j| j != i)
                .collect()
        })
        .collect();
    let mut state = vec![0u8; layers.len()];
    let mut stack: Vec<usize> = Vec::new();
    fn dfs(
        u: usize,
        edges: &[Vec<usize>],
        state: &mut [u8],
        stack: &mut Vec<usize>,
    ) -> Option<Vec<usize>> {
        state[u] = 1;
        stack.push(u);
        for &v in &edges[u] {
            if state[v] == 1 {
                let p = stack.iter().position(|&x| x == v).unwrap();
                return Some(stack[p..].to_vec());
            }
            if state[v] == 0
                && let Some(c) = dfs(v, edges, state, stack)
            {
                return Some(c);
            }
        }
        stack.pop();
        state[u] = 2;
        None
    }
    for u in 0..layers.len() {
        if state[u] == 0
            && let Some(cycle) = dfs(u, &edges, &mut state, &mut stack)
        {
            let names: Vec<&str> = cycle.iter().map(|&i| layers[i].name.as_str()).collect();
            let next = layers[cycle[(1) % cycle.len()]].name.as_str();
            // A permission matrix may allow both directions (crates still
            // cannot cycle: cargo rejects that), so this is a warning.
            warn.push(format!(
                "{f}:{}: {what}s permit each other in a cycle: {} -> {} (allowed, but check it is intended)",
                at(tbl(cycle[0]), "may_depend_on", next),
                names.join(" -> "),
                names[0]
            ));
            break;
        }
    }
}

/// All configuration errors for `cfg` (parsed from `text`) against the
/// workspace crate names `ws`. `f` is the config file name for messages.
pub fn validate(
    cfg: &Config,
    text: &str,
    ws: &BTreeSet<String>,
    f: &str,
) -> (Vec<String>, Vec<String>) {
    let lines = Lines::new(text);
    let mut out = Vec::new();
    let mut warn = Vec::new();
    let ok = cfg.allow_unknown_names;
    axis(
        &mut out,
        &mut warn,
        ok,
        f,
        &lines,
        "layer",
        "layer",
        &cfg.layers,
        ws,
    );
    axis(
        &mut out,
        &mut warn,
        ok,
        f,
        &lines,
        "group",
        "group",
        &cfg.groups,
        ws,
    );
    let crate_tables: Vec<usize> = (0..lines.tables.len())
        .filter(|&t| lines.tables[t].0 == "crate")
        .collect();
    for (i, r) in cfg.crates.iter().enumerate() {
        let t = crate_tables.get(i).copied().unwrap_or(0);
        if !ok && !ws.iter().any(|n| glob_match(&r.name, n)) {
            out.push(format!(
                "{f}:{}: `[[crate]] name = \"{}\"` matches no workspace crate",
                lines.key_line(t, "name"),
                r.name
            ));
        }
        for (key, list) in [
            ("allow", r.allow.as_deref().unwrap_or(&[])),
            ("allow_dev", &r.allow_dev[..]),
        ] {
            for d in list {
                if !ok && !is_glob(d) && !ws.contains(d) {
                    out.push(format!(
                        "{f}:{}: crate `{}`: `{key}` names `{d}`, which is not a workspace crate",
                        lines.item_line(t, key, d),
                        r.name
                    ));
                }
            }
        }
    }
    (out, warn)
}
