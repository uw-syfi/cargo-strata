//! Source facts for the lint rules: struct literals, calls, `pub use`, unsafe
//! sites, `#[path]`/`include!` targets, items outside `verus!`.
//!
//! One visitor, instantiated over `syn` and (feature `verus`) `verus_syn`.
//! Macro bodies other than `verus!` are token-scanned with the same fact
//! shapes, so a struct literal inside `vec![..]` is still seen.

use proc_macro2::{Delimiter, TokenStream, TokenTree};

#[derive(Debug, Clone, Default)]
pub struct Facts {
    /// `Name { .. }` expressions: last path segment, line.
    pub struct_lits: Vec<(String, usize)>,
    /// Path calls `a::b::f(..)` (method calls excluded): segments, line.
    pub calls: Vec<(Vec<String>, usize)>,
    /// One entry per leaf of each `pub use` tree.
    pub pub_uses: Vec<PubUse>,
    /// Lines of `unsafe` blocks, fns, impls, traits and extern blocks.
    pub unsafe_sites: Vec<usize>,
    /// `(first line, last line)` of every fn with a body.
    pub fns: Vec<(usize, usize)>,
    /// `#![forbid(unsafe_code)]` is present.
    pub forbid_unsafe: bool,
    pub path_attrs: Vec<PathAttr>,
    pub includes: Vec<Include>,
    /// Top-level items (inline modules included) that are not `use`, `mod`,
    /// `extern crate` or a `verus!` block: line, kind.
    pub plain_items: Vec<(usize, String)>,
    /// Method calls `.name(..)` (every file, line).
    pub methods: Vec<(String, usize)>,
    /// The file has a top-level `fn main`.
    pub has_main: bool,
    /// Every top-level item outside `verus!` bodies: line, `kind name` (`fn main`,
    /// `mod neg`, `struct S`) or the kind alone for unnamed items (`use`,
    /// `impl`, `name!` for a macro invocation).
    pub top_items: Vec<(usize, String)>,
    /// Path and bare calls inside the body of top-level `fn main`.
    pub main_calls: Vec<(Vec<String>, usize)>,
    /// Method calls inside the body of top-level `fn main`.
    pub main_methods: Vec<(String, usize)>,
    /// Struct literals with their full path and context (parallel to `struct_lits`).
    pub lit_sites: Vec<Site>,
    /// Path calls with context (parallel to `calls`).
    pub call_sites: Vec<Site>,
    /// Definitions, imports and module declarations, for name resolution.
    pub items: crate::resolve::Items,
}

/// Context threaded into token scans of macro bodies.
#[derive(Clone, Copy)]
pub struct Ctx<'a> {
    pub inline: &'a [String],
    pub self_ty: Option<&'a Vec<String>>,
}

