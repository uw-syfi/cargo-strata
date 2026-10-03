//! Pathological nesting must produce an error or a result, never a stack overflow.
use cargo_strata::scan::scan_source;

fn run(name: &str, src: String) {
    eprintln!("case {name}");
    let h = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            let _ = scan_source(&src, true, true);
            let _ = cargo_strata::facts::scan_facts(&src, true, false);
            let _ = cargo_strata::facts::token_hits(&src, &["assume(".into(), "a::b".into()]);
        })
        .unwrap();
    h.join().unwrap_or_else(|_| panic!("{name} panicked"));
}

#[test]
fn deep_nesting_is_an_error_not_a_crash() {
    let n = 200_000;
    run(
        "parens",
        format!("fn f() {{ {}1{} }}", "(".repeat(n), ")".repeat(n)),
    );
    run(
        "braces",
        format!("fn f() {}{}", "{".repeat(n), "}".repeat(n)),
    );
    run("mods", format!("{}{}", "mod a {".repeat(n), "}".repeat(n)));
    run("macro", format!("m!{}{}", "(".repeat(n), ")".repeat(n)));
    run(
        "verus",
        format!("verus! {{ {} {} }}", "fn f() {".repeat(n), "}".repeat(n)),
    );
    run(
        "use",
        format!("use {}a{};", "a::{".repeat(n), "}".repeat(n)),
    );
    run(
        "path",
        format!("fn f() {{ let _ = {}; }}", vec!["crate"; n].join("::")),
    );
    run(
        "types",
        format!("type T = {}u8{};", "Vec<".repeat(n), ">".repeat(n)),
    );
    run("unary", format!("fn f() {{ let _ = {}1; }}", "-".repeat(n)));
    run("ref", format!("fn f() {{ let _ = {}x; }}", "&".repeat(n)));
    run("deref", format!("fn f() {{ let _ = {}x; }}", "*".repeat(n)));
    run("not", format!("fn f() {{ let _ = {}x; }}", "!".repeat(n)));
    run(
        "binary",
        format!("fn f() {{ let _ = 1{}; }}", "+1".repeat(n)),
    );
    run(
        "method",
        format!("fn f() {{ let _ = x{}; }}", ".a()".repeat(n)),
    );
    run(
        "else_if",
        format!("fn f() {{ if a {{}} {} }}", "else if a {}".repeat(n)),
    );
    run(
        "cast",
        format!("fn f() {{ let _ = x{}; }}", " as u8".repeat(n)),
    );
    run(
        "refpat",
        format!("fn f() {{ let {}x = 1; }}", "&".repeat(n)),
    );
    run("attr", format!("#[a{}] fn f() {{}}", "(b)".repeat(n)));
    run(
        "generics_path",
        format!("type T = {};", "a::".repeat(n) + "b"),
    );
    run(
        "closure",
        format!("fn f() {{ let _ = {}1; }}", "|| ".repeat(n)),
    );
    run(
        "array_ty",
        format!("type T = {}u8{};", "[".repeat(n), "; 1]".repeat(n)),
    );
    run("lifetimes", format!("fn f<{}>() {{}}", "'a,".repeat(n)));
    run(
        "big_string",
        format!("const S: &str = \"{}\";", "(".repeat(n * 10)),
    );
    run(
        "big_comment",
        format!("/* {} */ fn f() {{}}", "( /*".repeat(n)),
    );
}
