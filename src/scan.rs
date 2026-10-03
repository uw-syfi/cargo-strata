//! Syntactic extraction of module-path references from one parsed file.
//!
//! The visitor is written once as a macro and instantiated over `syn` (always)
//! and `verus_syn` (feature `verus`), whose AST and `Visit` trait have the same
//! shape for the nodes used here.

use proc_macro2::{TokenStream, TokenTree};

/// A path that starts with `crate`, `self`, `super`, or (in a `use`) a name
/// that may be a child module. Not yet resolved to an absolute module path.
#[derive(Debug, Clone)]
pub struct RawRef {
    pub segs: Vec<String>,
    pub line: usize,
    /// Inline `mod x { }` nesting inside the file, outermost first.
    pub inline: Vec<String>,
    /// True if it came from a `use` item (bare first segment may be a child module).
    pub in_use: bool,
    /// A glob import (`use a::b::*`): `segs` names the module globbed.
    pub glob: bool,
}

/// An out-of-line `mod x;` declaration.
#[derive(Debug, Clone)]
pub struct ModDecl {
    pub name: String,
    pub path_attr: Option<String>,
    pub inline: Vec<String>,
}

/// The tokens of a `verus! { ... }` invocation.
#[derive(Debug, Clone)]
pub struct VerusBody {
    pub tokens: TokenStream,
    pub inline: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub refs: Vec<RawRef>,
    pub decls: Vec<ModDecl>,
    pub verus: Vec<VerusBody>,
    /// Bodies of item-position macro invocations other than `verus!`
    /// (`cfg_rt! { pub mod runtime; }`), parsed as items after the file.
    pub macros: Vec<VerusBody>,
    /// Inline modules declared (`mod x { .. }`), as paths relative to the file.
    pub inline_mods: Vec<Vec<String>>,
    /// Multi-segment paths outside `use` that do not start with `crate`,
    /// `self` or `super` (`memory::f()`, `Thing::new()`); only used by name resolution.
    pub bare: Vec<RawRef>,
    /// Definitions and imports, for name resolution.
    pub items: crate::resolve::Items,
}

pub fn is_root_kw(s: &str) -> bool {
    matches!(s, "crate" | "self" | "super")
}