/// Where a struct literal or call sits, for name resolution.
#[derive(Debug, Clone)]
pub struct Site {
    /// The path as written (`Self::new` keeps `Self`).
    pub segs: Vec<String>,
    pub line: usize,
    /// Inline modules enclosing the site, outermost first.
    pub inline: Vec<String>,
    /// The path of the enclosing `impl`'s self type, if any.
    pub self_ty: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct PubUse {
    pub segs: Vec<String>,
    pub glob: bool,
    /// Inline modules enclosing the `use`, outermost first.
    pub inline: Vec<String>,
    /// Line of the `use` keyword.
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct PathAttr {
    pub path: String,
    /// Inline modules enclosing the `mod` item, outermost first.
    pub inline: Vec<String>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Include {
    pub mac: String,
    /// String literals of the argument, concatenated.
    pub text: String,
    /// The argument mentions `env!("CARGO_MANIFEST_DIR")`.
    pub manifest_dir: bool,
    /// The argument has tokens other than literals and `concat!`/`env!`.
    pub dynamic: bool,
    pub line: usize,
}

/// `ty { .. }`-looking brace group after an ident: `{ .. }` or
/// `{ lower_ident (: | , | end) .. }`.
fn looks_like_literal_body(g: &proc_macro2::Group) -> bool {
    let mut it = g.stream().into_iter();
    match it.next() {
        None => false,
        Some(TokenTree::Punct(p)) if p.as_char() == '.' => {
            matches!(it.next(), Some(TokenTree::Punct(q)) if q.as_char() == '.')
        }
        Some(TokenTree::Ident(i)) => {
            let s = i.to_string();
            let c = s.chars().next().unwrap_or('A');
            (c.is_lowercase() || c == '_')
                && match it.next() {
                    None => true,
                    Some(TokenTree::Punct(p)) => matches!(p.as_char(), ':' | ','),
                    _ => false,
                }
        }
        _ => false,
    }
}

fn include_of(mac: &str, ts: &TokenStream, line: usize) -> Include {
    let mut text = String::new();
    let (mut manifest_dir, mut dynamic) = (false, false);
    fn walk(ts: TokenStream, text: &mut String, md: &mut bool, dynamic: &mut bool) {
        for t in ts {
            match t {
                TokenTree::Literal(l) => {
                    let s = l.to_string();
                    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
                        if inner == "CARGO_MANIFEST_DIR" {
                            *md = true;
                        } else {
                            text.push_str(inner);
                        }
                    } else {
                        *dynamic = true;
                    }
                }
                TokenTree::Group(g) => walk(g.stream(), text, md, dynamic),
                TokenTree::Ident(i) => {
                    if !matches!(i.to_string().as_str(), "concat" | "env") {
                        *dynamic = true;
                    }
                }
                TokenTree::Punct(p) => {
                    if !matches!(p.as_char(), '!' | ',') {
                        *dynamic = true;
                    }
                }
            }
        }
    }
    walk(ts.clone(), &mut text, &mut manifest_dir, &mut dynamic);
    Include {
        mac: mac.to_string(),
        text,
        manifest_dir,
        dynamic,
        line,
    }
}

/// Token-level scan of a macro body (also used for `verus!` without the feature).
pub fn scan_tokens(ts: TokenStream, out: &mut Facts, ctx: Ctx) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Group(g) => {
                scan_tokens(g.stream(), out, ctx);
                i += 1;
            }
            TokenTree::Ident(id) => {
                let first = id.to_string();
                let line = id.span().start().line;
                let prev_dot =
                    i > 0 && matches!(&toks[i - 1], TokenTree::Punct(p) if p.as_char() == '.');
                if prev_dot
                    && matches!(toks.get(i + 1), Some(TokenTree::Group(g)) if g.delimiter() == Delimiter::Parenthesis)
                {
                    out.methods.push((first.clone(), line));
                }
                let prev_kw = i > 0
                    && matches!(&toks[i - 1], TokenTree::Ident(p)
                        if matches!(p.to_string().as_str(), "struct" | "enum" | "union" | "trait" | "fn" | "mod" | "type"));
                if first == "unsafe" {
                    let hit = match toks.get(i + 1) {
                        Some(TokenTree::Group(g)) => g.delimiter() == Delimiter::Brace,
                        Some(TokenTree::Ident(n)) => {
                            matches!(n.to_string().as_str(), "fn" | "impl" | "extern" | "trait")
                        }
                        _ => false,
                    };
                    if hit {
                        out.unsafe_sites.push(line);
                    }
                    i += 1;
                    continue;
                }
                // path: ident (:: ident)*
                let mut segs = vec![first];
                let mut j = i + 1;
                while j + 2 < toks.len() {
                    let (TokenTree::Punct(a), TokenTree::Punct(b), TokenTree::Ident(n)) =
                        (&toks[j], &toks[j + 1], &toks[j + 2])
                    else {
                        break;
                    };
                    if a.as_char() != ':' || b.as_char() != ':' {
                        break;
                    }
                    segs.push(n.to_string());
                    j += 3;
                }
                // turbofish
                if j + 2 < toks.len()
                    && matches!((&toks[j], &toks[j + 1]), (TokenTree::Punct(a), TokenTree::Punct(b)) if a.as_char() == ':' && b.as_char() == ':')
                    && matches!(&toks[j + 2], TokenTree::Punct(p) if p.as_char() == '<')
                {
                    let mut depth = 0i32;
                    let mut k = j + 2;
                    while k < toks.len() {
                        if let TokenTree::Punct(p) = &toks[k] {
                            match p.as_char() {
                                '<' => depth += 1,
                                '>' => {
                                    depth -= 1;
                                    if depth == 0 {
                                        k += 1;
                                        break;
                                    }
                                }
                                _ => {}
                            }
                        }
                        k += 1;
                    }
                    j = k;
                }
                match toks.get(j) {
                    Some(TokenTree::Group(g))
                        if g.delimiter() == Delimiter::Brace && !prev_kw && !prev_dot =>
                    {
                        if looks_like_literal_body(g) {
                            out.struct_lits.push((segs.last().unwrap().clone(), line));
                            out.lit_sites.push(Site {
                                segs: segs.clone(),
                                line,
                                inline: ctx.inline.to_vec(),
                                self_ty: ctx.self_ty.cloned(),
                            });
                        }
                    }
                    Some(TokenTree::Group(g))
                        if g.delimiter() == Delimiter::Parenthesis && !prev_dot && !prev_kw =>
                    {
                        out.call_sites.push(Site {
                            segs: segs.clone(),
                            line,
                            inline: ctx.inline.to_vec(),
                            self_ty: ctx.self_ty.cloned(),
                        });
                        out.calls.push((segs, line));
                    }
                    _ => {}
                }
                i = j.max(i + 1);
            }
            _ => i += 1,
        }
    }
}

