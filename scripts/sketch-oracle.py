#!/usr/bin/env python3
"""Differential oracle for the sketch solver (crates/sketch): solves the
same sketches with SolveSpace's solver and compares the verdicts (solved,
degrees of freedom, redundant, inconsistent) and the solutions.

A development tool, not part of any build or CI job. It needs SolveSpace's
Python binding, `slvs` on PyPI (GPL-3.0-or-later), whose wheels stop at
CPython 3.13, in a scratch environment outside the repository:

    uv venv -p 3.13 "$TMPDIR/slvs-venv" && uv pip install -p "$TMPDIR/slvs-venv"/bin/python slvs
    "$TMPDIR/slvs-venv"/bin/python scripts/sketch-oracle.py --corpus
    "$TMPDIR/slvs-venv"/bin/python scripts/sketch-oracle.py --generate 300 --seed 1

--corpus runs the validation corpus (crates/sketch/tests/corpus);
--generate N makes N random sketches whose constraints are measured from a
random true configuration (so they are consistent), some with a constraint
dropped (under-constrained), duplicated (redundant) or falsified
(inconsistent), drawn with noise around the truth. Both solvers start from
the same drawing. Our side runs through
`cargo run --release -p sketch-solver --example corpus`.

SolveSpace has no signed axis distances, sweeps, circle tangents or
tangency away from a shared endpoint; cases using them are counted as
unsupported. Its angle constraint is unsigned (ours is signed), and its
under-constrained solutions differ from ours by design (both keep free
coordinates near the drawing, by different measures), so positions are
compared only where both report 0 degrees of freedom.
"""

import argparse
import json
import math
import os
import random
import subprocess
import sys
import tempfile

import slvs
from slvs import ConstraintType as CT

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
G = 2


class Unsupported(Exception):
    pass


