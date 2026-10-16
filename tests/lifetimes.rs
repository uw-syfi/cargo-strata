//! A lifetime (`'static`, `'a`) is its own token, not the identifier or
//! keyword it is spelled with. A forbid pattern word `static` bans `static`
//! items and must not match `&'static str`; a word written with a leading
//! `'` (`'static`) matches the lifetime and nothing else.

use cargo_strata::facts::token_hits;
use cargo_strata::{Outcome, run};
use std::path::Path;

fn pats(p: &[&str]) -> Vec<String> {
    p.iter().map(|s| s.to_string()).collect()
}

fn lines(src: &str, p: &[&str]) -> Vec<usize> {
    let mut v: Vec<usize> = token_hits(src, &pats(p))
        .unwrap()
        .into_iter()
        .map(|(_, l)| l)
        .collect();
    v.sort();
    v
}

#[test]
fn static_items_are_caught() {
    let src = "\
static FOO: u8 = 1;
static mut BAR: u8 = 2;
pub static BAZ: &str = \"x\";
pub(crate) static Q: u8 = 3;
fn f() { static LOCAL: u8 = 4; }
thread_local! { static TL: u8 = 5; }
macro_rules! m { () => { static M: u8 = 6; }; }
m! { static ARG: u8 = 7; }
";
    assert_eq!(lines(src, &["static"]), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(lines(src, &["static mut"]), vec![2]);
    assert_eq!(lines(src, &["pub static"]), vec![3]);
}

#[test]
fn static_lifetime_is_not_the_keyword() {
    let src = "\
fn label() -> &'static str { \"x\" }
fn g<T: 'static>(t: T) -> Box<dyn Fn() + 'static> { todo!() }
fn h() -> impl Iterator<Item = u8> + 'static { std::iter::empty() }
struct S<'a>(&'a str, &'static [u8]);
fn w<T>() where T: Send + 'static {}
const C: &'static str = \"y\";
fn l() { 'static_label: loop { break 'static_label; } }
fn k() { m!(&'static str); }
fn c() -> char { 'a' }
";
    assert!(lines(src, &["static"]).is_empty());
    assert!(lines(src, &["a"]).is_empty());
    assert!(lines(src, &["static("]).is_empty());
}

#[test]
fn mixed_lines_report_only_items() {
    let src = "\
static NAME: &'static str = \"n\";
fn label() -> &'static str { NAME }
thread_local! { static TL: &'static str = \"t\"; }
";
    assert_eq!(lines(src, &["static"]), vec![1, 3]);
}

#[test]
fn lifetime_words_match_only_lifetimes() {
    let src = "\
fn label() -> &'static str { \"x\" }
static FOO: u8 = 1;
fn g<T: 'static>() {}
fn a<'a>(x: &'a u8) -> &'a u8 { let a = 1; x }
";
    assert_eq!(lines(src, &["'static"]), vec![1, 3]);
    assert_eq!(lines(src, &["'a"]), vec![4, 4, 4]);
    assert_eq!(lines(src, &["'static str"]), vec![1]);
}

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn check(src: &str) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
    );
    write(
        root,
        "app/Cargo.toml",
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    );
    write(root, "app/src/lib.rs", src);
    write(
        root,
        "strata.toml",
        "[[forbid]]\nname = \"no-state\"\npatterns = [\"static\"]\nallow = []\n",
    );
    run(Some(&root.join("Cargo.toml")), None).unwrap()
}

#[test]
fn forbid_end_to_end() {
    let ok = check("pub fn label() -> &'static str { \"x\" }\npub fn b<T: 'static>(_: T) {}\n");
    assert!(
        ok.lines.iter().all(|l| !l.starts_with("error")),
        "{:?}",
        ok.lines
    );
    let bad = check("pub static FOO: u8 = 1;\nthread_local! { static TL: u8 = 2; }\n");
    let errs: Vec<_> = bad
        .lines
        .iter()
        .filter(|l| l.starts_with("error"))
        .collect();
    assert_eq!(errs.len(), 2, "{:?}", bad.lines);
}

#[test]
fn verus_bodies_are_scanned_lexically() {
    let src = "\
verus! {
    static V: u8 = 1;
    fn f(x: &'static u8) -> &'static u8 { x }
}
";
    assert_eq!(lines(src, &["static"]), vec![2]);
}
