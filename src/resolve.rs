//! Crate-local name resolution: which module defines the thing a path names.
//!
//! A [`Table`] holds, per module of one crate, the items it defines, its
//! `use` bindings (renames included) and its glob imports. [`Table::resolve`]
//! follows a path through those bindings, re-exports and globs to the module
//! that defines the target. Everything is syntactic: no traits, no macros, no
//! visibility (every binding is treated as importable, except that glob
//! re-export enumeration looks at `pub` uses only), no namespaces (a name with
//! several bindings uses the first that resolves inside the crate). Paths that
//! leave the crate (extern crates, `std`, unknown names) come back as
//! [`Res::External`], with leading local aliases substituted.
//!
//! Resolution is bounded: each query has a fixed step budget, so glob cycles
//! and self-referential `use` chains end as `External` instead of looping.

use crate::scan::ModDecl;
use std::collections::{BTreeSet, HashMap, HashSet};

/// Steps one query may take (each `use` hop or glob hop costs one).
pub const FUEL: u32 = 2048;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefKind {
    /// A struct, enum, fn, trait, const, static or union.
    Other,
    /// `type Name = a::b::C;` with a plain path on the right.
    Alias(Vec<String>),
}

#[derive(Debug, Clone)]
pub struct Def {
    /// Inline modules enclosing the item (relative to the file's module).
    pub inline: Vec<String>,
    pub name: String,
    pub kind: DefKind,
}

#[derive(Debug, Clone)]
pub struct Bind {
    pub inline: Vec<String>,
    /// The imported path as written (`crate::a::B`, `self::x`, `denied::Y`).
    pub path: Vec<String>,
    /// The name the use introduces (`None` for globs and `as _`).
    pub name: Option<String>,
    pub glob: bool,
    /// Any `pub` visibility.
    pub public: bool,
    pub line: usize,
}

/// Definitions and imports of one file, per inline module.
#[derive(Debug, Clone, Default)]
pub struct Items {
    pub defs: Vec<Def>,
    pub binds: Vec<Bind>,
    pub decls: Vec<ModDecl>,
    pub inline_mods: Vec<Vec<String>>,
}

impl Items {
    /// Re-root everything inside `inline` (the items of a `verus! { }` body
    /// that sits in the inline module `inline`).
    pub fn extend_under(&mut self, inline: &[String], inner: Items) {
        let pre = |v: &[String]| [inline, v].concat();
        for mut d in inner.defs {
            d.inline = pre(&d.inline);
            self.defs.push(d);
        }
        for mut b in inner.binds {
            b.inline = pre(&b.inline);
            self.binds.push(b);
        }
        for mut d in inner.decls {
            d.inline = pre(&d.inline);
            self.decls.push(d);
        }
        for m in inner.inline_mods {
            self.inline_mods.push(pre(&m));
        }
    }
}

