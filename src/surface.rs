//! Public surface of a crate: paths from other workspace crates into it may
//! name items only through the modules it declares public.
//!
//! The rule sits on the target crate (`public_modules`). For each path that
//! starts with the target's name in a dependent crate's source (`use` leaves,
//! and expression, type and pattern paths), the module is the longest prefix
//! of the rest of the path that names a module of the target. The path is
//! allowed if that module is the crate root or is listed exactly (submodules
//! of a listed module are not implied). Items after the module are not
//! checked.

use crate::modules::ModuleUnit;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceViolation {
    pub file: PathBuf,
    pub line: usize,
    /// The path as written.
    pub reference: String,
    /// The module of the target crate it reaches.
    pub module: String,
}

fn split(p: &str) -> Vec<String> {
    p.split("::")
        .filter(|s| !s.is_empty() && *s != "crate")
        .map(str::to_string)
        .collect()
}

/// Violations among the files of one dependent crate. `ident` is the target's
/// name as written in source (`-` replaced by `_`), `known` its modules.
pub fn evaluate(
    ident: &str,
    public: &[String],
    known: &HashSet<Vec<String>>,
    files: &[ModuleUnit],
    ws_root: &Path,
) -> Vec<SurfaceViolation> {
    let public: Vec<Vec<String>> = public.iter().map(|p| split(p)).collect();
    let mut out = Vec::new();
    for f in files {
        for r in f.scan.refs.iter().chain(&f.scan.bare) {
            if r.segs.len() < 2 || r.segs[0] != ident {
                continue;
            }
            let rest = &r.segs[1..];
            let n = (0..=rest.len())
                .rev()
                .find(|n| known.contains(&rest[..*n]))
                .unwrap_or(0);
            if n == 0 || public.iter().any(|p| p[..] == rest[..n]) {
                continue;
            }
            out.push(SurfaceViolation {
                file: f
                    .file
                    .strip_prefix(ws_root)
                    .unwrap_or(&f.file)
                    .to_path_buf(),
                line: r.line,
                reference: r.segs.join("::"),
                module: rest[..n].join("::"),
            });
        }
    }
    out.sort_by(|a, b| (&a.file, a.line, &a.reference).cmp(&(&b.file, b.line, &b.reference)));
    out.dedup();
    out
}
