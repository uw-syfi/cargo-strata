#!/usr/bin/env python3
"""Re-run the external-workspace checks from tests/corpus.toml.

usage: run.py [--dir CORPUS_DIR] [--only NAME] [--seeds N] [--bin PATH]
Clones each pinned commit (shallow) into CORPUS_DIR if missing, then runs
inject.py: generated config clean, injected violations reported at the right
place. Needs network and a Rust toolchain; the verus entry needs a build with
`--features verus`.
"""
import argparse, os, subprocess, sys, time, tomllib

here = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("--dir", default=os.path.join(here, "../../target/corpus"))
ap.add_argument("--only")
ap.add_argument("--seeds", type=int, default=2)
ap.add_argument("--bin", default=os.path.join(here, "../../target/release/cargo-strata"))
a = ap.parse_args()
manifest = tomllib.load(open(os.path.join(here, "../corpus.toml"), "rb"))
os.makedirs(a.dir, exist_ok=True)
bad = 0
for w in manifest["workspace"]:
    if a.only and w["name"] != a.only:
        continue
    d = os.path.join(a.dir, w["name"])
    if not os.path.isdir(d):
        subprocess.run(["git", "init", "-q", d], check=True)
        subprocess.run(["git", "-C", d, "fetch", "-q", "--depth", "1", w["repo"], w["commit"]], check=True)
        subprocess.run(["git", "-C", d, "checkout", "-q", "FETCH_HEAD"], check=True)
    print(f"=== {w['name']} @ {w['commit'][:10]}", flush=True)
    for seed in range(1, a.seeds + 1):
        cmd = [sys.executable, os.path.join(here, "inject.py"), d, "--manifest", w["manifest"],
               "--bin", a.bin, "--seed", str(seed)] + (["--verus"] if w.get("verus") else [])
        t = time.time()
        p = subprocess.run(cmd, capture_output=True, text=True)
        out = p.stdout
        # Expected parse errors are not failures.
        print(out.strip().splitlines()[-1], f"(seed {seed}, {time.time()-t:.0f}s)")
        if p.returncode != 0:
            bad += 1
            print(out)
            print(p.stderr[-500:])
sys.exit(1 if bad else 0)