macro_rules! items_collector {
    ($m:ident, $syn:ident) => {
        pub mod $m {
            use super::*;
            use $syn::visit::Visit;

            struct V<'a> {
                out: &'a mut Items,
                inline: Vec<String>,
                fn_depth: usize,
                skip_cfg_test: bool,
            }

            fn is_cfg_test(attrs: &[$syn::Attribute]) -> bool {
                attrs.iter().any(|a| {
                    a.path().is_ident("cfg")
                        && match &a.meta {
                            $syn::Meta::List(l) => l.tokens.to_string().trim() == "test",
                            _ => false,
                        }
                })
            }

            fn path_attr(attrs: &[$syn::Attribute]) -> Option<String> {
                for a in attrs {
                    if a.path().is_ident("path") {
                        if let $syn::Meta::NameValue(nv) = &a.meta {
                            if let $syn::Expr::Lit($syn::ExprLit {
                                lit: $syn::Lit::Str(s),
                                ..
                            }) = &nv.value
                            {
                                return Some(s.value());
                            }
                        }
                    }
                }
                None
            }

            fn item_attrs(i: &$syn::Item) -> &[$syn::Attribute] {
                use $syn::Item as I;
                match i {
                    I::Const(x) => &x.attrs,
                    I::Enum(x) => &x.attrs,
                    I::ExternCrate(x) => &x.attrs,
                    I::Fn(x) => &x.attrs,
                    I::ForeignMod(x) => &x.attrs,
                    I::Impl(x) => &x.attrs,
                    I::Macro(x) => &x.attrs,
                    I::Mod(x) => &x.attrs,
                    I::Static(x) => &x.attrs,
                    I::Struct(x) => &x.attrs,
                    I::Trait(x) => &x.attrs,
                    I::Type(x) => &x.attrs,
                    I::Union(x) => &x.attrs,
                    I::Use(x) => &x.attrs,
                    _ => &[],
                }
            }

            fn flatten(
                t: &$syn::UseTree,
                prefix: &mut Vec<String>,
                public: bool,
                line: usize,
                v: &mut V,
            ) {
                match t {
                    $syn::UseTree::Path(p) => {
                        prefix.push(p.ident.to_string());
                        flatten(&p.tree, prefix, public, line, v);
                        prefix.pop();
                    }
                    $syn::UseTree::Name(n) => {
                        let mut path = prefix.clone();
                        let name = n.ident.to_string();
                        if name == "self" {
                            if let Some(last) = prefix.last() {
                                v.bind(path, Some(last.clone()), false, public, line);
                            }
                        } else {
                            path.push(name.clone());
                            v.bind(path, Some(name), false, public, line);
                        }
                    }
                    $syn::UseTree::Rename(n) => {
                        let mut path = prefix.clone();
                        let orig = n.ident.to_string();
                        if orig != "self" {
                            path.push(orig);
                        }
                        let alias = n.rename.to_string();
                        let name = (alias != "_").then_some(alias);
                        v.bind(path, name, false, public, line);
                    }
                    $syn::UseTree::Glob(_) => v.bind(prefix.clone(), None, true, public, line),
                    $syn::UseTree::Group(g) => {
                        for it in &g.items {
                            flatten(it, prefix, public, line, v);
                        }
                    }
                }
            }

            fn simple_path(t: &$syn::Type) -> Option<Vec<String>> {
                match t {
                    $syn::Type::Path(p) if p.qself.is_none() => Some(
                        p.path
                            .segments
                            .iter()
                            .map(|s| s.ident.to_string())
                            .collect(),
                    ),
                    _ => None,
                }
            }

            impl V<'_> {
                fn bind(
                    &mut self,
                    path: Vec<String>,
                    name: Option<String>,
                    glob: bool,
                    public: bool,
                    line: usize,
                ) {
                    if path.is_empty() {
                        return;
                    }
                    self.out.binds.push(Bind {
                        inline: self.inline.clone(),
                        path,
                        name,
                        glob,
                        public,
                        line,
                    });
                }

                fn def(&mut self, name: String, kind: DefKind) {
                    if self.fn_depth == 0 {
                        self.out.defs.push(Def {
                            inline: self.inline.clone(),
                            name,
                            kind,
                        });
                    }
                }
            }

            impl<'ast> Visit<'ast> for V<'_> {
                fn visit_item(&mut self, i: &'ast $syn::Item) {
                    use $syn::Item as I;
                    if self.skip_cfg_test && is_cfg_test(item_attrs(i)) {
                        return;
                    }
                    match i {
                        I::Struct(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Enum(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Union(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Trait(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Fn(x) => self.def(x.sig.ident.to_string(), DefKind::Other),
                        I::Const(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Static(x) => self.def(x.ident.to_string(), DefKind::Other),
                        I::Type(x) => {
                            let kind = simple_path(&x.ty).map_or(DefKind::Other, DefKind::Alias);
                            self.def(x.ident.to_string(), kind)
                        }
                        I::ExternCrate(x) => {
                            let name = x
                                .rename
                                .as_ref()
                                .map(|(_, r)| r.to_string())
                                .unwrap_or_else(|| x.ident.to_string());
                            if x.rename.is_some() && name != "_" {
                                let line = x.extern_token.span.start().line;
                                self.bind(
                                    vec![x.ident.to_string()],
                                    Some(name),
                                    false,
                                    false,
                                    line,
                                );
                            }
                        }
                        _ => {}
                    }
                    $syn::visit::visit_item(self, i);
                }

                fn visit_item_use(&mut self, i: &'ast $syn::ItemUse) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    let public = !matches!(i.vis, $syn::Visibility::Inherited);
                    let line = i.use_token.span.start().line;
                    flatten(&i.tree, &mut Vec::new(), public, line, self);
                }

                fn visit_item_fn(&mut self, i: &'ast $syn::ItemFn) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    self.fn_depth += 1;
                    $syn::visit::visit_item_fn(self, i);
                    self.fn_depth -= 1;
                }

                fn visit_impl_item_fn(&mut self, i: &'ast $syn::ImplItemFn) {
                    self.fn_depth += 1;
                    $syn::visit::visit_impl_item_fn(self, i);
                    self.fn_depth -= 1;
                }

                fn visit_trait_item_fn(&mut self, i: &'ast $syn::TraitItemFn) {
                    self.fn_depth += 1;
                    $syn::visit::visit_trait_item_fn(self, i);
                    self.fn_depth -= 1;
                }

                fn visit_item_mod(&mut self, i: &'ast $syn::ItemMod) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    let name = i.ident.to_string();
                    match &i.content {
                        Some(_) => {
                            self.inline.push(name);
                            self.out.inline_mods.push(self.inline.clone());
                            $syn::visit::visit_item_mod(self, i);
                            self.inline.pop();
                        }
                        None => self.out.decls.push(ModDecl {
                            name,
                            path_attr: path_attr(&i.attrs),
                            inline: self.inline.clone(),
                        }),
                    }
                }
            }

            /// Collect the items of `f` (and of nested inline modules) into `out`.
            pub fn visit_file(f: &$syn::File, out: &mut Items, skip_cfg_test: bool) {
                let mut v = V {
                    out,
                    inline: Vec::new(),
                    fn_depth: 0,
                    skip_cfg_test,
                };
                v.visit_file(f);
            }
        }
    };
}

items_collector!(plain, syn);
#[cfg(feature = "verus")]
items_collector!(verus, verus_syn);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Res {
    /// A module of this crate.
    Module(Vec<String>),
    /// An item (or a path below one) defined in `module`: `module ++ rest`.
    Item {
        module: Vec<String>,
        rest: Vec<String>,
    },
    /// Outside this crate, or a name nothing here binds.
    External(Vec<String>),
}

impl Res {
    /// The whole resolved path (module path included).
    pub fn path(&self) -> Vec<String> {
        match self {
            Res::Module(m) | Res::External(m) => m.clone(),
            Res::Item { module, rest } => [&module[..], &rest[..]].concat(),
        }
    }

    /// The module that defines the target, when it is in this crate.
    pub fn module(&self) -> Option<&[String]> {
        match self {
            Res::Module(m) => Some(m),
            Res::Item { module, .. } => Some(module),
            Res::External(_) => None,
        }
    }

    fn push_rest(self, rest: &[String], t: &Table, fuel: &mut Cx) -> Res {
        if rest.is_empty() {
            return self;
        }
        match self {
            Res::Module(m) => t.walk(&m, rest, fuel),
            Res::Item { module, rest: r } => Res::Item {
                module,
                rest: [&r[..], rest].concat(),
            },
            Res::External(p) => Res::External([&p[..], rest].concat()),
        }
    }
}

#[derive(Default, Debug)]
struct Scope {
    defs: HashMap<String, DefKind>,
    /// Name -> imported paths (several when the name is bound more than once).
    binds: HashMap<String, Vec<Vec<String>>>,
    /// Glob-imported paths, and whether each is `pub`.
    globs: Vec<(Vec<String>, bool)>,
    /// `pub` named bindings (the re-exports), name -> paths.
    pub_binds: Vec<(String, Vec<String>)>,
}

/// Per-query state: the step budget, and the bindings being followed (a name
/// that is only defined in terms of itself does not resolve).
struct Cx {
    fuel: u32,
    stack: Vec<(Vec<String>, String)>,
    /// A binding was skipped because it is being followed already.
    skipped: bool,
}

enum Found {
    Child(Vec<String>),
    Def(Vec<String>, String, DefKind),
    Bind(Vec<String>, String, Vec<Vec<String>>),
}

/// The names, aliases and imports of one crate.
#[derive(Default, Debug)]
pub struct Table {
    mods: HashSet<Vec<String>>,
    scopes: HashMap<Vec<String>, Scope>,
    /// Names that are bound under a different name (`as`), or are type aliases.
    renamed: HashSet<String>,
}

impl Table {
    /// `files`: each file's module path and items. `known`: the modules of the crate.
    pub fn build<'a>(
        known: &HashSet<Vec<String>>,
        files: impl IntoIterator<Item = (&'a [String], &'a Items)>,
    ) -> Table {
        let mut t = Table {
            mods: known.clone(),
            ..Table::default()
        };
        for (module, items) in files {
            for d in &items.defs {
                let sc = t
                    .scopes
                    .entry([module, &d.inline[..]].concat())
                    .or_default();
                if matches!(d.kind, DefKind::Alias(_)) {
                    t.renamed.insert(d.name.clone());
                }
                sc.defs.insert(d.name.clone(), d.kind.clone());
            }
            for b in &items.binds {
                let sc = t
                    .scopes
                    .entry([module, &b.inline[..]].concat())
                    .or_default();
                if b.glob {
                    sc.globs.push((b.path.clone(), b.public));
                } else if let Some(n) = &b.name {
                    if b.path.last() != Some(n) {
                        t.renamed.insert(n.clone());
                    }
                    sc.binds.entry(n.clone()).or_default().push(b.path.clone());
                    if b.public {
                        sc.pub_binds.push((n.clone(), b.path.clone()));
                    }
                }
            }
        }
        t
    }

    /// Is `name` defined or explicitly imported (not just glob-imported) in module `m`?
    pub fn is_explicit(&self, m: &[String], name: &str) -> bool {
        self.mods.contains(&[m, &[name.to_string()][..]].concat())
            || self
                .scopes
                .get(m)
                .is_some_and(|s| s.defs.contains_key(name) || s.binds.contains_key(name))
    }

    pub fn is_module(&self, m: &[String]) -> bool {
        self.mods.contains(m)
    }

    /// Is `name` introduced by an `as` rename or a `type` alias somewhere in the crate?
    pub fn is_renamed(&self, name: &str) -> bool {
        self.renamed.contains(name)
    }

    /// Resolve `segs` as written inside module `scope`.
    pub fn resolve(&self, scope: &[String], segs: &[String]) -> Res {
        let mut fuel = Cx {
            fuel: FUEL,
            stack: Vec::new(),
            skipped: false,
        };
        self.res(scope, segs, &mut fuel)
    }

    fn res(&self, scope: &[String], segs: &[String], fuel: &mut Cx) -> Res {
        if segs.is_empty() {
            return Res::Module(scope.to_vec());
        }
        if fuel.fuel == 0 {
            return Res::External(segs.to_vec());
        }
        fuel.fuel -= 1;
        match segs[0].as_str() {
            "crate" => self.walk(&[], &segs[1..], fuel),
            "self" => self.walk(scope, &segs[1..], fuel),
            "super" => {
                let n = segs.iter().take_while(|s| *s == "super").count();
                if n > scope.len() {
                    return Res::External(segs.to_vec());
                }
                self.walk(&scope[..scope.len() - n], &segs[n..], fuel)
            }
            first => {
                let cands = self.candidates(scope, first, fuel);
                if cands.is_empty() {
                    return Res::External(segs.to_vec());
                }
                self.follow_any(cands, &segs[1..], fuel)
            }
        }
    }

    fn walk(&self, m: &[String], rest: &[String], fuel: &mut Cx) -> Res {
        let Some(first) = rest.first() else {
            return Res::Module(m.to_vec());
        };
        let cands = self.candidates(m, first, fuel);
        if !cands.is_empty() {
            return self.follow_any(cands, &rest[1..], fuel);
        }
        if fuel.fuel == 0 || fuel.skipped {
            // Out of budget, or only a circular import names it: say nothing
            // rather than name a wrong module.
            return Res::External([&["crate".to_string()][..], m, rest].concat());
        }
        Res::Item {
            module: m.to_vec(),
            rest: rest.to_vec(),
        }
    }

    /// Follow the first candidate meaning that resolves inside the crate (a
    /// glob may offer a circular one before a useful one).
    fn follow_any(&self, cands: Vec<Found>, rest: &[String], fuel: &mut Cx) -> Res {
        let mut first_res = None;
        for c in cands {
            let saved = std::mem::replace(&mut fuel.skipped, false);
            let r = self.follow(c, rest, fuel);
            fuel.skipped = saved;
            if !matches!(r, Res::External(_)) {
                return r;
            }
            first_res.get_or_insert(r);
        }
        first_res.unwrap_or(Res::External(rest.to_vec()))
    }

    fn follow(&self, f: Found, rest: &[String], fuel: &mut Cx) -> Res {
        match f {
            Found::Child(m) => self.walk(&m, rest, fuel),
            Found::Def(m, name, DefKind::Other) => Res::Item {
                module: m,
                rest: [&[name][..], rest].concat(),
            },
            Found::Def(m, _, DefKind::Alias(target)) => {
                self.res(&m, &target, fuel).push_rest(rest, self, fuel)
            }
            Found::Bind(scope, name, paths) => {
                fuel.stack.push((scope.clone(), name.clone()));
                let r = self.follow_bind(&scope, &name, &paths, rest, fuel);
                fuel.stack.pop();
                r
            }
        }
    }

    fn follow_bind(
        &self,
        scope: &[String],
        name: &str,
        paths: &[Vec<String>],
        rest: &[String],
        fuel: &mut Cx,
    ) -> Res {
        let mut first_res = None;
        for p in paths {
            if p.len() == 1 && p[0] == name {
                // `use serde;` or `extern crate x as x`: the name is the crate.
                first_res.get_or_insert(Res::External(p.clone()));
                continue;
            }
            let r = self.res(scope, p, fuel);
            if !matches!(r, Res::External(_)) {
                return r.push_rest(rest, self, fuel);
            }
            first_res.get_or_insert(r);
        }
        first_res
            .unwrap_or_else(|| Res::External(vec![name.to_string()]))
            .push_rest(rest, self, fuel)
    }

    /// The meanings of `name` in module `m`, best first: a child module, a
    /// definition or an explicit import (which shadow globs), else whatever
    /// the glob imports offer (recursively; `seen` breaks cycles).
    fn candidates(&self, m: &[String], name: &str, fuel: &mut Cx) -> Vec<Found> {
        let mut out = Vec::new();
        self.candidates_in(m, name, &mut HashSet::new(), fuel, &mut out);
        out
    }

    fn candidates_in(
        &self,
        m: &[String],
        name: &str,
        seen: &mut HashSet<Vec<String>>,
        fuel: &mut Cx,
        out: &mut Vec<Found>,
    ) {
        if !seen.insert(m.to_vec()) {
            return;
        }
        let child = [m, &[name.to_string()][..]].concat();
        if self.mods.contains(&child) {
            out.push(Found::Child(child));
            return;
        }
        let Some(sc) = self.scopes.get(m) else { return };
        if let Some(k) = sc.defs.get(name) {
            out.push(Found::Def(m.to_vec(), name.to_string(), k.clone()));
            return;
        }
        if let Some(p) = sc.binds.get(name) {
            if fuel.stack.iter().any(|(s, n)| s == m && n == name) {
                fuel.skipped = true;
            } else {
                out.push(Found::Bind(m.to_vec(), name.to_string(), p.clone()));
                return;
            }
        }
        for (g, _) in &sc.globs {
            if fuel.fuel == 0 {
                return;
            }
            fuel.fuel -= 1;
            if let Res::Module(gm) = self.res(m, g, fuel) {
                self.candidates_in(&gm, name, seen, fuel, out);
            }
        }
    }

    /// Public re-exports visible through `m`: named bindings `(name, defining
    /// scope, path)` and globs that leave the crate, following public globs of
    /// modules of this crate. Each is resolved to its target.
    pub fn reexports(&self, m: &[String]) -> Vec<(Option<String>, Res)> {
        let mut out = Vec::new();
        self.reexports_in(m, &mut HashSet::new(), &mut out);
        out
    }

    /// Each target is resolved with its own budget; `seen` bounds the walk
    /// over modules.
    fn reexports_in(
        &self,
        m: &[String],
        seen: &mut HashSet<Vec<String>>,
        out: &mut Vec<(Option<String>, Res)>,
    ) {
        if !seen.insert(m.to_vec()) {
            return;
        }
        let Some(sc) = self.scopes.get(m) else { return };
        for (name, path) in &sc.pub_binds {
            out.push((Some(name.clone()), self.resolve(m, path)));
        }
        for (g, public) in &sc.globs {
            if !public {
                continue;
            }
            match self.resolve(m, g) {
                Res::Module(gm) => {
                    // What `gm` defines comes in too: `gm` itself is a source.
                    if self.scopes.get(&gm).is_some_and(|s| !s.defs.is_empty()) {
                        out.push((None, Res::Module(gm.clone())));
                    }
                    self.reexports_in(&gm, seen, out)
                }
                r @ Res::External(_) => out.push((None, r)),
                Res::Item { .. } => {}
            }
        }
    }

    /// Defining modules of everything a glob import of module `g` can bring in
    /// through public re-exports (the modules those items live in).
    pub fn glob_sources(&self, g: &[String]) -> BTreeSet<Vec<String>> {
        self.reexports(g)
            .into_iter()
            .filter_map(|(_, r)| r.module().map(<[String]>::to_vec))
            .collect()
    }
}
