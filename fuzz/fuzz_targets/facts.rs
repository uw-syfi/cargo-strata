#![no_main]
//! Lint front end: arbitrary source text must never panic in fact
//! extraction, token-pattern matching or attribute matching.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let Ok(src) = std::str::from_utf8(rest) else {
        return;
    };
    let _ = cargo_strata::facts::scan_facts(src, flags & 1 != 0, flags & 2 != 0);
    let pats = ["assume(".to_string(), "axiom fn".to_string(), "a::b".to_string(), "admit()".to_string()];
    let attrs = ["path".to_string(), "cfg_attr".to_string(), "a::b".to_string(), "doc".to_string()];
    let _ = cargo_strata::facts::lexical_hits(src, &pats, &attrs);
});
