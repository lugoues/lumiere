#!/usr/bin/env python3
"""Differential harness: replay every shipped animation through the Python
reference engine (NeewerLux) under a virtual clock and diff its frame stream
against `cargo xtask dump-schedule` output for the converted file.

Usage: python3 assets/dev/diff_engine.py [--tolerance-ms 40] [names...]
Requires: references/NeewerLux checkout and a built xtask (cargo build -p xtask).
"""
import json, re, subprocess, sys, glob, os

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
REF = os.path.join(ROOT, "references/NeewerLux/NeewerLux.py")
SRC_DIR = os.path.join(ROOT, "references/NeewerLux/light_prefs/animations")
CONV_DIR = os.path.join(ROOT, "assets/animations")
XTASK = os.path.join(ROOT, "target/debug/xtask")

def grab(src, name):
    m = re.search(rf'^def {name}\(.*?(?=^def |^class |^async def )', src, re.S | re.M)
    if not m:
        raise SystemExit(f"could not extract {name} from reference")
    return m.group(0)

def build_python_engine():
    src = open(REF).read()
    code = grab(src, "interpolateHSI") + grab(src, "interpolateCCT") + grab(src, "animationEngineThread")

    class VirtualTime:
        def __init__(self):
            self.now = 0.0
        def sleep(self, seconds):
            self.now += seconds
        def time(self):
            return self.now

    def run(animation):
        clock = VirtualTime()
        frames = []
        env = {
            "time": clock,
            "printDebugString": lambda s: None,
            "animationSendFrame": lambda cmds, loop: frames.append((round(clock.now * 1000), [dict(c) for c in cmds])),
            "mainWindow": None,
            "preAnimationStates": {},
            "animRevertOnFinish": False,
            "threadAction": "",
            "workerWakeEvent": type("E", (), {"set": staticmethod(lambda: None)})(),
        }
        exec(code, env)
        env["animationEngineThread"](animation, None, speedMultiplier=1.0, loopOverride=False, fps=5, briScale=1.0, maxLoops=0)
        return frames
    return run

def normalize_python(frames):
    out = []
    for at_ms, cmds in frames:
        ops = []
        for c in sorted(cmds, key=lambda c: str(c.get("light"))):
            mode = c.get("mode", "").upper()
            if mode == "HSI":
                ops.append((str(c["light"]), "hsi", (c.get("hue", 0) % 360, c.get("sat", 100), c.get("bri", 100))))
            elif mode == "CCT":
                t = c.get("temp", 56)
                if t > 100:
                    t = round(t / 100)
                ops.append((str(c["light"]), "cct", (t, c.get("bri", 100))))
            else:
                ops.append((str(c["light"]), mode.lower(), ()))
        out.append((at_ms, ops))
    return out

def normalize_rust(lines):
    out = []
    for line in lines:
        f = json.loads(line)
        ops = []
        for op in sorted(f["ops"], key=lambda o: o["target"]):
            m = op["mode"]
            kind = m["mode"]
            if kind == "hsi":
                ops.append((op["target"], "hsi", (m["hue"] % 360, m["sat"], m["bri"])))
            elif kind == "cct":
                ops.append((op["target"], "cct", (round(m["temp"] / 100), m["bri"])))
            else:
                ops.append((op["target"], kind, ()))
        out.append((f["at_ms"], ops))
    return out

def diff(name, py, rs, tol_ms, tol_val=2):
    if len(py) != len(rs):
        return f"frame count differs: python {len(py)} vs rust {len(rs)}"
    for i, ((pt, pops), (rt, rops)) in enumerate(zip(py, rs)):
        if abs(pt - rt) > tol_ms:
            return f"frame {i}: time {pt}ms vs {rt}ms"
        if len(pops) != len(rops):
            return f"frame {i}: op count {len(pops)} vs {len(rops)}"
        for (ptg, pk, pv), (rtg, rk, rv) in zip(pops, rops):
            if ptg != rtg or pk != rk:
                return f"frame {i}: target/mode ({ptg},{pk}) vs ({rtg},{rk})"
            for a, b in zip(pv, rv):
                delta = min(abs(a - b), 360 - abs(a - b)) if pk == "hsi" and a is pv[0] else abs(a - b)
                if delta > tol_val:
                    return f"frame {i}: values {pv} vs {rv}"
    return None

def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    tol_ms = 40
    for a in sys.argv[1:]:
        if a.startswith("--tolerance-ms"):
            tol_ms = int(a.split("=")[1])
    engine = build_python_engine()
    failures = 0
    files = sorted(glob.glob(os.path.join(SRC_DIR, "*.json")))
    for f in files:
        anim = json.load(open(f))
        name = anim["name"]
        if args and not any(a.lower() in name.lower() for a in args):
            continue
        py = normalize_python(engine(anim))
        conv = None
        for c in glob.glob(os.path.join(CONV_DIR, "*.json")):
            if json.load(open(c)).get("name") == name:
                conv = c
                break
        if conv is None:
            print(f"FAIL {name}: no converted file")
            failures += 1
            continue
        dump = subprocess.run([XTASK, "dump-schedule", conv], capture_output=True, text=True)
        if dump.returncode != 0:
            print(f"FAIL {name}: dump-schedule: {dump.stderr.strip()}")
            failures += 1
            continue
        rs = normalize_rust([l for l in dump.stdout.splitlines() if l.strip()])
        problem = diff(name, py, rs, tol_ms)
        if problem:
            print(f"FAIL {name}: {problem}")
            failures += 1
        else:
            print(f"ok   {name} ({len(py)} frames)")
    print(f"\n{failures} failures")
    sys.exit(1 if failures else 0)

main()
