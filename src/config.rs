use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Check dev-dependency edges too (default false).
    #[serde(default)]
    pub check_dev: bool,
    /// Check build-dependency edges (default true).
    #[serde(default = "yes")]
    pub check_build: bool,
    /// Check optional (feature-gated) dependencies too (default true).
    /// `cargo metadata`'s resolve graph omits optional deps whose feature is
    /// off; this reads the declared dependencies, which include them.
    #[serde(default = "yes")]
    pub check_optional: bool,
    /// Do not reject names in the config that match no workspace crate or
    /// module (default false). For one config shared by checkouts whose
    /// member sets differ.
    #[serde(default)]
    pub allow_unknown_names: bool,
    /// Skip `#[cfg(test)]` modules, fns and uses in module checks (default true).
    #[serde(default = "yes")]
    pub skip_cfg_test: bool,
    /// Resolve names inside a crate (re-exports, globs, renames, `Self`,
    /// type aliases) before applying module rules and the authority and
    /// re-export lints (default true). `false` keeps the path-as-written
    /// behavior.
    #[serde(default = "yes")]
    pub resolve: bool,
    /// Apply the layer axis to dev edges too (default false).
    #[serde(default)]
    pub layer_dev: bool,
    /// Apply the group axis to dev edges too (default true).
    #[serde(default = "yes")]
    pub group_dev: bool,
    /// Every workspace crate (`true`), or every one matching these globs,
    /// must belong to a layer.
    #[serde(default)]
    pub require_layer: Require,
    /// The same for groups.
    #[serde(default)]
    pub require_group: Require,
    /// Layers, ordered lowest first unless `may_depend_on` is given.
    #[serde(default, rename = "layer")]
    pub layers: Vec<Layer>,
    /// A second, independent axis with the same shape as layers.
    #[serde(default, rename = "group")]
    pub groups: Vec<Layer>,
    #[serde(default, rename = "crate")]
    pub crates: Vec<CrateRule>,
    /// JSON file (relative to the workspace root) mapping crate name to its
    /// allowed number of unsafe sites, for crates with `unsafe = "ratchet"`.
    pub unsafe_ratchet: Option<String>,
    /// Source lints (authority, re-export, unsafe, ...) skip `#[cfg(test)]`
    /// items (default false: they see test code).
    #[serde(default)]
    pub lint_skip_cfg_test: bool,
    /// Files (globs relative to the workspace root) the source lints skip,
    /// for example fixtures that are not valid Rust.
    #[serde(default)]
    pub lint_exclude: Vec<String>,
    /// Types (or functions) that only listed files may construct.
    #[serde(default, rename = "authority")]
    pub authority: Vec<Authority>,
    /// Token patterns allowed only in listed files.
    #[serde(default, rename = "forbid")]
    pub forbids: Vec<Forbid>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Forbid {
    /// Label used in messages.
    pub name: String,
    /// Token patterns: space-separated words matched against consecutive
    /// tokens at one nesting level. A word is an identifier, a `a::b` path,
    /// `name(` (a call: the identifier followed by a parenthesized group, not
    /// preceded by `.` or `:`) or `name()` (call with no arguments).
    pub patterns: Vec<String>,
    /// Files (globs relative to the workspace root) where the patterns are allowed.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Crates (name globs) checked; default all.
    #[serde(default = "star")]
    pub crates: Vec<String>,
    /// Directories of each crate to scan (default `src`).
    #[serde(default = "src_dir")]
    pub dirs: Vec<String>,
    /// In the allowed files, every hit needs a line comment containing this
    /// tag on its line or in the comment/attribute block directly above it.
    pub require_comment: Option<String>,
    /// Hits in a file outside `allow` that are tolerated (a grandfathered
    /// use). An entry that matches no hit is reported as stale.
    #[serde(default)]
    pub exempt: Vec<ForbidExempt>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct ForbidExempt {
    pub file: String,
    pub pattern: String,
}

fn src_dir() -> Vec<String> {
    vec!["src".into()]
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    /// Label used in messages.
    pub name: String,
    /// The type whose construction is restricted.
    #[serde(rename = "type")]
    pub ty: Option<String>,
    /// Restrict `Type { .. }` struct literals (default true when `type` is set).
    #[serde(default = "yes")]
    pub literal: bool,
    /// Associated functions of the type whose call counts as construction
    /// (`Type::new(..)`).
    #[serde(default)]
    pub assoc: Vec<String>,
    /// Free functions (matched by last path segment, not methods) whose call
    /// counts as construction.
    #[serde(default)]
    pub calls: Vec<String>,
    /// Files (globs relative to the workspace root) allowed to construct it.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Crates (name globs) whose files are checked; default all.
    #[serde(default = "star")]
    pub crates: Vec<String>,
    /// Directories of each crate to scan (default the whole crate dir).
    #[serde(default = "dot")]
    pub dirs: Vec<String>,
    /// A construct on a line containing this text, or inside a fn whose
    /// text contains it, is exempt (a negative control).
    pub exempt_marker: Option<String>,
}

fn star() -> Vec<String> {
    vec!["*".into()]
}
fn dot() -> Vec<String> {
    vec![".".into()]
}

/// Which workspace crates must belong to a layer (or group): none, all
/// (`true`), or those matching a list of crate-name globs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "RequireRepr")]
pub enum Require {
    #[default]
    None,
    All,
    Crates(Vec<String>),
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RequireRepr {
    Flag(bool),
    Crates(Vec<String>),
}

impl From<RequireRepr> for Require {
    fn from(r: RequireRepr) -> Self {
        match r {
            RequireRepr::Flag(false) => Require::None,
            RequireRepr::Flag(true) => Require::All,
            RequireRepr::Crates(g) => Require::Crates(g),
        }
    }
}

impl From<bool> for Require {
    fn from(b: bool) -> Self {
        RequireRepr::Flag(b).into()
    }
}

impl Require {
    /// Whether crate `name` must belong to the axis.
    pub fn covers(&self, name: &str) -> bool {
        match self {
            Require::None => false,
            Require::All => true,
            Require::Crates(g) => g.iter().any(|p| glob_match(p, name)),
        }
    }
}

/// `fn main` may call only what is listed here.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct MainRule {
    /// Path calls (`a::b::f`, segments joined by `::`) and bare calls (`Some`)
    /// main may make besides the entries.
    #[serde(default)]
    pub calls: Vec<String>,
    /// Method names main may call.
    #[serde(default)]
    pub methods: Vec<String>,
    /// Entry points: main must call at least one of them in total, and each
    /// at most once.
    #[serde(default)]
    pub entries: Vec<String>,
    /// If present, the binary's root file may hold only `fn main`, inner
    /// attributes and top-level items whose `kind name` (`mod neg`,
    /// `use`, `fn helper`) matches one of these globs.
    #[serde(default)]
    pub items: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum UnsafePolicy {
    /// Count unsafe sites in `src/`; the count must equal the ratchet entry.
    Ratchet,
    /// Every lib/bin target root must carry `#![forbid(unsafe_code)]`.
    Forbid,
}

fn yes() -> bool {
    true
}

/// A layer (in `[[layer]]`) or a group (in `[[group]]`): a named set of
/// crates. Without `may_depend_on`, layers are ordered, lowest first, and a
/// crate may depend on its own and lower layers. With `may_depend_on`, that
/// list is exact (own layer is not implicit) and ordering is not consulted.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct Layer {
    pub name: String,
    /// Crate-name globs belonging to this layer.
    pub crates: Vec<String>,
    pub may_depend_on: Option<Vec<String>>,
    /// Members may not depend on crates matching these globs, on any edge
    /// kind the axis applies to. `layer_exempt`/`group_exempt` suppress it.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Like `deny`, for normal edges only.
    #[serde(default)]
    pub deny_normal: Vec<String>,
    /// If present, normal dependencies on crates outside the workspace must
    /// match one of these globs (`[]` forbids all external normal deps).
    pub external_allow: Option<Vec<String>>,
    /// Normal dependencies on crates outside the workspace matching these
    /// globs are violations.
    #[serde(default)]
    pub external_deny: Vec<String>,
    /// Directory (relative to the workspace root, `/`-separated) every member
    /// crate must live in or under. Checked for each crate in this layer/group.
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct CrateRule {
    /// Crate-name glob this rule applies to.
    pub name: String,
    /// If present, the exact set (globs) of workspace crates this crate may
    /// depend on through normal and build edges. Unlisted workspace
    /// dependencies are violations.
    pub allow: Option<Vec<String>>,
    /// Extra workspace crates allowed through dev edges only.
    #[serde(default)]
    pub allow_dev: Vec<String>,
    /// Dependencies matching these globs are always violations (any crate,
    /// workspace or not, any edge kind).
    #[serde(default)]
    pub deny: Vec<String>,
    /// Like `deny`, for normal edges only.
    #[serde(default)]
    pub deny_normal: Vec<String>,
    /// No crate matching these globs may be reachable through normal edges
    /// among workspace crates.
    #[serde(default)]
    pub deny_reach: Vec<String>,
    /// Dependencies exempt from the layer axis (a grandfathered edge). An
    /// exemption that suppresses no violation is itself reported as stale.
    #[serde(default)]
    pub layer_exempt: Vec<String>,
    /// Same, for the group axis.
    #[serde(default)]
    pub group_exempt: Vec<String>,
    /// Parse `verus! { }` bodies for module rules (needs the `verus` feature).
    #[serde(default)]
    pub verus: bool,
    /// Directory globs (relative to the workspace root) that must not hold
    /// any package reachable through dependencies of this crate.
    #[serde(default)]
    pub deny_reach_dir: Vec<String>,
    /// Directory globs (relative to the workspace root) that source files of
    /// this crate may not pull in through `#[path]` or `include!`.
    #[serde(default)]
    pub deny_source_dir: Vec<String>,
    /// Crates (as written in source, `-` and `_` equal) whose items this
    /// crate may not `pub use`, except the names in `allow_reexport`.
    #[serde(default)]
    pub deny_reexport: Vec<String>,
    /// Leaf names allowed through `deny_reexport`; `"*"` lifts the rule.
    #[serde(default)]
    pub allow_reexport: Vec<String>,
    /// Unsafe-code policy for this crate.
    #[serde(rename = "unsafe")]
    pub unsafe_policy: Option<UnsafePolicy>,
    /// Restrictions on the body of `fn main` in this crate's binaries.
    pub main: Option<MainRule>,
    /// Every item in `src/` must be a `use`, `mod`, `extern crate` or sit
    /// inside `verus! { }`.
    #[serde(default)]
    pub verus_only: bool,
    /// Modules of this crate (paths from the crate root) through which other
    /// workspace crates may name its items. A path from a dependent crate
    /// whose module is neither the crate root nor listed here is a violation
    /// (`error[surface]`). Absent: no rule.
    pub public_modules: Option<Vec<String>>,
    /// Crate-name globs of the dependent crates `public_modules` applies to.
    /// Absent: every workspace crate that depends on this one.
    pub public_modules_for: Option<Vec<String>>,
    #[serde(default, rename = "module")]
    pub modules: Vec<ModuleRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ModuleRule {
    /// Module path relative to the crate root, e.g. `memory` or `a::b`.
    pub path: String,
    /// If present, the module may reference only itself and these modules
    /// (prefix match on module paths). Absent: no allowlist.
    pub depends_on: Option<Vec<String>>,
    /// Modules this module must never reference.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Modules this module may reference although `deny` or `depends_on`
    /// would reject them (a grandfathered edge). An entry that suppresses no
    /// violation is reported as stale.
    #[serde(default)]
    pub exempt: Vec<String>,
}

pub fn glob_match(pat: &str, name: &str) -> bool {
    glob::Pattern::new(pat)
        .map(|p| p.matches(name))
        .unwrap_or(false)
}
