#!/usr/bin/env python3
"""The training run of a profile-guided build (scripts/pgo.sh).

    scripts/pgo-train.py INSTRUMENTED_NEOSCAD WORK_DIR

Runs an instrumented `neoscad` over the workloads the release binary is
measured on, so the profile weights the code they execute: every model of
conformance/bench.json exported to STL (four also as `--render` PNGs), the
`eval_only` metric's BOSL2 tests exported to `.echo`, every 8th BOSL2
documentation example to STL, OpenSCAD's `examples/Basics` and
`examples/Functions` to STL and CSG, `snapshot`, `check` and `measure` on
four models, a served edit loop (`neoscad serve` on stdio: open, then
edits alternating render and snapshot), and one deep-recursion model
(`DEEP`) to `.echo` and STL.

The deep-recursion model is there for the heap evaluator: past 8 nested
native calls (`NATIVE_CALLS` in crates/eval/src/heap_expr.rs), function
calls and the expressions around them run on a heap stack, and statements
always do (crates/eval/src/heap.rs). No bench model and few corpus files
recurse that deep, so without it the heap's call loop would be trained
almost only by statements and would be laid out as cold code.

The training is deterministic in what it runs: fixed inputs, fixed sizes,
no unseeded `rands()` in the models written here. (Some BOSL2 tests call
`rands()` unseeded, which the command line seeds from the clock, and
parallel runs interleave differently; both move counters a little, not
which code runs.)

What it leaves out is deliberate: the odd-numbered BOSL2 examples, the
other BOSL2 `examples/` files and OpenSCAD's `examples/Old`, `Advanced`
and `Parametric` are the held-out set that shows the profile is not
overfitted to what it was trained on (perf-opportunities.md, P2).

Every run is bounded: neoscad's own `--limit memory=2G` and `time=120`,
plus a wall-clock timeout, so a training pass cannot take the machine's
memory. The profile is only as good as the runs are ordinary, so a run
that fails is reported, not fatal: a model that stops early still
exercises the parser and evaluator on the way.
"""
import json
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REF = REPO / ".reference"
LIMITS = ["--no-server", "--limit", "memory=2G", "--limit", "time=120"]

# Recursion past the native call levels, so the heap evaluator's loops are
# profiled: non-tail calls (`sum`), branching calls (`fib`), calls through
# `let`, a comprehension, `each` and a function literal, a recursion through
# a range's bounds (a shape that stays native and starts a nested heap
# loop per level), and module recursion through a transform, `children()`
# and a block. Fixed sizes, well inside the default depth limit and the
# training's time and memory limits (about 0.2 s natively); the `for`
# varies the arguments so that every round evaluates afresh.
DEEP = """\
function sum(n) = n == 0 ? 0 : n + sum(n - 1);
function fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2);
function nest(n) = n == 0 ? 0 : let(a = nest(n - 1)) max(a, 0) + 1;
function lc(n) = n == 0 ? 0 : [for (i = [0:0]) lc(n - 1) + 1][0];
function ev(n) = n == 0 ? [] : [each ev(n - 1), n % 7];
lit = function(n) n == 0 ? 0 : 1 + lit(n - 1);
function rr(n) = n == 0 ? 0 : len([for (i = [0 : rr(n - 1)]) i]);
for (k = [0 : 9])
    echo(sum(20000 + k), fib(22 + k % 3), nest(10000 + k), lc(10000 + k),
         len(ev(3000 + k)), lit(10000 + k), rr(300 + k));
module chain(n) { if (n > 0) translate([0, 0, 0.01]) chain(n - 1); else cube(1); }
module kids(n) { if (n > 0) kids(n - 1) children(); else children(); }
module tower(n) { if (n > 0) { cube([1, 1, sum(20) / 210]); translate([0, 0, 1]) tower(n - 1); } }
chain(2000);
kids(500) sphere(1);
translate([5, 0, 0]) tower(40);
"""