macro_rules! facts_scanner {
    ($m:ident, $syn:ident) => {
        pub mod $m {
            use super::*;
            use $syn::visit::Visit;

            pub struct V<'a> {
                pub out: &'a mut Facts,
                pub inline: Vec<String>,
                pub skip_cfg_test: bool,
                pub verus_bodies: Vec<(TokenStream, Vec<String>)>,
                pub in_main: bool,
                pub impl_self: Vec<Option<Vec<String>>>,
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

            fn flatten(
                t: &$syn::UseTree,
                prefix: &mut Vec<String>,
                line: usize,
                inline: &[String],
                out: &mut Vec<PubUse>,
            ) {
                match t {
                    $syn::UseTree::Path(p) => {
                        prefix.push(p.ident.to_string());
                        flatten(&p.tree, prefix, line, inline, out);
                        prefix.pop();
                    }
                    $syn::UseTree::Name(n) => {
                        let mut s = prefix.clone();
                        s.push(n.ident.to_string());
                        out.push(PubUse {
                            segs: s,
                            glob: false,
                            inline: inline.to_vec(),
                            line,
                        });
                    }
                    $syn::UseTree::Rename(n) => {
                        let mut s = prefix.clone();
                        s.push(n.ident.to_string());
                        out.push(PubUse {
                            segs: s,
                            glob: false,
                            inline: inline.to_vec(),
                            line,
                        });
                    }
                    $syn::UseTree::Glob(_) => out.push(PubUse {
                        segs: prefix.clone(),
                        glob: true,
                        inline: inline.to_vec(),
                        line,
                    }),
                    $syn::UseTree::Group(g) => {
                        for it in &g.items {
                            flatten(it, prefix, line, inline, out);
                        }
                    }
                }
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

            fn line_of(s: proc_macro2::Span) -> usize {
                s.start().line
            }

            fn self_path(t: &$syn::Type) -> Option<Vec<String>> {
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
                fn site(&self, segs: Vec<String>, line: usize) -> Site {
                    Site {
                        segs,
                        line,
                        inline: self.inline.clone(),
                        self_ty: self.impl_self.last().cloned().flatten(),
                    }
                }

                fn fn_span(&mut self, fn_tok: proc_macro2::Span, block: &$syn::Block) {
                    self.out
                        .fns
                        .push((line_of(fn_tok), block.brace_token.span.close().end().line));
                }
            }

            impl<'ast> Visit<'ast> for V<'_> {
                fn visit_item(&mut self, i: &'ast $syn::Item) {
                    if self.skip_cfg_test && is_cfg_test(item_attrs(i)) {
                        return;
                    }
                    $syn::visit::visit_item(self, i);
                }

                fn visit_impl_item(&mut self, i: &'ast $syn::ImplItem) {
                    if let $syn::ImplItem::Fn(f) = i {
                        if self.skip_cfg_test && is_cfg_test(&f.attrs) {
                            return;
                        }
                    }
                    $syn::visit::visit_impl_item(self, i);
                }

                fn visit_trait_item(&mut self, i: &'ast $syn::TraitItem) {
                    if let $syn::TraitItem::Fn(f) = i {
                        if self.skip_cfg_test && is_cfg_test(&f.attrs) {
                            return;
                        }
                    }
                    $syn::visit::visit_trait_item(self, i);
                }

                fn visit_item_use(&mut self, i: &'ast $syn::ItemUse) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    if !matches!(i.vis, $syn::Visibility::Inherited) {
                        let line = i.use_token.span.start().line;
                        flatten(
                            &i.tree,
                            &mut Vec::new(),
                            line,
                            &self.inline,
                            &mut self.out.pub_uses,
                        );
                    }
                }

                fn visit_item_fn(&mut self, i: &'ast $syn::ItemFn) {
                    if i.sig.unsafety.is_some() {
                        self.out.unsafe_sites.push(line_of(i.sig.fn_token.span));
                    }
                    self.fn_span(i.sig.fn_token.span, &i.block);
                    let main = self.inline.is_empty() && !self.in_main && i.sig.ident == "main";
                    if main {
                        self.in_main = true;
                        self.out.has_main = true;
                    }
                    $syn::visit::visit_item_fn(self, i);
                    if main {
                        self.in_main = false;
                    }
                }

                fn visit_expr_method_call(&mut self, i: &'ast $syn::ExprMethodCall) {
                    let l = line_of(i.method.span());
                    self.out.methods.push((i.method.to_string(), l));
                    if self.in_main {
                        self.out.main_methods.push((i.method.to_string(), l));
                    }
                    $syn::visit::visit_expr_method_call(self, i);
                }

                fn visit_impl_item_fn(&mut self, i: &'ast $syn::ImplItemFn) {
                    if i.sig.unsafety.is_some() {
                        self.out.unsafe_sites.push(line_of(i.sig.fn_token.span));
                    }
                    self.fn_span(i.sig.fn_token.span, &i.block);
                    $syn::visit::visit_impl_item_fn(self, i);
                }

                fn visit_trait_item_fn(&mut self, i: &'ast $syn::TraitItemFn) {
                    if i.sig.unsafety.is_some() {
                        self.out.unsafe_sites.push(line_of(i.sig.fn_token.span));
                    }
                    if let Some(b) = &i.default {
                        self.fn_span(i.sig.fn_token.span, b);
                    }
                    $syn::visit::visit_trait_item_fn(self, i);
                }

                fn visit_foreign_item_fn(&mut self, i: &'ast $syn::ForeignItemFn) {
                    if i.sig.unsafety.is_some() {
                        self.out.unsafe_sites.push(line_of(i.sig.fn_token.span));
                    }
                    $syn::visit::visit_foreign_item_fn(self, i);
                }

                fn visit_item_impl(&mut self, i: &'ast $syn::ItemImpl) {
                    if let Some(u) = &i.unsafety {
                        self.out.unsafe_sites.push(line_of(u.span));
                    }
                    self.impl_self.push(self_path(&i.self_ty));
                    $syn::visit::visit_item_impl(self, i);
                    self.impl_self.pop();
                }

                fn visit_item_trait(&mut self, i: &'ast $syn::ItemTrait) {
                    if let Some(u) = &i.unsafety {
                        self.out.unsafe_sites.push(line_of(u.span));
                    }
                    $syn::visit::visit_item_trait(self, i);
                }

                fn visit_item_foreign_mod(&mut self, i: &'ast $syn::ItemForeignMod) {
                    if let Some(u) = &i.unsafety {
                        self.out.unsafe_sites.push(line_of(u.span));
                    }
                    $syn::visit::visit_item_foreign_mod(self, i);
                }

                fn visit_type_bare_fn(&mut self, i: &'ast $syn::TypeBareFn) {
                    if let Some(u) = &i.unsafety {
                        self.out.unsafe_sites.push(line_of(u.span));
                    }
                    $syn::visit::visit_type_bare_fn(self, i);
                }

                fn visit_expr_unsafe(&mut self, i: &'ast $syn::ExprUnsafe) {
                    self.out.unsafe_sites.push(line_of(i.unsafe_token.span));
                    $syn::visit::visit_expr_unsafe(self, i);
                }

                fn visit_expr_struct(&mut self, i: &'ast $syn::ExprStruct) {
                    if let Some(last) = i.path.segments.last() {
                        self.out
                            .struct_lits
                            .push((last.ident.to_string(), line_of(last.ident.span())));
                        if i.qself.is_none() {
                            let segs = i
                                .path
                                .segments
                                .iter()
                                .map(|s| s.ident.to_string())
                                .collect();
                            let site = self.site(segs, line_of(last.ident.span()));
                            self.out.lit_sites.push(site);
                        }
                    }
                    $syn::visit::visit_expr_struct(self, i);
                }

                fn visit_expr_call(&mut self, i: &'ast $syn::ExprCall) {
                    if let $syn::Expr::Path(p) = &*i.func {
                        if p.qself.is_none() {
                            let segs: Vec<String> = p
                                .path
                                .segments
                                .iter()
                                .map(|s| s.ident.to_string())
                                .collect();
                            if let Some(l) = p.path.segments.last() {
                                let line = line_of(l.ident.span());
                                if self.in_main {
                                    self.out.main_calls.push((segs.clone(), line));
                                }
                                let site = self.site(segs.clone(), line);
                                self.out.call_sites.push(site);
                                self.out.calls.push((segs, line));
                            }
                        }
                    }
                    $syn::visit::visit_expr_call(self, i);
                }

                fn visit_item_mod(&mut self, i: &'ast $syn::ItemMod) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    if let Some(p) = path_attr(&i.attrs) {
                        self.out.path_attrs.push(PathAttr {
                            path: p,
                            inline: self.inline.clone(),
                            line: line_of(i.mod_token.span),
                        });
                    }
                    if i.content.is_some() {
                        self.inline.push(i.ident.to_string());
                        $syn::visit::visit_item_mod(self, i);
                        self.inline.pop();
                    }
                }

                fn visit_macro(&mut self, m: &'ast $syn::Macro) {
                    let last = m
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default();
                    let line = last_line(&m.path);
                    if last == "verus" {
                        self.verus_bodies
                            .push((m.tokens.clone(), self.inline.clone()));
                    } else {
                        if matches!(last.as_str(), "include" | "include_str" | "include_bytes") {
                            self.out.includes.push(include_of(&last, &m.tokens, line));
                        }
                        let (c0, m0) = (self.out.calls.len(), self.out.methods.len());
                        let self_ty = self.impl_self.last().cloned().flatten();
                        scan_tokens(
                            m.tokens.clone(),
                            self.out,
                            Ctx {
                                inline: &self.inline,
                                self_ty: self_ty.as_ref(),
                            },
                        );
                        if self.in_main {
                            let calls = self.out.calls[c0..].to_vec();
                            let methods = self.out.methods[m0..].to_vec();
                            self.out.main_calls.extend(calls);
                            self.out.main_methods.extend(methods);
                        }
                    }
                    $syn::visit::visit_macro(self, m);
                }
            }

            fn last_line(p: &$syn::Path) -> usize {
                p.segments
                    .last()
                    .map(|s| s.ident.span().start().line)
                    .unwrap_or(0)
            }

            pub fn visit_file(
                f: &$syn::File,
                out: &mut Facts,
                skip_cfg_test: bool,
            ) -> Vec<(TokenStream, Vec<String>)> {
                let mut v = V {
                    out,
                    inline: Vec::new(),
                    skip_cfg_test,
                    verus_bodies: Vec::new(),
                    in_main: false,
                    impl_self: Vec::new(),
                };
                v.visit_file(f);
                v.verus_bodies
            }
        }
    };
}

