//! Membership in a layer or group: by crate name, or by the directory a
//! crate lives in, with the instance a `{name}` segment of `dir` captures.

use crate::config::{Layer, glob_match};
use crate::dirs::{Captures, DirPattern, show_captures};

/// The entry a crate belongs to on one axis, and its instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Membership {
    pub index: usize,
    pub captures: Captures,
}

/// The captures if entry `l` holds crate `name`, living in `dir` (relative
/// to the root; `None` when unknown or outside it). With `crates`, the name
/// decides and `dir` only names the instance; without, the directory decides.
pub fn holds(l: &Layer, name: &str, dir: Option<&str>) -> Option<Captures> {
    let instance = || {
        let pat = DirPattern::new(l.dir.as_deref()?).ok()?;
        pat.contains(dir?)
    };
    match &l.crates {
        Some(globs) => globs
            .iter()
            .any(|g| glob_match(g, name))
            .then(|| instance().unwrap_or_default()),
        None => instance(),
    }
}

/// The first entry of `axis` that holds the crate.
pub fn member_of(axis: &[Layer], name: &str, dir: Option<&str>) -> Option<Membership> {
    axis.iter()
        .enumerate()
        .find_map(|(index, l)| holds(l, name, dir).map(|captures| Membership { index, captures }))
}

/// True when the two instances agree on every capture name both have.
pub fn agree(a: &Captures, b: &Captures) -> bool {
    a.iter().all(|(k, v)| b.get(k).is_none_or(|w| w == v))
}

/// `` `domain` `` or `` `domain` (domain = sregym) `` for messages.
pub fn label(axis: &[Layer], m: &Membership) -> String {
    let name = &axis[m.index].name;
    if m.captures.is_empty() {
        format!("`{name}`")
    } else {
        format!("`{name}` ({})", show_captures(&m.captures))
    }
}