def main():
    neoscad, work = sys.argv[1], Path(sys.argv[2])
    models, out = work / "models", work / "out"
    models.mkdir(parents=True, exist_ok=True)
    out.mkdir(parents=True, exist_ok=True)
    for need in ["openscad/examples", "BOSL2/tests_x", "BOSL2/examples_x"]:
        if not (REF / need).is_dir():
            sys.exit(f"pgo-train: .reference/{need} is missing; see CLAUDE.md "
                     "(reference checkout) and `conformance bosl2-corpus`")
    env = dict(os.environ, OPENSCADPATH=str(REF))
    # As `conformance bench` runs them: each binary with its bundled fonts.
    env.pop("NEOSCAD_FONT_DIR", None)
    env.pop("OPENSCAD_FONT_PATH", None)

    def run(cmd, timeout=600):
        try:
            return subprocess.run(cmd, cwd=models, env=env, timeout=timeout,
                                  stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL).returncode
        except subprocess.TimeoutExpired:
            return "timeout"

    bench = json.loads((REPO / "conformance/bench.json").read_text())
    files = {}
    for mid, m in bench["models"].items():
        if "file" in m:
            files[mid] = Path(m["file"].replace("{REF}", str(REF / "openscad"))
                              .replace("{BOSL2}", str(REF / "BOSL2")))
        else:
            files[mid] = models / f"{mid}.scad"
            files[mid].write_text(m["source"])
        for name, src in m.get("inputs", {}).items():
            # Generated by the instrumented binary itself: import_stl's
            # 21 MB sphere is a fair piece of training too.
            if not (models / name).exists():
                gen = models / f"gen_{name}.scad"
                gen.write_text(src)
                run([neoscad, *LIMITS, "-o", str(models / name), str(gen)])

    jobs = []
    for mid, f in files.items():
        jobs.append([neoscad, *LIMITS, "-o", str(out / f"{mid}.stl"), str(f)])
    for mid in ["ex_csg_basic", "csg_spheres", "bosl_gears__003", "mink_nonconvex"]:
        jobs.append([neoscad, *LIMITS, "--render", "-o", str(out / f"{mid}.png"), str(files[mid])])
    for f in sorted((REF / "BOSL2/tests_x").glob("*.scad")):
        jobs.append([neoscad, *LIMITS, "-o", str(out / f"t_{f.stem}.echo"), str(f)])
    for f in sorted((REF / "BOSL2/examples_x").glob("*.scad"))[0::8]:
        jobs.append([neoscad, *LIMITS, "-o", str(out / f"x_{f.stem}.stl"), str(f)])
    for sub in ["Basics", "Functions"]:
        for f in sorted((REF / "openscad/examples" / sub).glob("*.scad")):
            jobs.append([neoscad, *LIMITS, "-o", str(out / f"o_{f.stem}.stl"), str(f)])
            jobs.append([neoscad, *LIMITS, "-o", str(out / f"o_{f.stem}.csg"), str(f)])
    deep = models / "deep_recursion.scad"
    deep.write_text(DEEP)
    jobs.append([neoscad, *LIMITS, "-o", str(out / "deep_recursion.echo"), str(deep)])
    jobs.append([neoscad, *LIMITS, "-o", str(out / "deep_recursion.stl"), str(deep)])
    for f in [files["ex_csg_basic"], files["bosl_gears__003"], files["csg_spheres"],
              REF / "openscad/examples/Basics/LetterBlock.scad"]:
        jobs.append([neoscad, "snapshot", "-o", str(out / f"snap_{f.stem}.png"), str(f)])
        jobs.append([neoscad, "check", str(f)])
        jobs.append([neoscad, "measure", str(f)])

    # Parallel runs are fine: the profile runtime merges counters from
    # concurrent processes into the same file (`%m` in the raw file name),
    # and only the counts' proportions matter.
    workers = min(6, os.cpu_count() or 1)
    with ThreadPoolExecutor(workers) as pool:
        codes = list(pool.map(run, jobs))
    # Exit 1 is a model error (a BOSL2 example that is 2D, say); anything
    # else is worth a look.
    odd = [(c, j[-1]) for c, j in zip(codes, jobs) if c not in (0, 1)]
    print(f"pgo-train: {len(jobs)} runs, {len(odd)} unexpected exits")
    for c, f in odd[:20]:
        print(f"  {c}: {f}")
    # A few odd exits are noise; most of them means the instrumented
    # binary itself is broken, and its profile with it. On Windows ARM64
    # every run died with 0xC0000005 (access violation) and llvm-profdata
    # then failed with "no profile can be merged"; stopping here names the
    # cause instead.
    if len(odd) > len(jobs) // 2:
        sys.exit(f"pgo-train: {len(odd)} of {len(jobs)} runs exited abnormally; "
                 "the instrumented binary does not work on this target")
    serve(neoscad, models, out, env)


def serve(neoscad, models, out, env):
    """The served edit loop (docs/serve-protocol.md), as `edit_loop` drives it."""
    src = ('include <BOSL2/std.scad>\ncuboid([40,30,12], rounding=2, edges="Z");\n'
           "right(35) cyl(h=20, d=10, chamfer=1);\n")
    p = subprocess.Popen([neoscad, "serve"], cwd=models, env=env, stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def send(msg):
        body = json.dumps(msg)
        p.stdin.write(f"Content-Length: {len(body)}\r\n\r\n{body}".encode())
        p.stdin.flush()

    def call(i, method, params):
        send({"jsonrpc": "2.0", "id": i, "method": method, "params": params})
        while True:
            n = 0
            while True:
                line = p.stdout.readline().decode()
                if not line:
                    raise RuntimeError("neoscad serve exited")
                if line.startswith("Content-Length:"):
                    n = int(line.split(":")[1])
                if line in ("\r\n", "\n"):
                    break
            m = json.loads(p.stdout.read(n))
            if m.get("id") == i:
                return m

    path = str(models / "serve.scad")
    call(1, "initialize", None)
    call(2, "open", {"path": path, "text": src})
    for k in range(8):
        call(10 + 2 * k, "update", {"path": path, "text": src.replace("12]", f"{13 + k}]")})
        call(11 + 2 * k, "render" if k % 2 else "snapshot",
             {"path": path, "output": str(out / "serve.png"), "progress": False})
    send({"jsonrpc": "2.0", "method": "exit"})
    p.wait(timeout=60)
    print(f"pgo-train: serve edit loop done, exit {p.returncode}")


if __name__ == "__main__":
    main()