facts_scanner!(plain, syn);
#[cfg(feature = "verus")]
facts_scanner!(verus, verus_syn);

fn item_kind_line(i: &syn::Item) -> (usize, String) {
    use syn::Item as I;
    use syn::spanned::Spanned;
    match i {
        I::Fn(x) => (x.sig.fn_token.span.start().line, "fn".into()),
        I::Struct(x) => (x.struct_token.span.start().line, "struct".into()),
        I::Enum(x) => (x.enum_token.span.start().line, "enum".into()),
        I::Union(x) => (x.union_token.span.start().line, "union".into()),
        I::Trait(x) => (x.trait_token.span.start().line, "trait".into()),
        I::Impl(x) => (x.impl_token.span.start().line, "impl".into()),
        I::Type(x) => (x.type_token.span.start().line, "type".into()),
        I::Const(x) => (x.const_token.span.start().line, "const".into()),
        I::Static(x) => (x.static_token.span.start().line, "static".into()),
        I::ForeignMod(x) => (x.abi.extern_token.span.start().line, "extern".into()),
        I::Macro(x) => (
            x.mac.path.span().start().line,
            x.mac
                .path
                .segments
                .last()
                .map(|s| format!("{}!", s.ident))
                .unwrap_or_default(),
        ),
        other => (other.span().start().line, "item".into()),
    }
}

