//! Reading `cargo metadata`. The rules read the declared dependencies of a
//! workspace's members and of the local packages those reach by path, all of
//! which `cargo metadata --no-deps` lists without resolving the registry graph
//! (0.4 s against 9 s for a workspace of a few hundred crates). Only
//! `check_optional = false` needs a full read: it asks the resolve graph which
//! optional dependencies features switch on.

use cargo_metadata::camino::Utf8PathBuf;
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `cargo metadata` of `manifest`'s workspace (or the current directory's),
/// resolved.
pub fn full(manifest: Option<&Path>) -> Result<Metadata, String> {
    exec(manifest, false)
}

/// The members only, unresolved; enough to locate a workspace.
pub fn members(manifest: Option<&Path>) -> Result<Metadata, String> {
    exec(manifest, true)
}

fn exec(manifest: Option<&Path>, no_deps: bool) -> Result<Metadata, String> {
    let mut cmd = MetadataCommand::new();
    if let Some(m) = manifest {
        cmd.manifest_path(m);
    }
    if no_deps {
        cmd.no_deps();
    }
    cmd.exec()
        .map_err(|e| format!("cargo metadata failed: {e}"))
}

/// Unresolved reads of several workspaces in one run, sharing the packages
/// each `--no-deps` call lists, so a workspace that others reach by path is
/// read once.
#[derive(Default)]
pub struct Reader {
    pool: HashMap<Utf8PathBuf, Package>,
}

impl Reader {
    /// The packages a full read of `manifest`'s workspace lists as local: its
    /// members and every package their declared path dependencies reach,
    /// following only the normal and build dependencies of a package that is
    /// not a member, as cargo resolves no other package's dev-dependencies.
    /// No resolve graph. An optional path dependency counts whether or not a
    /// feature switches it on, as `check_optional` reads them.
    ///
    /// A path package whose own workspace cannot be read alone (one inside a
    /// workspace's directory that is not its member, which cargo reads only
    /// as another workspace's dependency) makes this the full read.
    pub fn declared(&mut self, manifest: Option<&Path>) -> Result<Metadata, String> {
        let mut meta = members(manifest)?;
        let mut have: HashSet<Utf8PathBuf> = meta
            .packages
            .iter()
            .map(|p| p.manifest_path.clone())
            .collect();
        let mut todo: Vec<Utf8PathBuf> = meta
            .packages
            .iter()
            .flat_map(|p| path_deps(p, true))
            .collect();
        while let Some(m) = todo.pop() {
            if !have.insert(m.clone()) {
                continue;
            }
            let Some(p) = self.package(&m) else {
                return full(manifest);
            };
            todo.extend(path_deps(&p, false));
            meta.packages.push(p);
        }
        Ok(meta)
    }

    /// The package at manifest `m`, from an unresolved read of its workspace;
    /// None when that read fails or does not list it.
    fn package(&mut self, m: &Utf8PathBuf) -> Option<Package> {
        if !self.pool.contains_key(m) {
            for p in members(Some(m.as_std_path())).ok()?.packages {
                self.pool.entry(p.manifest_path.clone()).or_insert(p);
            }
        }
        self.pool.get(m).cloned()
    }
}

fn path_deps(p: &Package, member: bool) -> impl Iterator<Item = Utf8PathBuf> + '_ {
    p.dependencies
        .iter()
        .filter(move |d| member || d.kind != DependencyKind::Development)
        .filter_map(|d| d.path.as_ref().map(|dir| dir.join("Cargo.toml")))
}
