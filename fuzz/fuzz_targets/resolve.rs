#![no_main]
//! Name resolution: arbitrary source must never make the item table or a
//! query panic or loop (glob cycles, self-referential imports, deep chains).
use cargo_strata::resolve::Table;
use libfuzzer_sys::fuzz_target;
use std::collections::HashSet;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let Ok(src) = std::str::from_utf8(rest) else {
        return;
    };
    let Ok(scan) = cargo_strata::scan::scan_source(src, flags & 1 != 0, flags & 2 != 0) else {
        return;
    };
    let mut known: HashSet<Vec<String>> = HashSet::new();
    known.insert(vec![]);
    for m in &scan.items.inline_mods {
        known.insert(m.clone());
    }
    let t = Table::build(&known, [(&[][..], &scan.items)]);
    for b in &scan.items.binds {
        let _ = t.resolve(&b.inline, &b.path);
        let _ = t.reexports(&b.inline);
        let _ = t.glob_sources(&b.inline);
    }
    for r in scan.refs.iter().chain(&scan.bare) {
        let _ = t.resolve(&r.inline, &r.segs);
    }
});