fn top_items(items: &[syn::Item], out: &mut Vec<(usize, String)>) {
    use syn::Item as I;
    use syn::spanned::Spanned;
    for i in items {
        let named = |line: usize, kind: &str, name: &syn::Ident| (line, format!("{kind} {name}"));
        out.push(match i {
            I::Use(x) => (x.use_token.span.start().line, "use".into()),
            I::ExternCrate(x) => named(x.extern_token.span.start().line, "extern crate", &x.ident),
            I::Mod(x) => named(x.mod_token.span.start().line, "mod", &x.ident),
            I::Fn(x) => named(x.sig.fn_token.span.start().line, "fn", &x.sig.ident),
            I::Struct(x) => named(x.struct_token.span.start().line, "struct", &x.ident),
            I::Enum(x) => named(x.enum_token.span.start().line, "enum", &x.ident),
            I::Union(x) => named(x.union_token.span.start().line, "union", &x.ident),
            I::Trait(x) => named(x.trait_token.span.start().line, "trait", &x.ident),
            I::Type(x) => named(x.type_token.span.start().line, "type", &x.ident),
            I::Const(x) => named(x.const_token.span.start().line, "const", &x.ident),
            I::Static(x) => named(x.static_token.span.start().line, "static", &x.ident),
            other => {
                let (line, kind) = item_kind_line(other);
                (
                    if line == 0 {
                        other.span().start().line
                    } else {
                        line
                    },
                    kind,
                )
            }
        });
    }
}