/// Find `crate::a::b`-style chains in a macro token stream.
pub fn scan_tokens(ts: TokenStream, inline: &[String], out: &mut Vec<RawRef>) {
    let toks: Vec<TokenTree> = ts.into_iter().collect();
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            TokenTree::Group(g) => scan_tokens(g.stream(), inline, out),
            TokenTree::Ident(id) if is_root_kw(&id.to_string()) => {
                let line = id.span().start().line;
                let mut segs = vec![id.to_string()];
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
                if segs.len() >= 2 {
                    out.push(RawRef {
                        segs,
                        line,
                        inline: inline.to_vec(),
                        in_use: false,
                        glob: false,
                    });
                }
                i = j.max(i + 1);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

macro_rules! scanner {
    ($m:ident, $syn:ident) => {
        pub mod $m {
            use super::*;
            use $syn::visit::Visit;

            pub struct V<'a> {
                pub out: &'a mut Scan,
                pub inline: Vec<String>,
                pub skip_cfg_test: bool,
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

            fn flatten(t: &$syn::UseTree, prefix: &mut Vec<String>, line: usize, v: &mut V) {
                match t {
                    $syn::UseTree::Path(p) => {
                        prefix.push(p.ident.to_string());
                        flatten(&p.tree, prefix, line, v);
                        prefix.pop();
                    }
                    $syn::UseTree::Name(n) => {
                        let mut s = prefix.clone();
                        if n.ident != "self" {
                            s.push(n.ident.to_string());
                        }
                        v.push(s, n.ident.span().start().line.max(line), false);
                    }
                    $syn::UseTree::Rename(n) => {
                        let mut s = prefix.clone();
                        if n.ident != "self" {
                            s.push(n.ident.to_string());
                        }
                        v.push(s, n.ident.span().start().line.max(line), false);
                    }
                    $syn::UseTree::Glob(g) => v.push(
                        prefix.clone(),
                        g.star_token.span.start().line.max(line),
                        true,
                    ),
                    $syn::UseTree::Group(g) => {
                        for it in &g.items {
                            flatten(it, prefix, line, v);
                        }
                    }
                }
            }

            /// Attributes of any item kind we know; others are never `cfg(test)`-skipped.
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

            impl V<'_> {
                fn push(&mut self, segs: Vec<String>, line: usize, glob: bool) {
                    if segs.is_empty() {
                        return;
                    }
                    self.out.refs.push(RawRef {
                        segs,
                        line,
                        inline: self.inline.clone(),
                        in_use: true,
                        glob,
                    });
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
                    use $syn::ImplItem as I;
                    let attrs: &[$syn::Attribute] = match i {
                        I::Const(x) => &x.attrs,
                        I::Fn(x) => &x.attrs,
                        I::Type(x) => &x.attrs,
                        _ => &[],
                    };
                    if self.skip_cfg_test && is_cfg_test(attrs) {
                        return;
                    }
                    $syn::visit::visit_impl_item(self, i);
                }

                fn visit_trait_item(&mut self, i: &'ast $syn::TraitItem) {
                    use $syn::TraitItem as I;
                    let attrs: &[$syn::Attribute] = match i {
                        I::Const(x) => &x.attrs,
                        I::Fn(x) => &x.attrs,
                        I::Type(x) => &x.attrs,
                        _ => &[],
                    };
                    if self.skip_cfg_test && is_cfg_test(attrs) {
                        return;
                    }
                    $syn::visit::visit_trait_item(self, i);
                }

                fn visit_item_use(&mut self, i: &'ast $syn::ItemUse) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    let line = i.use_token.span.start().line;
                    flatten(&i.tree, &mut Vec::new(), line, self);
                }

                fn visit_item_fn(&mut self, i: &'ast $syn::ItemFn) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    $syn::visit::visit_item_fn(self, i);
                }

                fn visit_path(&mut self, p: &'ast $syn::Path) {
                    if let Some(first) = p.segments.first() {
                        if p.segments.len() >= 2 {
                            let rooted = is_root_kw(&first.ident.to_string());
                            let segs = p.segments.iter().map(|s| s.ident.to_string()).collect();
                            let line = first.ident.span().start().line;
                            let r = RawRef {
                                segs,
                                line,
                                inline: self.inline.clone(),
                                in_use: false,
                                glob: false,
                            };
                            if rooted {
                                self.out.refs.push(r);
                            } else {
                                self.out.bare.push(r);
                            }
                        }
                    }
                    $syn::visit::visit_path(self, p);
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

                fn visit_item_macro(&mut self, i: &'ast $syn::ItemMacro) {
                    if self.skip_cfg_test && is_cfg_test(&i.attrs) {
                        return;
                    }
                    let verus = i
                        .mac
                        .path
                        .segments
                        .last()
                        .is_some_and(|s| s.ident == "verus");
                    if i.ident.is_none() && !verus {
                        // Parsed as items after the file; if that fails, the
                        // tokens are scanned for `crate::` chains instead.
                        self.out.macros.push(VerusBody {
                            tokens: i.mac.tokens.clone(),
                            inline: self.inline.clone(),
                        });
                        return;
                    }
                    $syn::visit::visit_item_macro(self, i);
                }

                fn visit_macro(&mut self, m: &'ast $syn::Macro) {
                    let last = m
                        .path
                        .segments
                        .last()
                        .map(|s| s.ident.to_string())
                        .unwrap_or_default();
                    if last == "verus" {
                        self.out.verus.push(VerusBody {
                            tokens: m.tokens.clone(),
                            inline: self.inline.clone(),
                        });
                    } else {
                        scan_tokens(m.tokens.clone(), &self.inline, &mut self.out.refs);
                    }
                    $syn::visit::visit_macro(self, m);
                }
            }

            pub fn visit_file(f: &$syn::File, out: &mut Scan, skip_cfg_test: bool) {
                let mut v = V {
                    out,
                    inline: Vec::new(),
                    skip_cfg_test,
                };
                v.visit_file(f);
            }
        }
    };
}

scanner!(plain, syn);
#[cfg(feature = "verus")]
scanner!(verus, verus_syn);

/// Maximum bracket (and generic-argument) nesting accepted before parsing.
/// The token-stream lexer and the parser recurse per nested group.
pub const MAX_NESTING: usize = 256;

/// Maximum tokens between statement or argument separators (`;` `,` `{` `}`).
/// Flat chains such as `1+1+1+...` or `-----x` parse into trees as deep as
/// the chain is long, and recurse on drop and visit.
pub const MAX_CHAIN_TOKENS: usize = 20_000;

/// Reject source whose shape would make the parser recurse too deeply:
/// bracket nesting, generic nesting, or a very long separator-free chain.
/// Comments, strings and char literals are skipped; this is a lexical
/// approximation, only ever used to refuse pathological input.
pub fn check_shape(src: &str) -> Result<(), String> {
    let b = src.as_bytes();
    let (mut i, mut depth, mut angle, mut chain) = (0usize, 0usize, 0usize, 0usize);
    let too_deep = || format!("nesting deeper than {MAX_NESTING} brackets");
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut nest = 1;
            i += 2;
            while i < b.len() && nest > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    nest += 1;
                    i += 1;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    nest -= 1;
                    i += 1;
                }
                i += 1;
            }
            continue;
        }
        chain += 1;
        if chain > MAX_CHAIN_TOKENS {
            return Err(format!(
                "more than {MAX_CHAIN_TOKENS} tokens without a separator"
            ));
        }
        match c {
            b'"' => {
                // Raw string if preceded by `r` and any number of `#`.
                let mut j = i;
                let mut hashes = 0;
                while j > 0 && b[j - 1] == b'#' {
                    hashes += 1;
                    j -= 1;
                }
                let raw = j > 0 && b[j - 1] == b'r';
                i += 1;
                while i < b.len() {
                    if !raw && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"'
                        && (!raw
                            || b.len() >= i + 1 + hashes
                                && b[i + 1..i + 1 + hashes].iter().all(|&c| c == b'#'))
                    {
                        if raw {
                            i += hashes;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // Char literal `'x'` or `'\..'`; otherwise a lifetime or label.
                if b.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < b.len() && b[i] != b'\'' && b[i] != b'\n' {
                        i += 1;
                    }
                } else if let Some(ch) = src[i + 1..].chars().next() {
                    let end = i + 1 + ch.len_utf8();
                    if b.get(end) == Some(&b'\'') {
                        i = end;
                    }
                }
            }
            b'<' => {
                angle += 1;
                if angle > MAX_NESTING {
                    return Err(too_deep());
                }
            }
            b'>' if i == 0 || !matches!(b[i - 1], b'-' | b'=') => angle = angle.saturating_sub(1),
            b'(' | b'[' => {
                angle = 0;
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(too_deep());
                }
            }
            b'{' => {
                angle = 0;
                chain = 0;
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(too_deep());
                }
            }
            b')' | b']' => {
                angle = 0;
                depth = depth.saturating_sub(1);
            }
            b'}' => {
                angle = 0;
                chain = 0;
                depth = depth.saturating_sub(1);
            }
            b';' | b',' => {
                angle = 0;
                chain = 0;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80 => {
                while i + 1 < b.len()
                    && (b[i + 1].is_ascii_alphanumeric() || b[i + 1] == b'_' || b[i + 1] >= 0x80)
                {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

/// Parse a Rust source file and scan it. Tokens of `verus! { }` bodies are
/// only parsed when the `verus` feature is enabled and `parse_verus` is set.
/// `line:column: message` for a parser error, so a parse failure in a large
/// file can be found.
pub fn at(loc: proc_macro2::LineColumn, e: &dyn std::fmt::Display) -> String {
    format!("{}:{}: {e}", loc.line, loc.column + 1)
}

pub fn scan_source(src: &str, parse_verus: bool, skip_cfg_test: bool) -> Result<Scan, String> {
    if src.len() > MAX_SOURCE_BYTES {
        return Err(format!("source larger than {MAX_SOURCE_BYTES} bytes"));
    }
    // The parser and visitors recurse on expression depth, which flat chains
    // such as `1+1+1+...` make as deep as the input is long. Run on a large
    // (lazily committed) stack so source up to `MAX_SOURCE_BYTES` cannot
    // overflow it, and turn any panic into an error.
    std::thread::scope(|sc| {
        std::thread::Builder::new()
            .stack_size(SCAN_STACK_BYTES)
            .spawn_scoped(sc, || {
                // `Scan` holds token streams, which are not `Send`; the
                // result crossing the thread boundary never does.
                scan_source_inner(src, parse_verus, skip_cfg_test)
                    .map(|s| (s.refs, s.decls, s.inline_mods, s.bare, s.items))
            })
            .map_err(|e| format!("cannot start scan thread: {e}"))?
            .join()
            .unwrap_or_else(|_| Err("internal error: panic while scanning".to_string()))
    })
    .map(|(refs, decls, inline_mods, bare, items)| Scan {
        refs,
        decls,
        verus: Vec::new(),
        macros: Vec::new(),
        inline_mods,
        bare,
        items,
    })
}

/// Largest source file accepted.
pub const MAX_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const SCAN_STACK_BYTES: usize = 1 << 30;

/// Bodies found inside a nested scan, still to be parsed.
#[derive(Default)]
struct Pending {
    verus: Vec<VerusBody>,
    macros: Vec<VerusBody>,
}

/// Fold the scan of a nested body (a `verus!` block or a macro's items) into
/// `out`, as if its items sat inside `inline` in the enclosing file.
fn merge(out: &mut Scan, mut inner: Scan, inline: &[String], next: &mut Pending) {
    let prefix = |v: &[String]| [inline, v].concat();
    out.items
        .extend_under(inline, std::mem::take(&mut inner.items));
    for mut r in inner.refs {
        r.inline = prefix(&r.inline);
        out.refs.push(r);
    }
    for mut r in inner.bare {
        r.inline = prefix(&r.inline);
        out.bare.push(r);
    }
    for mut d in inner.decls {
        d.inline = prefix(&d.inline);
        out.decls.push(d);
    }
    for m in inner.inline_mods {
        out.inline_mods.push(prefix(&m));
    }
    for (from, to) in [
        (inner.verus, &mut next.verus),
        (inner.macros, &mut next.macros),
    ] {
        for mut b in from {
            b.inline = prefix(&b.inline);
            to.push(b);
        }
    }
}

/// Nesting of `verus!` blocks and item macros followed (a macro whose body
/// invokes another).
const MAX_MACRO_DEPTH: usize = 8;

fn scan_source_inner(src: &str, parse_verus: bool, skip_cfg_test: bool) -> Result<Scan, String> {
    check_shape(src)?;
    let file = syn::parse_file(src).map_err(|e| at(e.span().start(), &e))?;
    let mut out = Scan::default();
    plain::visit_file(&file, &mut out, skip_cfg_test);
    crate::resolve::plain::visit_file(&file, &mut out.items, skip_cfg_test);
    let mut pending = Pending {
        verus: std::mem::take(&mut out.verus),
        macros: std::mem::take(&mut out.macros),
    };
    // Item macros whose body parses as items (`cfg_rt! { pub mod x; }`) are
    // read as if the items were written in place; any other body is skipped.
    for _ in 0..MAX_MACRO_DEPTH {
        if pending.verus.is_empty() && pending.macros.is_empty() {
            break;
        }
        let cur = std::mem::take(&mut pending);
        #[cfg(feature = "verus")]
        if parse_verus {
            for b in cur.verus {
                let f: verus_syn::File = verus_syn::parse2(b.tokens)
                    .map_err(|e| format!("verus! body: {}", at(e.span().start(), &e)))?;
                let mut inner = Scan::default();
                verus::visit_file(&f, &mut inner, skip_cfg_test);
                crate::resolve::verus::visit_file(&f, &mut inner.items, skip_cfg_test);
                merge(&mut out, inner, &b.inline, &mut pending);
            }
        }
        #[cfg(not(feature = "verus"))]
        let _ = parse_verus;
        for b in cur.macros {
            let Ok(f) = syn::parse2::<syn::File>(b.tokens.clone()) else {
                scan_tokens(b.tokens, &b.inline, &mut out.refs);
                continue;
            };
            let mut inner = Scan::default();
            plain::visit_file(&f, &mut inner, skip_cfg_test);
            crate::resolve::plain::visit_file(&f, &mut inner.items, skip_cfg_test);
            merge(&mut out, inner, &b.inline, &mut pending);
        }
    }
    out.verus.clear();
    out.macros.clear();
    Ok(out)
}
