use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo strata check [--manifest-path <Cargo.toml>] [--config <strata.toml>] [--strict]\n       cargo strata init [--manifest-path <Cargo.toml>] [--output <strata.toml>] [--verus]";

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("strata") {
        args.remove(0);
    }
    if args.first().map(String::as_str) == Some("init") {
        return init(args);
    }
    if args.first().map(String::as_str) != Some("check") {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let (mut manifest, mut config, mut strict) = (None, None, false);
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest-path" => match it.next() {
                Some(v) => manifest = Some(PathBuf::from(v)),
                None => return usage_err("--manifest-path needs a value"),
            },
            "--config" => match it.next() {
                Some(v) => config = Some(PathBuf::from(v)),
                None => return usage_err("--config needs a value"),
            },
            "--strict" => strict = true,
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match cargo_strata::run_with(manifest.as_deref(), config.as_deref(), strict) {
        Ok(o) => {
            for w in &o.warnings {
                eprintln!("warning: {w}");
            }
            for l in &o.lines {
                println!("{l}");
            }
            if o.violations > 0 {
                eprintln!("{} violation(s)", o.violations);
                ExitCode::from(1)
            } else if o.errors > 0 {
                ExitCode::from(2)
            } else {
                println!("strata: ok");
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            for l in e.lines() {
                eprintln!("error: {l}");
            }
            ExitCode::from(2)
        }
    }
}

fn init(args: Vec<String>) -> ExitCode {
    let (mut manifest, mut output, mut verus) = (None, None, false);
    let mut it = args.into_iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--manifest-path" => match it.next() {
                Some(v) => manifest = Some(PathBuf::from(v)),
                None => return usage_err("--manifest-path needs a value"),
            },
            "--output" => match it.next() {
                Some(v) => output = Some(PathBuf::from(v)),
                None => return usage_err("--output needs a value"),
            },
            "--verus" => verus = true,
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    match cargo_strata::init(manifest.as_deref(), verus) {
        Ok((text, errors)) => {
            for e in &errors {
                eprintln!("warning: {e}");
            }
            match output {
                Some(p) => {
                    if let Err(e) = std::fs::write(&p, text) {
                        eprintln!("error: cannot write {}: {e}", p.display());
                        return ExitCode::from(2);
                    }
                }
                None => print!("{text}"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage_err(msg: &str) -> ExitCode {
    eprintln!("error: {msg}\n{USAGE}");
    ExitCode::from(2)
}