fn plain_items(items: &[syn::Item], out: &mut Vec<(usize, String)>) {
    use syn::Item as I;
    for i in items {
        match i {
            I::Use(_) | I::ExternCrate(_) => {}
            I::Mod(m) => {
                if let Some((_, inner)) = &m.content {
                    plain_items(inner, out);
                }
            }
            I::Macro(m)
                if m.mac
                    .path
                    .segments
                    .last()
                    .is_some_and(|s| s.ident == "verus") => {}
            other => out.push(item_kind_line(other)),
        }
    }
}

fn forbids_unsafe(f: &syn::File) -> bool {
    f.attrs.iter().any(|a| {
        a.path().is_ident("forbid")
            && matches!(&a.meta, syn::Meta::List(l)
                if l.tokens.clone().into_iter().any(|t| matches!(t, TokenTree::Ident(i) if i == "unsafe_code")))
    })
}

pub fn scan_facts(src: &str, parse_verus: bool, skip_cfg_test: bool) -> Result<Facts, String> {
    if src.len() > crate::scan::MAX_SOURCE_BYTES {
        return Err(format!(
            "source larger than {} bytes",
            crate::scan::MAX_SOURCE_BYTES
        ));
    }
    std::thread::scope(|sc| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(sc, || scan_inner(src, parse_verus, skip_cfg_test))
            .map_err(|e| format!("cannot start scan thread: {e}"))?
            .join()
            .unwrap_or_else(|_| Err("internal error: panic while scanning".to_string()))
    })
}

