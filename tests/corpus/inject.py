#!/usr/bin/env python3
"""Run strata on an external workspace: a generated config must be clean, and
each injected violation (an existing edge removed from an allow list) must be
reported at the right place.

usage: inject.py WORKSPACE_DIR [--manifest REL] [--verus] [--bin PATH] [--seed N]
Exit 0 when all checks pass; findings are printed and give exit 1.
"""
import argparse, os, re, subprocess, sys, tempfile, time, tomllib, random


def q(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def emit(cfg):
    out = ["check_dev = true\n"]
    for c in cfg["crate"]:
        out.append(f"\n[[crate]]\nname = {q(c['name'])}\nallow = [{', '.join(map(q, c['allow']))}]\n")
        if c.get("allow_dev"):
            out.append(f"allow_dev = [{', '.join(map(q, c['allow_dev']))}]\n")
        if c.get("verus"):
            out.append("verus = true\n")
        for m in c.get("module", []):
            out.append(f"\n[[crate.module]]\npath = {q(m['path'])}\ndepends_on = [{', '.join(map(q, m['depends_on']))}]\n")
    return "".join(out)


def run(binp, manifest, config, ws):
    t = time.time()
    p = subprocess.run([binp, "check", "--manifest-path", manifest, "--config", config],
                       capture_output=True, text=True, cwd=os.path.dirname(manifest))
    return p, time.time() - t


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("ws")
    ap.add_argument("--manifest", default="Cargo.toml")
    ap.add_argument("--verus", action="store_true")
    ap.add_argument("--bin", default=os.path.join(os.path.dirname(__file__), "../../target/release/cargo-strata"))
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--config", help="keep the generated config here")
    a = ap.parse_args()
    ws = os.path.abspath(a.ws)
    manifest = os.path.join(ws, a.manifest)
    binp = os.path.abspath(a.bin)
    mdir = os.path.dirname(manifest)
    fails = []

    cfgfile = a.config or tempfile.mktemp(suffix=".toml")
    t = time.time()
    cmd = [binp, "init", "--manifest-path", manifest, "--output", cfgfile] + (["--verus"] if a.verus else [])
    p = subprocess.run(cmd, capture_output=True, text=True)
    print(f"init: exit {p.returncode} {time.time()-t:.1f}s")
    if p.returncode != 0:
        print(p.stderr)
        return 1
    p, secs = run(binp, manifest, cfgfile, ws)
    clean = p.stdout.strip().splitlines()
    print(f"check (generated config): exit {p.returncode} {secs:.1f}s; {len(clean)} stdout lines")
    parse_errs = [l for l in clean if l.startswith("error[parse]")]
    findings = [l for l in clean if l.startswith("error[") and not l.startswith("error[parse]")]
    if "panicked" in p.stderr:
        fails.append("PANIC on clean config")
    for l in findings[:20]:
        fails.append("FALSE FINDING " + l)
    for l in parse_errs:
        print("  parse error (not a finding):", l)
    if any(l.startswith("error[parse]") for l in clean) is False and p.returncode not in (0,):
        fails.append(f"clean config exit {p.returncode}: {p.stderr[-300:]}")

    cfg = tomllib.load(open(cfgfile, "rb"))
    rng = random.Random(a.seed)
    crates = cfg["crate"]
    injections = []
    cand = [(c, d) for c in crates for d in c["allow"]]
    if cand:
        c, d = rng.choice(cand)
        injections.append(("crate", c["name"], d))
    modc = [(c, m, t_) for c in crates for m in c.get("module", []) for t_ in m["depends_on"]
            if not any(o != t_ and (t_ == o or t_.startswith(o + "::")) for o in m["depends_on"])]
    chosen = set()
    for _ in range(2):
        pool = [x for x in modc if x[0]["name"] not in chosen] or modc
        if not pool:
            break
        c, m, t_ = rng.choice(pool)
        chosen.add(c["name"])
        injections.append(("module", c["name"], m["path"], t_))
    if len(injections) < 3:
        print(f"note: only {len(injections)} injectable edges")
    for inj in injections:
        cfg2 = tomllib.loads(open(cfgfile, "rb").read().decode())
        for c in cfg2["crate"]:
            if c["name"] != inj[1]:
                continue
            if inj[0] == "crate":
                c["allow"].remove(inj[2])
                c["allow_dev"] = [x for x in c.get("allow_dev", [])]
            else:
                for m in c["module"]:
                    if m["path"] == inj[2]:
                        m["depends_on"].remove(inj[3])
        f2 = tempfile.mktemp(suffix=".toml")
        open(f2, "w").write(emit(cfg2))
        p, secs = run(binp, manifest, f2, ws)
        os.unlink(f2)
        got = [l for l in p.stdout.splitlines() if l.startswith("error[") and not l.startswith("error[parse]")]
        print(f"inject {inj}: exit {p.returncode}, {len(got)} finding(s)")
        if "panicked" in p.stderr:
            fails.append(f"PANIC on injection {inj}")
        if not got:
            fails.append(f"MISSED {inj}")
            continue
        for l in got:
            if inj[0] == "crate":
                m = re.match(r"error\[crate\]: (\S+?):(\d+): `(.*?)` -> `(.*?)`", l)
                if not m:
                    fails.append(f"UNEXPECTED {inj}: {l}")
                    continue
                path, line, frm, to = m.group(1), int(m.group(2)), m.group(3), m.group(4)
                if (frm, to) != (inj[1], inj[2]):
                    fails.append(f"UNEXPECTED {inj}: {l}")
                    continue
                src = open(os.path.join(mdir, path)).read().splitlines()
                ok = 0 < line <= len(src)
                if ok:
                    ltxt = src[line - 1]
                    km = re.match(r"\s*\[?(?:[\w-]+\.)*?([\w-]+)\s*[=.\]]", ltxt)
                    key = km.group(1) if km else ""
                    wsdeps = tomllib.load(open(os.path.join(mdir, "Cargo.toml"), "rb")).get("workspace", {}).get("dependencies", {})
                    pkg = wsdeps.get(key, {}).get("package") if isinstance(wsdeps.get(key), dict) else None
                    ok = (to in ltxt or f'package = "{to}"' in ltxt or key in (to, to.replace("-", "_")) or pkg == to)
                if not ok:
                    fails.append(f"WRONG LOCATION {inj}: {l}")
            else:
                m = re.match(r"error\[module\]: (\S+?):(\d+): module `(.*?)` references `(.*?)` \(via `(.*?)`\)", l)
                if not m:
                    fails.append(f"UNEXPECTED {inj}: {l}")
                    continue
                path, line, frm, to, via = m.group(1), int(m.group(2)), m.group(3), m.group(4), m.group(5)
                if not (frm.startswith(inj[2]) and to == inj[3]):
                    # a reference into another module (target prefix) is still a legitimate hit only when equal
                    fails.append(f"UNEXPECTED {inj}: {l}")
                    continue
                full = os.path.join(mdir, path)
                src = open(full).read().splitlines()
                last = to.split("::")[-1].lstrip("r#")
                seg = via.split("::")[-1].lstrip("r#")
                window = "\n".join(src[max(0, line - 1):line + 2])
                if not (0 < line <= len(src)) or not (last in window or seg in window):
                    fails.append(f"WRONG LOCATION {inj}: {l}\n    line: {src[line-1] if 0 < line <= len(src) else '?'}")
    if not a.config:
        os.unlink(cfgfile)
    for f in fails:
        print("FAIL:", f)
    print("RESULT:", "ok" if not fails else f"{len(fails)} failure(s)")
    return 1 if fails else 0


sys.exit(main())
