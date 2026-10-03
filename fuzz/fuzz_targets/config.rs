#![no_main]
//! Config front end: arbitrary TOML must parse or error, and any config that
//! parses must evaluate over a small workspace without panicking.
use cargo_strata::config::Config;
use cargo_strata::crates::{Edge, evaluate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(cfg) = toml::from_str::<Config>(text) else {
        return;
    };
    let ws: std::collections::BTreeMap<String, String> = ["a", "b", "c"]
        .iter()
        .map(|n| (n.to_string(), String::new()))
        .collect();
    let mut edges = Vec::new();
    for from in ["a", "b", "c"] {
        for to in ["a", "b", "c", "serde"] {
            for kind in ["normal", "dev", "build"] {
                edges.push(Edge {
                    from: from.into(),
                    to: to.into(),
                    kind,
                    manifest: String::new(),
                    optional: kind == "build",
                });
            }
        }
    }
    let _ = evaluate(&cfg, &ws, &edges);
});