fn scan_inner(src: &str, parse_verus: bool, skip_cfg_test: bool) -> Result<Facts, String> {
    crate::scan::check_shape(src)?;
    let file = syn::parse_file(src).map_err(|e| crate::scan::at(e.span().start(), &e))?;
    let mut out = Facts {
        forbid_unsafe: forbids_unsafe(&file),
        ..Facts::default()
    };
    plain_items(&file.items, &mut out.plain_items);
    top_items(&file.items, &mut out.top_items);
    let bodies = plain::visit_file(&file, &mut out, skip_cfg_test);
    crate::resolve::plain::visit_file(&file, &mut out.items, skip_cfg_test);
    #[cfg(feature = "verus")]
    if parse_verus {
        for (b, inline) in bodies {
            let f: verus_syn::File = verus_syn::parse2(b)
                .map_err(|e| format!("verus! body: {}", crate::scan::at(e.span().start(), &e)))?;
            let mut inner = Facts::default();
            let _ = verus::visit_file(&f, &mut inner, skip_cfg_test);
            crate::resolve::verus::visit_file(&f, &mut inner.items, skip_cfg_test);
            inner.plain_items.clear();
            out.items
                .extend_under(&inline, std::mem::take(&mut inner.items));
            for mut s in inner.lit_sites {
                s.inline = [&inline[..], &s.inline[..]].concat();
                out.lit_sites.push(s);
            }
            for mut s in inner.call_sites {
                s.inline = [&inline[..], &s.inline[..]].concat();
                out.call_sites.push(s);
            }
            for mut u in inner.pub_uses {
                u.inline = [&inline[..], &u.inline[..]].concat();
                out.pub_uses.push(u);
            }
            out.struct_lits.extend(inner.struct_lits);
            out.calls.extend(inner.calls);
            out.methods.extend(inner.methods);
            out.unsafe_sites.extend(inner.unsafe_sites);
            out.fns.extend(inner.fns);
            out.path_attrs.extend(inner.path_attrs);
            out.includes.extend(inner.includes);
        }
    }
    #[cfg(not(feature = "verus"))]
    let _ = (parse_verus, bodies);
    Ok(out)
}

/// One compiled token pattern word.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Word {
    Ident(String),
    /// `::`
    Path,
    /// `name(`: call. `empty` for `name()`.
    Call {
        name: String,
        empty: bool,
    },
}

