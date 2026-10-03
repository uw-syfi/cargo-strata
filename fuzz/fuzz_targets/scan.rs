#![no_main]
//! Parser front end: arbitrary source text must never panic, with and without
//! `verus!` parsing and `cfg(test)` skipping.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else {
        return;
    };
    let Ok(src) = std::str::from_utf8(rest) else {
        return;
    };
    let _ = cargo_strata::scan::scan_source(src, flags & 1 != 0, flags & 2 != 0);
});