def solve_slvs(case, start):
    """Solve `case` with SolveSpace from the points in `start`."""
    slvs.clear_sketch()
    # The workplane and the arcs' normal in group 1, which is solved first
    # and then held, as in SolveSpace's own pydemo; the sketch in group 2.
    wp = slvs.add_base_2d(1)
    normal = slvs.add_normal_3d(1, *slvs.make_quaternion(1, 0, 0, 0, 1, 0))
    ents, kinds, ends = {}, {}, {}
    for e in case["entities"]:
        name, kind = e[0], e[1]
        kinds[name] = kind
        if kind == "point":
            x, y = start.get(name, e[2])
            ents[name] = slvs.add_point_2d(G, x, y, wp)
        elif kind == "line":
            ents[name] = slvs.add_line_2d(G, ents[e[2]], ents[e[3]], wp)
            ends[name] = (e[2], e[3])
        elif kind == "arc":
            if len(e) > 5 and e[5] == "cw":
                raise Unsupported("clockwise arc")
            nm = normal
            ents[name] = slvs.add_arc(G, nm, ents[e[2]], ents[e[3]], ents[e[4]], wp)
            ends[name] = (e[3], e[4])
        elif kind == "circle":
            nm = normal
            d = slvs.add_distance(G, e[3], wp)
            ents[name] = slvs.add_circle(G, nm, ents[e[2]], d, wp)
    # Coincident classes, to find shared ends for tangency.
    cls = {n: n for n in kinds}

    def find(n):
        while cls[n] != n:
            n = cls[n]
        return n

    for c in case["constraints"]:
        if c[0] == "coincident":
            cls[find(c[1])] = find(c[2])
    E = slvs.E_NONE
    for c in case["constraints"]:
        k, a = c[0], c[1:]
        if k == "fix":
            if kinds[a[0]] == "line":
                for p in ends[a[0]]:
                    slvs.dragged(G, ents[p], wp)
            else:
                if len(a) > 1:
                    p = ents[a[0]]
                    slvs.set_param_value(p["param"][0], a[1][0])
                    slvs.set_param_value(p["param"][1], a[1][1])
                slvs.dragged(G, ents[a[0]], wp)
        elif k == "coincident":
            slvs.coincident(G, ents[a[0]], ents[a[1]], wp)
        elif k == "on":
            t = CT.PT_ON_LINE if kinds[a[1]] == "line" else CT.PT_ON_CIRCLE
            slvs.add_constraint(G, t, wp, 0.0, ents[a[0]], E, ents[a[1]])
        elif k in ("horizontal", "vertical"):
            t = CT.HORIZONTAL if k == "horizontal" else CT.VERTICAL
            if len(a) == 1:
                slvs.add_constraint(G, t, wp, 0.0, E, E, ents[a[0]])
            else:
                slvs.add_constraint(G, t, wp, 0.0, ents[a[0]], ents[a[1]])
        elif k == "parallel":
            slvs.parallel(G, ents[a[0]], ents[a[1]], wp)
        elif k == "perpendicular":
            slvs.perpendicular(G, ents[a[0]], ents[a[1]], wp)
        elif k == "distance":
            if kinds[a[1]] == "line" and kinds[a[0]] == "point":
                # SolveSpace's distance is signed, negative to the left of
                # the line (constrainteq.cpp, PointLineDistance); ours keeps
                # the drawn side. Give it the drawn side's sign.
                pa, pb = start[ends[a[1]][0]], start[ends[a[1]][1]]
                pp = start[a[0]]
                left = (pb[0] - pa[0]) * (pp[1] - pa[1]) - (pb[1] - pa[1]) * (pp[0] - pa[0]) >= 0
                value = -abs(a[2]) if left else abs(a[2])
                slvs.add_constraint(G, CT.PT_LINE_DISTANCE, wp, value, ents[a[0]], E, ents[a[1]])
            elif kinds[a[0]] == "point":
                slvs.distance(G, ents[a[0]], ents[a[1]], a[2], wp)
            else:
                raise Unsupported("line-line distance")
        elif k in ("distance_x", "distance_y"):
            # No signed axis distance in SolveSpace: a helper point h level
            # with one end and plumb with the other, at the distance from
            # it (2 unknowns, 3 equations: the same net count). The sign is
            # left to the drawing, as SolveSpace's own dimensions are.
            pa, pb = ents[a[0]], ents[a[1]]
            H, V = CT.HORIZONTAL, CT.VERTICAL
            if a[2] == 0:
                slvs.add_constraint(G, V if k == "distance_x" else H, wp, 0.0, pa, pb)
            else:
                sa, sb = start[a[0]], start[a[1]]
                hx, hy = (sb[0], sa[1]) if k == "distance_x" else (sa[0], sb[1])
                h = slvs.add_point_2d(G, hx, hy, wp)
                first, second = (H, V) if k == "distance_x" else (V, H)
                slvs.add_constraint(G, first, wp, 0.0, pa, h)
                slvs.add_constraint(G, second, wp, 0.0, h, pb)
                slvs.distance(G, pa, h, abs(a[2]), wp)
        elif k == "length":
            p, q = ends[a[0]]
            slvs.distance(G, ents[p], ents[q], a[1], wp)
        elif k == "radius":
            slvs.diameter(G, ents[a[0]], 2 * a[1])
        elif k == "diameter":
            slvs.diameter(G, ents[a[0]], a[1])
        elif k == "angle":
            # Unsigned: the drawing decides the side.
            slvs.angle(G, ents[a[0]], ents[a[1]], abs(a[2]), wp, False)
        elif k == "equal":
            slvs.equal(G, ents[a[0]], ents[a[1]], wp)
        elif k == "midpoint":
            slvs.midpoint(G, ents[a[0]], ents[a[1]], wp)
        elif k == "symmetric":
            if kinds[a[2]] != "line":
                raise Unsupported("symmetry about a point")
            slvs.add_constraint(G, CT.SYMMETRIC_LINE, wp, 0.0, ents[a[0]], ents[a[1]], ents[a[2]])
        elif k == "tangent":
            x, y = a
            if "circle" in (kinds[x], kinds[y]):
                raise Unsupported("tangent circle")
            shared = None
            for i, p in enumerate(ends[x]):
                for j, q in enumerate(ends[y]):
                    if shared is None and find(p) == find(q):
                        shared = (i, j)
            if shared is None:
                raise Unsupported("tangency away from a shared endpoint")
            if kinds[x] == "line" or kinds[y] == "line":
                arc, line, side = (y, x, shared[1]) if kinds[x] == "line" else (x, y, shared[0])
                slvs.add_constraint(G, CT.ARC_LINE_TANGENT, wp, 0.0, E, E, ents[arc], ents[line], other=side)
            else:
                slvs.add_constraint(
                    G, CT.CURVE_CURVE_TANGENT, wp, 0.0, E, E, ents[x], ents[y], other=shared[0], other2=shared[1]
                )
        else:
            raise Unsupported(k)
    slvs.solve_sketch(1, False)
    # With failed constraints asked for, the binding returns (result, failed).
    r, _failed = slvs.solve_sketch(G, True)
    pts = {}
    for name, kind in kinds.items():
        if kind == "point":
            p = ents[name]["param"]
            pts[name] = [slvs.get_param_value(p[0]), slvs.get_param_value(p[1])]
    return r, pts