fn compile(pattern: &str) -> Vec<Word> {
    let mut out = Vec::new();
    for w in pattern.split_whitespace() {
        if let Some(name) = w.strip_suffix("()") {
            out.push(Word::Call {
                name: name.to_string(),
                empty: true,
            });
        } else if let Some(name) = w.strip_suffix('(') {
            out.push(Word::Call {
                name: name.to_string(),
                empty: false,
            });
        } else {
            for (i, seg) in w.split("::").enumerate() {
                if i > 0 {
                    out.push(Word::Path);
                }
                out.push(Word::Ident(seg.to_string()));
            }
        }
    }
    out
}

fn is_punct(t: &TokenTree, c: char) -> bool {
    matches!(t, TokenTree::Punct(p) if p.as_char() == c)
}

/// Does `pat` match `toks` starting at `i`? Returns the first token's line.
fn match_at(pat: &[Word], toks: &[TokenTree], i: usize) -> bool {
    let mut j = i;
    let mut k = 0;
    while k < pat.len() {
        match &pat[k] {
            Word::Ident(n) => {
                if !matches!(toks.get(j), Some(TokenTree::Ident(id)) if id == n) {
                    return false;
                }
                j += 1;
            }
            Word::Path => {
                if !(toks.get(j).is_some_and(|t| is_punct(t, ':'))
                    && toks.get(j + 1).is_some_and(|t| is_punct(t, ':')))
                {
                    return false;
                }
                j += 2;
            }
            Word::Call { name, empty } => {
                if !matches!(toks.get(j), Some(TokenTree::Ident(id)) if id == name) {
                    return false;
                }
                if i > 0 && j == i && (is_punct(&toks[i - 1], '.') || is_punct(&toks[i - 1], ':')) {
                    return false;
                }
                match toks.get(j + 1) {
                    Some(TokenTree::Group(g))
                        if g.delimiter() == Delimiter::Parenthesis
                            && (!*empty || g.stream().is_empty()) => {}
                    _ => return false,
                }
                j += 2;
            }
        }
        k += 1;
    }
    true
}

fn hits_in(ts: TokenStream, pats: &[(String, Vec<Word>)], out: &mut Vec<(String, usize)>) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    for i in 0..toks.len() {
        for (text, pat) in pats {
            if !pat.is_empty() && match_at(pat, &toks, i) {
                let line = match &toks[i] {
                    TokenTree::Ident(id) => id.span().start().line,
                    TokenTree::Group(g) => g.span().start().line,
                    TokenTree::Punct(p) => p.span().start().line,
                    TokenTree::Literal(l) => l.span().start().line,
                };
                out.push((text.clone(), line));
            }
        }
        if let TokenTree::Group(g) = &toks[i] {
            hits_in(g.stream(), pats, out);
        }
    }
}

/// Occurrences of token patterns anywhere in a file, `verus!` bodies
/// included (a lexical scan: no feature needed). `(pattern, line)`.
pub fn token_hits(src: &str, patterns: &[String]) -> Result<Vec<(String, usize)>, String> {
    if src.len() > crate::scan::MAX_SOURCE_BYTES {
        return Err(format!(
            "source larger than {} bytes",
            crate::scan::MAX_SOURCE_BYTES
        ));
    }
    crate::scan::check_shape(src)?;
    let pats: Vec<(String, Vec<Word>)> = patterns.iter().map(|p| (p.clone(), compile(p))).collect();
    std::thread::scope(|sc| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(sc, || {
                let ts: TokenStream = src
                    .parse()
                    .map_err(|e: proc_macro2::LexError| e.to_string())?;
                let mut out = Vec::new();
                hits_in(ts, &pats, &mut out);
                Ok(out)
            })
            .map_err(|e| format!("cannot start scan thread: {e}"))?
            .join()
            .unwrap_or_else(|_| Err("internal error: panic while scanning".to_string()))
    })
}