# --- Generated sketches ------------------------------------------------------


def angle_deg(u, v):
    return math.degrees(math.atan2(u[0] * v[1] - u[1] * v[0], u[0] * v[0] + u[1] * v[1]))


def sub(a, b):
    return (a[0] - b[0], a[1] - b[1])


def dist(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


class Gen:
    def __init__(self, rng, name):
        self.rng = rng
        self.truth = {}
        self.entities = []
        self.constraints = []
        self.name = name
        self.lines = {}

    def pt(self, name, p):
        self.truth[name] = p
        self.entities.append([name, "point", list(p)])
        return name

    def line(self, name, a, b):
        self.entities.append([name, "line", a, b])
        self.lines[name] = (a, b)
        return name

    def c(self, *args):
        self.constraints.append(list(args))

    def length(self, l):
        a, b = self.lines[l]
        return dist(self.truth[a], self.truth[b])

    def direction(self, l):
        a, b = self.lines[l]
        return sub(self.truth[b], self.truth[a])

    def case(self, noise, variant):
        size = max(
            max(p[0] for p in self.truth.values()) - min(p[0] for p in self.truth.values()),
            max(p[1] for p in self.truth.values()) - min(p[1] for p in self.truth.values()),
            1.0,
        )
        fixed = {c[1] for c in self.constraints if c[0] == "fix"}
        for name in list(fixed):
            fixed.update(self.lines.get(name, ()))
        for e in self.entities:
            if e[1] == "point" and e[0] not in fixed:
                e[2] = [e[2][0] + self.rng.gauss(0, noise * size), e[2][1] + self.rng.gauss(0, noise * size)]
        return {
            "name": f"generated/{self.name}/{variant}/noise{noise}",
            "entities": self.entities,
            "construction": [],
            "constraints": self.constraints,
            "truth": {k: list(v) for k, v in self.truth.items()},
            "variant": variant,
            "expect": {"status": "solved" if variant != "inconsistent" else "not_converged"},
        }


def gen_polygon(rng, i):
    n = rng.randint(3, 8)
    R = 10 ** rng.uniform(0, 2.5)
    angs = sorted(rng.uniform(0, 2 * math.pi) for _ in range(n))
    pts = [(R * rng.uniform(0.5, 1.0) * math.cos(a), R * rng.uniform(0.5, 1.0) * math.sin(a)) for a in angs]
    # Turn so the first edge is horizontal.
    d = sub(pts[1], pts[0])
    t = -math.atan2(d[1], d[0])
    pts = [(p[0] * math.cos(t) - p[1] * math.sin(t), p[0] * math.sin(t) + p[1] * math.cos(t)) for p in pts]
    g = Gen(rng, f"polygon{i}")
    names = [g.pt(f"p{k}", p) for k, p in enumerate(pts)]
    ls = [g.line(f"e{k}", names[k], names[(k + 1) % n]) for k in range(n)]
    g.c("fix", names[0])
    g.c("horizontal", ls[0])
    for k in range(n - 1):
        g.c("length", ls[k], g.length(ls[k]))
    for k in range(1, n - 1):
        g.c("angle", ls[k - 1], ls[k], angle_deg(g.direction(ls[k - 1]), g.direction(ls[k])))
    return g, ls


def gen_rect_circles(rng, i):
    w, h = rng.uniform(5, 100), rng.uniform(5, 100)
    ox, oy = rng.uniform(-50, 50), rng.uniform(-50, 50)
    g = Gen(rng, f"rect{i}")
    c = [g.pt(f"c{k}", p) for k, p in enumerate([(ox, oy), (ox + w, oy), (ox + w, oy + h), (ox, oy + h)])]
    ls = [g.line(f"s{k}", c[k], c[(k + 1) % 4]) for k in range(4)]
    g.c("fix", c[0])
    g.c("horizontal", ls[0])
    g.c("vertical", ls[1])
    g.c("horizontal", ls[2])
    g.c("vertical", ls[3])
    g.c("length", ls[0], w)
    g.c("length", ls[1], h)
    # A circle centred on the diagonal's midpoint, through a point on the
    # bottom edge's midpoint.
    diag = g.line("diag", c[0], c[2])
    m = g.pt("m", (ox + w / 2, oy + h / 2))
    g.c("midpoint", m, diag)
    r = rng.uniform(0.1, 0.45) * min(w, h)
    g.entities.append(["circ", "circle", m, r])
    g.c("diameter", "circ", 2 * r)
    q = g.pt("q", (ox + w / 2 + r * math.cos(0.7), oy + h / 2 + r * math.sin(0.7)))
    g.c("on", q, "circ")
    g.c("distance", q, ls[0], dist((0, oy), (0, g.truth[q][1])))
    return g, ls


def gen_slot(rng, i):
    L, W = rng.uniform(5, 100), rng.uniform(1, 60)
    g = Gen(rng, f"slot{i}")
    c1 = g.pt("c1", (0.0, 0.0))
    c2 = g.pt("c2", (L, 0.0))
    axis = g.line("axis", c1, c2)
    ts, te = g.pt("ts", (0.0, W / 2)), g.pt("te", (L, W / 2))
    bs, be = g.pt("bs", (L, -W / 2)), g.pt("be", (0.0, -W / 2))
    top, bot = g.line("top", ts, te), g.line("bot", bs, be)
    g.entities.append(["e1", "arc", c1, ts, be])
    g.entities.append(["e2", "arc", c2, bs, te])
    g.c("fix", c1)
    g.c("horizontal", axis)
    g.c("length", axis, L)
    g.c("tangent", "e1", top)
    g.c("tangent", "e1", bot)
    g.c("tangent", "e2", top)
    g.c("tangent", "e2", bot)
    g.c("diameter", "e1", W)
    g.c("equal", "e1", "e2")
    return g, [top, bot]


def gen_trapezoid(rng, i):
    a, b, h = rng.uniform(10, 80), rng.uniform(2, 60), rng.uniform(5, 60)
    g = Gen(rng, f"trapezoid{i}")
    o = g.pt("o", (0.0, 0.0))
    up = g.pt("up", (0.0, 1.0))
    axis = g.line("axis", o, up)
    g.c("fix", "axis")
    p = [g.pt("p0", (-a / 2, 0.0)), g.pt("p1", (a / 2, 0.0)), g.pt("p2", (b / 2, h)), g.pt("p3", (-b / 2, h))]
    ls = [g.line(f"t{k}", p[k], p[(k + 1) % 4]) for k in range(4)]
    # Symmetry about the vertical axis already makes both bases horizontal.
    g.c("symmetric", p[0], p[1], axis)
    g.c("symmetric", p[3], p[2], axis)
    g.c("length", ls[0], a)
    g.c("length", ls[2], b)
    g.c("distance", p[2], ls[0], h)
    g.c("on", p[0], g.line("base", o, p[1]))
    return g, ls


def gen_linkage(rng, i):
    # A four-bar linkage closed by a perpendicular and a point on a line.
    g = Gen(rng, f"linkage{i}")
    s = rng.uniform(5, 50)
    a = g.pt("a", (0.0, 0.0))
    b = g.pt("b", (s * rng.uniform(1.5, 3), 0.0))
    t1 = rng.uniform(0.3, 2.5)
    c = g.pt("c", (s * math.cos(t1), s * math.sin(t1)))
    d = g.pt("d", (g.truth[b][0] + s * 0.8, s * rng.uniform(0.5, 1.5)))
    ab, ac, cd, bd = g.line("ab", a, b), g.line("ac", a, c), g.line("cd", c, d), g.line("bd", b, d)
    g.c("fix", a)
    g.c("horizontal", ab)
    g.c("length", ab, g.length(ab))
    g.c("length", ac, g.length(ac))
    g.c("angle", ab, ac, angle_deg(g.direction(ab), g.direction(ac)))
    g.c("length", cd, g.length(cd))
    g.c("length", bd, g.length(bd))
    return g, [ab, ac, cd, bd]


GENERATORS = [gen_polygon, gen_rect_circles, gen_slot, gen_trapezoid, gen_linkage]


def generate(n, seed):
    rng = random.Random(seed)
    cases = []
    for i in range(n):
        gen = GENERATORS[i % len(GENERATORS)]
        g, lines = gen(rng, i)
        variant = rng.choice(["full", "full", "under", "redundant", "inconsistent"])
        dims = [k for k, c in enumerate(g.constraints) if c[0] in ("length", "distance", "angle", "diameter")]
        if variant == "under" and dims:
            del g.constraints[rng.choice(dims)]
        elif variant == "redundant":
            g.constraints.append(list(g.constraints[rng.choice(dims)]))
        elif variant == "inconsistent":
            c = list(g.constraints[rng.choice(dims)])
            c[-1] = c[-1] * 1.1 + (5.0 if c[0] == "angle" else 0.1)
            g.constraints.append(c)
        noise = rng.choice([0.005, 0.02, 0.05, 0.1])
        cases.append(g.case(noise, variant))
    return {"origin": "generated", "commit": "-", "licence": "-", "cases": cases}


# --- Comparison --------------------------------------------------------------


def ours(path):
    env = dict(os.environ, CORPUS_FILE=path) if path else dict(os.environ)
    out = subprocess.run(
        ["cargo", "run", "-q", "--release", "-p", "sketch-solver", "--example", "corpus"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return {r["name"]: r for r in map(json.loads, out.splitlines())}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", action="store_true")
    ap.add_argument("--generate", type=int, default=0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--dump", help="write the generated cases to this file and stop")
    args = ap.parse_args()
    if args.dump:
        with open(args.dump, "w") as f:
            json.dump(generate(args.generate, args.seed), f, indent=1)
        return 0
    files = []
    if args.corpus:
        corpus = os.path.join(ROOT, "crates/sketch/tests/corpus")
        for d in sorted(os.listdir(corpus)):
            dd = os.path.join(corpus, d)
            if os.path.isdir(dd):
                for f in sorted(os.listdir(dd)):
                    if f.endswith(".json"):
                        files.append((os.path.join(dd, f), None))
    if args.generate:
        tmp = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
        json.dump(generate(args.generate, args.seed), tmp)
        tmp.close()
        files.append((tmp.name, tmp.name))
    stats = {}

    def count(key):
        stats[key] = stats.get(key, 0) + 1

    worst_time = 0.0
    times = []
    cache = {}
    for path, env_path in files:
        data = json.load(open(path))
        if env_path not in cache:
            cache[env_path] = ours(env_path)
        results = cache[env_path]
        for case in data["cases"]:
            name = case["name"]
            o = results.get(name)
            if o is None:
                continue
            count("cases")
            times.append(o["seconds"])
            try:
                r, pts = solve_slvs(case, o["start"])
            except Unsupported as e:
                count("unsupported by SolveSpace")
                if args.verbose:
                    print(f"{name}: unsupported ({e})")
                continue
            count("compared")
            ss_ok = r["result"] in (slvs.ResultFlag.OKAY, slvs.ResultFlag.REDUNDANT_OKAY)
            our_ok = o["status"] == "solved"
            line = []
            if o["flipped"] != "[]":
                count("ours solved but reports a flip")
            if ss_ok == our_ok:
                count("same verdict (solved / not)")
            else:
                count("different verdict")
                line.append(f"verdict ours {o['status']} vs SolveSpace {slvs.ResultFlag(r['result']).name}")
            if ss_ok and our_ok:
                if r["dof"] == o["dof"]:
                    count("same dof (both solved)")
                else:
                    count("different dof (both solved)")
                    line.append(f"dof ours {o['dof']} vs SolveSpace {r['dof']}")
                ss_red = r["result"] == slvs.ResultFlag.REDUNDANT_OKAY
                our_red = bool(o["redundant"])
                count("same redundancy verdict" if ss_red == our_red else "different redundancy verdict")
                if ss_red != our_red:
                    line.append(f"redundant ours {o['redundant']} vs SolveSpace {ss_red}")
                if r["dof"] == 0 and o["dof"] == 0 and pts:
                    size = max(1.0, max(abs(v) for p in pts.values() for v in p))
                    d = max(
                        max(abs(pts[n][0] - p[0]), abs(pts[n][1] - p[1]))
                        for n, p in o["points"].items()
                        if n in pts and isinstance(p, list)
                    )
                    if d <= 1e-6 * size:
                        count("same solution (both 0 dof)")
                    else:
                        count("different solution (both 0 dof)")
                        line.append(f"solutions differ by {d:.3g}")
                    truth = case.get("truth")
                    if truth:
                        tsize = max(1.0, max(abs(v) for p in truth.values() for v in p))
                        od = max(abs(o["points"][n][k] - truth[n][k]) for n in truth if n in o["points"] for k in (0, 1))
                        sd = max(abs(pts[n][k] - truth[n][k]) for n in truth if n in pts for k in (0, 1))
                        count("ours at the generating truth" if od <= 1e-6 * tsize else "ours elsewhere than the generating truth")
                        count("SolveSpace at the generating truth" if sd <= 1e-6 * tsize else "SolveSpace elsewhere than the generating truth")
                        if od > 1e-6 * tsize:
                            line.append(f"ours {od:.3g} from the truth")
            if not ss_ok and not our_ok:
                ss_inc = r["result"] == slvs.ResultFlag.INCONSISTENT
                our_conf = bool(o["conflicts"])
                count("both inconsistent" if ss_inc and our_conf else "both failed, different diagnosis")
            if line and args.verbose:
                print(f"{name}: " + "; ".join(line))
    for k in sorted(stats):
        print(f"{stats[k]:6d}  {k}")
    if times:
        times.sort()
        print(f"our solve time: median {times[len(times) // 2] * 1e6:.0f} µs, max {times[-1] * 1e6:.0f} µs")


if __name__ == "__main__":
    sys.exit(main())
