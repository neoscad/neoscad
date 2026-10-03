#!/usr/bin/env python3
"""Independent STL grader for the agent CAD comparison (docs/agent-eval.md).

    grade.py --task T1 bracket=out/bracket.stl
    grade.py --task T2 base=out/base.stl lid=out/lid.stl
    grade.py --task T3 adapter=out/adapter.stl

Prints one JSON object: per part, ModelRift's mesh facts and "clean"
verdict (stlmesh.topology), then the task's dimensional checks. Python
stdlib only, and it imports none of the tools under test.

Every check has `gate` (counts toward pass and toward "silent wrong
geometry") or is information only, and `ok` is True, False or None (None:
the grader could not measure it from the mesh, which is stated in `note`
rather than guessed). The spec wording leaves choices open (plate sizes,
which corners, where the vents go); checks gate only on numbers the spec
states, and each tolerance is documented next to it.
"""

import argparse
import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import stlmesh as sm  # noqa: E402

PARTS = {"T0": ["plate"], "T1": ["bracket"], "T2": ["base", "lid"], "T3": ["adapter"],
         "T4": ["body", "lid"], "T5": ["knob"], "T6": ["plate", "large_gear", "small_gear"]}
SQ2M1 = math.sqrt(2) - 1


def check(name, ok, value=None, expected=None, gate=True, note=None):
    c = {"name": name, "gate": gate, "ok": ok, "value": value, "expected": expected}
    if note:
        c["note"] = note
    return c


def r3(x):
    if x is None or isinstance(x, (bool, str)):
        return x
    if isinstance(x, dict):
        return {k: r3(v) for k, v in x.items()}
    if isinstance(x, (list, tuple)):
        return [r3(y) for y in x]
    return round(x, 3)


def within(x, lo, hi):
    return x is not None and lo <= x <= hi


class Part:
    """A mesh with lazily built ray and slice indexes."""

    def __init__(self, verts, tris):
        self.verts, self.tris = verts, tris
        self._rays, self._slices = {}, {}
        xs = list(zip(*verts)) if verts else [[0], [0], [0]]
        self.bmin = [min(c) for c in xs]
        self.bmax = [max(c) for c in xs]

    def ray(self, axis):
        if axis not in self._rays:
            self._rays[axis] = sm.RayIndex(self.verts, self.tris, axis)
        return self._rays[axis]

    def slicer(self, axis):
        if axis not in self._slices:
            self._slices[axis] = sm.SliceIndex(self.verts, self.tris, axis)
        return self._slices[axis]

    def intervals(self, axis, p):
        """Inside intervals along `axis` through point p (p[axis] ignored)."""
        u, v = sm._axes(axis)
        return self.ray(axis).intervals(p[u], p[v])

    def section(self, axis, value):
        return self.slicer(axis).section(value)

    def transformed(self, fn):
        return Part([fn(p) for p in self.verts], self.tris)


def rot_x180(p):
    """A proper rotation (keeps winding and handedness): turn upside down."""
    return (p[0], -p[1], -p[2])


def to_z(axis):
    """The cyclic permutation taking `axis` to z: a proper rotation, so a
    right-hand thread stays right-hand."""
    return lambda p: (p[(axis + 1) % 3], p[(axis + 2) % 3], p[axis])


def lin(a, b, n):
    return [a + (b - a) * i / (n - 1) for i in range(n)] if n > 1 else [(a + b) / 2]


def classify_loops(loops):
    outer = [l for l in loops if sm.poly_area(l) > 0]
    holes = [l for l in loops if sm.poly_area(l) < 0]
    return outer, holes


# ---------------------------------------------------------------------------
# T0: the dry run's trivial part, a 20 x 10 x 4 plate with a 3 mm hole


def grade_t0(parts):
    p = parts["plate"]
    size = [p.bmax[i] - p.bmin[i] for i in range(3)]
    loops, _ = p.section(2, (p.bmin[2] + p.bmax[2]) / 2)
    _, holes = classify_loops(loops)
    d = [sm.equiv_diameter(h) for h in holes]
    return [
        check("size 20 x 10 x 4 (+-0.05)", all(abs(a - b) <= 0.05 for a, b in zip(size, (20, 10, 4))),
              r3(size), [20, 10, 4]),
        check("one hole of diameter 3 (+-0.15)", len(d) == 1 and abs(d[0] - 3) <= 0.15, r3(d), 3),
    ]


# ---------------------------------------------------------------------------
# T1: L-bracket


def grade_t1(parts):
    p = parts["bracket"]
    out = []
    # The two plates' outer faces are the bbox faces that rays along the
    # axis hit at the very extreme most often; the third axis runs along
    # the L's corner edge.
    cov = {}
    for a in range(3):
        u, v = sm._axes(a)
        lo = hi = tot = 0
        for pu in lin(p.bmin[u], p.bmax[u], 32)[1:-1]:
            for pv in lin(p.bmin[v], p.bmax[v], 32)[1:-1]:
                q = [0, 0, 0]
                q[u], q[v] = pu, pv
                iv = p.intervals(a, q)
                tot += 1
                if iv and abs(iv[0][0] - p.bmin[a]) < 0.05:
                    lo += 1
                if iv and abs(iv[-1][1] - p.bmax[a]) < 0.05:
                    hi += 1
        cov[a] = (lo / tot, hi / tot)
    order = sorted(range(3), key=lambda a: -max(cov[a]))
    plates, e = order[:2], order[2]
    info = {}
    for a in plates:
        lo = cov[a][0] >= cov[a][1]
        info[a] = {"outer": p.bmin[a] if lo else p.bmax[a], "inward": 1 if lo else -1,
                   "far": p.bmax[a] if lo else p.bmin[a]}
    out.append(check("plate axes found", True, {"plates": plates, "edge_axis": e,
                     "face_coverage": {a: r3(cov[a]) for a in range(3)}}, gate=False,
                     note="axes whose bbox face is most covered by a flat outer face"))

    # Plate thickness: rays along the plate normal that start on the outer
    # face; most cross only the plate, so the median is its thickness.
    # Tolerance +-0.1: plates are flat, so any faceting error is zero.
    thick = {}
    for a in plates:
        b = [x for x in plates if x != a][0]
        lens = []
        for pb in lin(p.bmin[b], p.bmax[b], 40)[1:-1]:
            for pe in lin(p.bmin[e], p.bmax[e], 40)[1:-1]:
                q = [0, 0, 0]
                q[b], q[e] = pb, pe
                for s, t in p.intervals(a, q):
                    if abs((s if info[a]["inward"] > 0 else t) - info[a]["outer"]) < 0.05:
                        lens.append(t - s)
        thick[a] = sm.median(lens)
        info[a]["T"] = thick[a] or 4
    out.append(check("both plates 4 thick (+-0.1)", all(within(t, 3.9, 4.1) for t in thick.values()),
                     r3(list(thick.values())), 4))

    # Holes: hole loops in each plate's mid-plane section; countersinks
    # from sections 0.3 and 0.8 mm inside each face.
    holes = []
    for a in plates:
        o, inw, T = info[a]["outer"], info[a]["inward"], info[a]["T"]
        loops, _ = p.section(a, o + inw * T / 2)
        _, hl = classify_loops(loops)
        for h in hl:
            d = sm.equiv_diameter(h)
            if not 1.5 <= d <= 15:
                continue
            c = sm.poly_centroid(h)

            def dia_at(depth_from_outer):
                ls, _ = p.section(a, o + inw * depth_from_outer)
                best = None
                for l in ls:
                    if sm.poly_area(l) < 0:
                        cc = sm.poly_centroid(l)
                        if math.hypot(cc[0] - c[0], cc[1] - c[1]) < d / 2:
                            best = sm.equiv_diameter(l)
                return best

            faces = {"outer": (dia_at(0.3), dia_at(0.8)), "inner": (dia_at(T - 0.3), dia_at(T - 0.8))}
            cs = None
            for face, (d1, d2) in faces.items():
                if d1 and d2 and d1 - d > 1.0 and (cs is None or d1 > cs["d_0.3"]):
                    slope = (d1 - d2) / 0.5  # diameter change per mm of depth
                    cs = {"face": face, "d_0.3": d1, "head_diameter": d1 + slope * 0.3,
                          "angle_deg": 2 * math.degrees(math.atan(slope / 2))}
            u, v = sm._axes(a)
            q = [0, 0, 0]
            q[u], q[v] = c
            clear = not p.intervals(a, q)
            holes.append({"plate_axis": a, "center": r3(c), "diameter": r3(d), "countersink": cs,
                          "line_of_sight_clear": clear})
    cs = [h for h in holes if h["countersink"]]
    plain = [h for h in holes if not h["countersink"]]
    out.append(check("2 countersunk + 2 plain holes", len(cs) == 2 and len(plain) == 2,
                     {"countersunk": len(cs), "plain": len(plain),
                      "holes": [{**h, "countersink": h["countersink"] and {k: r3(v) if k != "face" else v
                                 for k, v in h["countersink"].items()}} for h in holes]}, "2 + 2"))
    if cs:
        # +-0.4 on the head diameter: extrapolating the cone to the surface
        # from two sections; +-8 degrees on the angle for faceted cones.
        out.append(check("countersink head 9 (+-0.4) at 90 deg (+-8)",
                         all(within(h["countersink"]["head_diameter"], 8.6, 9.4)
                             and within(h["countersink"]["angle_deg"], 82, 98) for h in cs),
                         [(r3(h["countersink"]["head_diameter"]), r3(h["countersink"]["angle_deg"])) for h in cs],
                         [9, 90]))
        out.append(check("countersinks open on the inner face (screw heads inside the L)",
                         all(h["countersink"]["face"] == "inner" for h in cs),
                         [h["countersink"]["face"] for h in cs], "inner"))
    else:
        out.append(check("countersink head 9 at 90 deg", False, None, [9, 90], note="no countersink found"))
    out.append(check("every hole has a clear line of sight (screw and driver access)",
                     bool(holes) and all(h["line_of_sight_clear"] for h in holes),
                     [h["line_of_sight_clear"] for h in holes], True))

    # Corners. For a rounded corner of radius r, the sharp corner point is
    # r(sqrt2 - 1) from the arc along the diagonal.
    a1, a2 = plates
    corners = []
    for a in plates:
        b = a2 if a == a1 else a1
        o, inw, T = info[a]["outer"], info[a]["inward"], info[a]["T"]
        loops, _ = p.section(a, o + inw * T / 2)
        outer, _ = classify_loops(loops)
        if not outer:
            continue
        big = max(outer, key=sm.poly_area)
        u, v = sm._axes(a)
        for pe in (p.bmin[e], p.bmax[e]):
            q = [0, 0, 0]
            q[b], q[e] = info[b]["far"], pe
            dist = sm.dist_point_poly((q[u], q[v]), big)
            corners.append({"where": f"free corner of plate {a}", "radius": dist / SQ2M1})
    # Profile sections across the corner edge give the inner fillet and the
    # outer corner of the L.
    u, v = sm._axes(e)
    C = [0, 0, 0]
    inner_r, outer_r = [], []
    for pe in lin(p.bmin[e], p.bmax[e], 21)[1:-1]:
        loops, _ = p.section(e, pe)
        if not loops:
            continue
        I = [0, 0, 0]
        dvec = [0, 0, 0]
        for a in plates:
            C[a] = info[a]["outer"]
            I[a] = info[a]["outer"] + info[a]["inward"] * info[a]["T"]
            dvec[a] = info[a]["inward"] / math.sqrt(2)
        hit = sm.ray2d_first_hit((I[u], I[v]), (dvec[u], dvec[v]), loops)
        if hit is not None:
            inner_r.append(hit / SQ2M1)
        hit = sm.ray2d_first_hit((C[u], C[v]), (dvec[u], dvec[v]), loops)
        if hit is not None:
            outer_r.append(hit / SQ2M1)
    ri = sm.median(inner_r)
    ro = sm.median(outer_r)
    corners.append({"where": "outer corner of the L", "radius": ro})
    # +-0.8: a faceted arc's midpoint sits inside the true arc, and the
    # median over sections along the edge skips the gussets.
    out.append(check("R4 fillet on the inner corner (+-0.8)", within(ri, 3.2, 4.8), r3(ri), 4))
    n3 = sum(1 for c in corners if within(c["radius"], 2.4, 3.6))
    out.append(check("R3 on at least 2 outer corners (+-0.6)", n3 >= 2,
                     [{"where": c["where"], "radius": r3(c["radius"])} for c in corners], 3,
                     note="'outer vertical corners' is ambiguous about which corners; "
                          "this measures the plates' free corners and the L's outer corner"))

    # Gussets: rays along the corner edge through points in the free space
    # 3 mm diagonally beyond the inner corner (clear of an R4 fillet) cross
    # each gusset once. A gusset with legs under 6 mm would be missed.
    counts = {}
    for k in (3, 6, 10, 15):
        q = [0, 0, 0]
        for a in plates:
            q[a] = info[a]["outer"] + info[a]["inward"] * (info[a]["T"] + k)
        counts[k] = len(p.intervals(e, q))
    out.append(check("two gussets", counts[3] == 2, counts, 2,
                     note="solid spans along the corner edge, k mm beyond the inner corner"))
    out.append(check("printable without supports", None, None, None, gate=False,
                     note="not judged from the mesh: bridges and small horizontal holes print "
                          "without supports, so overhang area (in the topology) is only a hint"))
    return out


# ---------------------------------------------------------------------------
# T2: enclosure


def base_cavity(b):
    """Floor top, cavity and wall measurements of an open-top box."""
    cx, cy = (b.bmin[0] + b.bmax[0]) / 2, (b.bmin[1] + b.bmax[1]) / 2
    floor = None
    for dx in (0, 5, -5, 10):
        iv = b.intervals(2, (cx + dx, cy, 0))
        if iv and abs(iv[0][0] - b.bmin[2]) < 0.05 and iv[0][1] < b.bmax[2] - 0.5:
            floor = iv[0][1]
            break
    if floor is None:
        return None
    Lx, Ly = b.bmax[0] - b.bmin[0], b.bmax[1] - b.bmin[1]
    gaps = {0: [], 1: []}
    outs = {0: [], 1: []}
    walls = []
    top = b.bmax[2]
    for z in lin(floor + 0.5, top - 0.5, 6):
        for axis, other, c, L in ((0, 1, cy, Ly), (1, 0, cx, Lx)):
            for f in lin(-0.3, 0.3, 9):
                q = [0, 0, z]
                q[other] = c + f * L
                iv = b.intervals(axis, q)
                if len(iv) >= 2:
                    gaps[axis].append(iv[-1][0] - iv[0][1])
                    outs[axis].append(iv[-1][1] - iv[0][0])
                    walls += [iv[0][1] - iv[0][0], iv[-1][1] - iv[-1][0]]
    return {"floor_top": floor, "cavity": [sm.median(gaps[0]), sm.median(gaps[1])],
            "outer": [sm.median(outs[0]), sm.median(outs[1])], "wall": sm.median(walls)}


def panel_openings(p, axis, at):
    """Through-openings of a flat panel: hole loops in the section at the
    panel's mid-thickness, with their bbox dimensions."""
    loops, _ = p.section(axis, at)
    _, holes = classify_loops(loops)
    res = []
    for h in holes:
        x0, y0, x1, y1 = sm.poly_bbox(h)
        w, t = sorted((x1 - x0, y1 - y0), reverse=True)
        res.append({"axis": axis, "at": r3(at), "size": [r3(w), r3(t)], "center": r3(sm.poly_centroid(h)),
                    "diameter": r3(sm.equiv_diameter(h))})
    return res


# How much longer than wide an opening must be to count as a slot (T2's
# vents; see grade_t2).
SLOT_RATIO = 1.5


def grade_t2(parts):
    out = []
    base, lid = parts.get("base"), parts.get("lid")
    if base is None or lid is None:
        return [check("both parts present", False, sorted(parts), ["base", "lid"])]
    cav = base_cavity(base)
    flipped = False
    if cav is None:
        base = base.transformed(rot_x180)
        cav = base_cavity(base)
        flipped = True
    if cav is None:
        return [check("base has a floor and a cavity", False, None, None,
                      note="no floor found under the middle of the base from either side")]
    out.append(check("base cavity measured", True, {k: r3(v) for k, v in cav.items()} | {"open_side_down": flipped},
                     gate=False))
    # Tolerances: walls +-0.1 and cavity +-0.1 per dimension are flat faces
    # measured with rays (medians over 6 levels x 9 positions skip vents).
    out.append(check("walls 2 (+-0.1)", within(cav["wall"], 1.9, 2.1), r3(cav["wall"]), 2))
    dims = sorted([d for d in cav["cavity"] if d is not None], reverse=True)
    out.append(check("cavity 50.8 x 26.8 (PCB 50 x 26 + 0.4 per side, +-0.1)",
                     len(dims) == 2 and abs(dims[0] - 50.8) <= 0.1 and abs(dims[1] - 26.8) <= 0.1,
                     r3(dims), [50.8, 26.8]))

    # Posts: M2 holes in the section 1 mm above the floor, judged by their
    # circumscribed diameter, the size the model gave them. The area-
    # equivalent diameter of a faceted hole is smaller than the circle its
    # vertices lie on: a 1.5 pilot at $fn = 64 measured 1.499 and failed
    # the 1.5-1.8 window on a correct part. The window's ends allow 0.005
    # for the STL's 32-bit coordinates.
    loops, _ = base.section(2, cav["floor_top"] + 1.0)
    _, holes = classify_loops(loops)
    post_holes = [d for d in (sm.circum_diameter(h) for h in holes) if 1.4 <= d <= 3.7]
    out.append(check("four M2 posts (holes 1.5..3.6 dia, 1 mm above the floor)",
                     len(post_holes) == 4 and all(1.495 <= d <= 3.605 for d in post_holes),
                     r3(post_holes), 4, note="a post counts by its hole's circumscribed diameter: pilot "
                                              "(1.5-1.8), clearance (2.2-2.4) or heat-set insert (3.2-3.6); "
                                              "solid posts are not counted"))

    # Openings: walls at mid-wall, the floor, and the lid's plate.
    w = cav["wall"] or 2
    opens = []
    for axis in (0, 1):
        opens += [dict(o, part="base") for o in panel_openings(base, axis, base.bmin[axis] + w / 2)]
        opens += [dict(o, part="base") for o in panel_openings(base, axis, base.bmax[axis] - w / 2)]
    floor_t = cav["floor_top"] - base.bmin[2]
    opens += [dict(o, part="base") for o in panel_openings(base, 2, base.bmin[2] + floor_t / 2)]

    # Lid: turn it so its flat plate is at the bottom.
    def plate_side(p):
        lo = hi = 0
        for x in lin(p.bmin[0], p.bmax[0], 12)[1:-1]:
            for y in lin(p.bmin[1], p.bmax[1], 12)[1:-1]:
                iv = p.intervals(2, (x, y, 0))
                lo += bool(iv) and abs(iv[0][0] - p.bmin[2]) < 0.05
                hi += bool(iv) and abs(iv[-1][1] - p.bmax[2]) < 0.05
        return lo >= hi

    if not plate_side(lid):
        lid = lid.transformed(rot_x180)
    firsts = []
    for x in lin(lid.bmin[0], lid.bmax[0], 12)[2:-2]:
        for y in lin(lid.bmin[1], lid.bmax[1], 12)[2:-2]:
            iv = lid.intervals(2, (x, y, 0))
            if iv and abs(iv[0][0] - lid.bmin[2]) < 0.05:
                firsts.append(iv[0][1] - iv[0][0])
    plate_t = sm.median(firsts)
    if plate_t:
        opens += [dict(o, part="lid") for o in panel_openings(lid, 2, lid.bmin[2] + plate_t / 2)]
    usb = [o for o in opens if abs(o["size"][0] - 9.5) <= 0.25 and abs(o["size"][1] - 3.5) <= 0.25]
    out.append(check("USB-C cutout 9.5 x 3.5 (+-0.25)", len(usb) >= 1, [o["size"] for o in opens], [9.5, 3.5],
                     note="through-openings of the base walls, floor and lid plate (section bboxes); "
                          "a notch open to the rim is not found"))
    # Vents: the largest group of identical slot-shaped openings, so snap
    # windows or screw holes of another size are not counted as vents. The
    # spec says "slots" and gives no proportion; a slot is an opening
    # clearly longer than it is wide, so 1.5:1 or more counts. Round holes
    # (1:1) and square or hexagonal vents (up to 1.15:1) do not. The rule
    # was 2:1 until a row of five 2.5 x 4.5 vents with pointed roofs, built
    # and described as slots (1.8:1), failed it: a person grading the spec
    # would count those.
    slots = [o for o in opens if o not in usb[:1] and o["size"][1] >= 0.5 and o["size"][0] >= SLOT_RATIO * o["size"][1]]
    groups = []
    for o in slots:
        for g in groups:
            if all(abs(o["size"][i] - g[0]["size"][i]) <= 0.2 for i in (0, 1)):
                g.append(o)
                break
        else:
            groups.append([o])
    best = max(groups, key=len) if groups else []
    out.append(check("five vent slots", len(best) == 5,
                     {"largest_group": len(best), "size": best[0]["size"] if best else None,
                      "where": sorted({o["part"] for o in best}), "all_slots": [o["size"] for o in slots]}, 5,
                     note="slots: through-openings at least 1.5 times as long as wide, grouped by size (+-0.2)"))

    # Lid lip clearance: rings of the lid above its plate, measured with
    # rays at 9 positions per axis on levels 0.2 mm apart, against the
    # base's cavity (lip inside) or outer wall (skirt outside). Each level
    # takes the median over positions, so snap bumps on part of the
    # perimeter do not count. The lip passes when some band of it at least
    # 0.6 mm (three 0.2 mm layers) tall is at 0.2 per side on both axes:
    # that band is what locates the lid. A median over the whole height
    # failed the OpenSCAD lid of cad-20260928T231444Z, whose lip is
    # relieved 0.8 per side to clear the base's snap barbs and has a 1 mm
    # catch band at exactly 0.2 (clearance 1.0 over most of the height).
    res = {0: [], 1: []}
    widths = {0: [], 1: []}
    per_level = []
    kind = None
    if plate_t is not None and lid.bmax[2] - (lid.bmin[2] + plate_t) > 0.8:
        z0 = lid.bmin[2] + plate_t
        Lx, Ly = lid.bmax[0] - lid.bmin[0], lid.bmax[1] - lid.bmin[1]
        cx, cy = (lid.bmin[0] + lid.bmax[0]) / 2, (lid.bmin[1] + lid.bmax[1]) / 2
        zl = lin(z0 + 0.2, lid.bmax[2] - 0.2, max(2, min(40, int((lid.bmax[2] - z0 - 0.4) / 0.2) + 1)))
        for z in zl:
            lvl = {0: [], 1: []}
            for axis, other, c, L in ((0, 1, cy, Ly), (1, 0, cx, Lx)):
                along = lid.bmax[axis] - lid.bmin[axis]
                # Match the lid axis to the base axis of the nearest size.
                bi = min((0, 1), key=lambda i: abs((cav["outer"][i] or 0) - along))
                cavd, outd = cav["cavity"][bi], cav["outer"][bi]
                if cavd is None or outd is None:
                    continue
                for f in lin(-0.3, 0.3, 9):
                    q = [0, 0, z]
                    q[other] = c + f * L
                    iv = lid.intervals(axis, q)
                    if len(iv) < 2:
                        continue
                    n = len(iv) // 2
                    rings = [(iv[k][0], iv[-1 - k][1], iv[k][1], iv[-1 - k][0]) for k in range(n)]
                    inside = [r for r in rings if r[1] - r[0] < cavd]
                    outside = [r for r in rings if r[3] - r[2] > outd]
                    if inside:
                        r = inside[0]
                        lvl[axis].append((cavd - (r[1] - r[0])) / 2)
                        widths[axis].append(r[1] - r[0])
                        kind = kind or "lip inside the walls"
                    elif outside:
                        r = outside[-1]
                        lvl[axis].append(((r[3] - r[2]) - outd) / 2)
                        widths[axis].append(r[3] - r[2])
                        kind = kind or "skirt outside the walls"
            for axis in (0, 1):
                res[axis] += lvl[axis]
            per_level.append((z, sm.median(lvl[0]), sm.median(lvl[1])))
    dz = (per_level[1][0] - per_level[0][0]) if len(per_level) > 1 else 0
    band, best = [], []
    for z, c0, c1 in per_level:
        if within(c0, 0.15, 0.25) and within(c1, 0.15, 0.25):
            band.append((z, c0, c1))
            best = max(best, band, key=len)
        else:
            band = []
    band_mm = len(best) * dz
    cl = [sm.median([b[1] for b in best]), sm.median([b[2] for b in best])] if best else \
        [sm.median(res[0]), sm.median(res[1])]
    out.append(check("lid lip clearance 0.2 per side (+-0.05)", band_mm >= 0.6 - 1e-6,
                     {"clearance_per_axis": r3(cl), "band_mm": r3(band_mm),
                      "band_z_from_plate": r3([best[0][0] - z0, best[-1][0] - z0]) if best else None,
                      "median_over_height": r3([sm.median(res[0]), sm.median(res[1])]),
                      "kind": kind, "lid_plate": r3(plate_t)}, 0.2,
                     note="some band of the lip at least 0.6 mm tall at 0.2 per side on both axes; "
                          "median over positions per level, so local snap bumps do not count"))
    spread = [max(w) - min(w) for w in widths.values() if w]
    out.append(check("snap features on the lip", None, {"lip_width_spread": r3(spread)}, None, gate=False,
                     note="whether snaps work cannot be judged from a mesh; a spread over ~0.3 mm "
                          "shows bumps or cut-outs on the lip"))
    return out


# ---------------------------------------------------------------------------
# T3: threaded hose-barb adapter


def xcorr_lag(a, b, max_lag):
    """Lag L (in samples, |L| <= max_lag) maximising corr(a[i], b[i - L])."""
    n = len(a)
    ma, mb = sum(a) / n, sum(b) / n
    best, best_l = -2.0, 0
    for L in range(-max_lag, max_lag + 1):
        s = sa = sb = 0.0
        for i in range(max(0, L), min(n, n + L)):
            x, y = a[i] - ma, b[i - L] - mb
            s += x * y
            sa += x * x
            sb += y * y
        c = s / math.sqrt(sa * sb) if sa > 0 and sb > 0 else 0
        if c > best:
            best, best_l = c, L
    return best_l, best


# Radius lost per unit of length, away from the hex or thread, from which
# the end of T3's barb is a flare rather than a barb (see grade_t3).
FLARE_SLOPE = 0.5


def grade_t3(parts):
    p0 = parts["adapter"]
    out = []
    size = [p0.bmax[i] - p0.bmin[i] for i in range(3)]
    axis = max(range(3), key=lambda i: size[i])
    p = p0.transformed(to_z(axis))
    cx, cy = (p.bmin[0] + p.bmax[0]) / 2, (p.bmin[1] + p.bmax[1]) / 2
    step = 0.05
    zs = [z for z in lin(p.bmin[2] + 0.02, p.bmax[2] - 0.02, int((p.bmax[2] - p.bmin[2] - 0.04) / step) + 1)]
    R = {0: [], 90: [], 180: [], 270: []}
    chan = []
    closed_levels = 0
    for z in zs:
        ix = p.intervals(0, (0, cy, z))
        iy = p.intervals(1, (cx, 0, z))
        for iv, c, pos, neg in ((ix, cx, 0, 180), (iy, cy, 90, 270)):
            if iv:
                R[pos].append(iv[-1][1] - c)
                R[neg].append(c - iv[0][0])
                left = [s for s in iv if s[1] <= c]
                right = [s for s in iv if s[0] >= c]
                if any(s[0] < c < s[1] for s in iv):
                    closed_levels += 1
                elif left and right:
                    chan.append(right[0][0] - left[-1][1])
            else:
                R[pos].append(0.0)
                R[neg].append(0.0)
    out.append(check("axis", True, {"axis": "xyz"[axis], "length": r3(size[axis])}, gate=False))
    through = not p.intervals(2, (cx, cy, 0))
    dch = sm.median(chan)
    # +-0.2: a faceted bore's diameter measured along x and y sits between
    # its inscribed and circumscribed circles.
    out.append(check("8 through-channel (+-0.2, open at every level)",
                     through and closed_levels == 0 and within(dch, 7.8, 8.2),
                     {"median_diameter": r3(dch), "levels_closed": closed_levels, "axis_line_clear": through}, 8))

    # Segments along the axis, told apart by their radial profile rather
    # than by where they sit. The spec never states the order of thread,
    # flange and barb, and the validation round cad-20260928T231444Z had a
    # correct part built flange-down (hex, thread, barb from the bed up):
    # the grader assumed the thread and the barb lay on opposite sides of
    # the flange, found no barb, and failed five gates on a part that met
    # every stated number. Each level is hex (mean radius >= 13.5, wider
    # than any M24 thread), thread-like (some direction reaches radius 9,
    # beyond any barb for a 12 ID hose, whose gate allows 8) or barb-like.
    Rmean = [sum(R[d][i] for d in R) / 4 for i in range(len(zs))]
    rmax4 = [max(R[d][i] for d in R) for i in range(len(zs))]
    rmin4 = [min(R[d][i] for d in R) for i in range(len(zs))]
    labels = ["H" if Rmean[i] >= 13.5 else "T" if rmax4[i] >= 9 else "B" for i in range(len(zs))]
    runs = []  # [label, first index, last index]
    for i, lab in enumerate(labels):
        if runs and runs[-1][0] == lab:
            runs[-1][2] = i
        else:
            runs.append([lab, i, i])

    def longest(lab):
        rs = [r for r in runs if r[0] == lab]
        return max(rs, key=lambda r: r[2] - r[1]) if rs else None

    def edge_lo(i):
        return max(p.bmin[2], zs[i] - step / 2)

    def edge_hi(i):
        return min(p.bmax[2], zs[i] + step / 2)

    # Hex: the longest run of hex levels, sectioned every 0.25 mm. Only
    # levels with full corners (corners/flats of a regular hexagon,
    # 2/sqrt3 = 1.1547) count toward the across-flats median: a flange
    # whose corners are chamfered by a printable cone underneath, as on a
    # hex nut, is still 30 across flats (the CadQuery part of
    # cad-20260928T231444Z had 42-degree chamfered corners over 2.5 of its
    # 7.7 mm and failed a median of corners/flats over the whole flange).
    # A cylinder or a 12-gon never shows full hexagon corners.
    hx = longest("H")
    fl = (zs[hx[1]], zs[hx[2]]) if hx else None
    if not hx:
        out.append(check("hex flange 30 across flats (+-0.2)", False, r3(max(Rmean)), 30,
                         note="no level wider than the thread (mean radius >= 13.5)"))
    else:
        f0, f1 = fl
        levels = lin(f0 + 0.1, f1 - 0.1, max(2, min(60, int((f1 - f0) / 0.25))))
        per = []
        for z in levels:
            loops, _ = p.section(2, z)
            outer, _ = classify_loops(loops)
            if outer:
                mn, mx = sm.width_range(max(outer, key=sm.poly_area), 180)
                per.append((mn, mx / mn))
        full = [(a, r) for a, r in per if 1.13 <= r <= 1.18]
        af = sm.median([a for a, _ in full])
        full_mm = len(full) * (f1 - f0) / max(1, len(levels))
        # At least 0.5 mm of full-cornered hexagon: two sampled levels, so
        # one stray section cannot make a round flange pass.
        out.append(check("hex flange 30 across flats (+-0.2)", len(full) >= 2 and within(af, 29.8, 30.2),
                         {"across_flats": r3(af), "corners_over_flats": r3(sm.median([r for _, r in full])),
                          "full_hexagon_mm": r3(full_mm), "flange_thickness": r3(f1 - f0),
                          "corners_over_flats_all_levels": r3(sm.median([r for _, r in per]))}, 30,
                         note="across flats over the levels whose corners are a regular hexagon's "
                              "(corners/flats 1.13-1.18); chamfered corners elsewhere are allowed"))

    # Thread: the longest run of thread-like levels. Within it a level is
    # threaded while its groove is open: one of the four directions (a
    # quarter pitch apart, so one always looks into the groove of an
    # ISO-like profile) reaches within 30% of the depth of the root. A
    # cone or hull under the flange springs from the root and closes the
    # groove within 0.3 of the depth, so it is not thread (counting it
    # failed length, major diameter and pitch on all three correct threads
    # of pilot cad-20260928T202850Z). The crest radius is the 80th
    # percentile over levels of the widest direction: a helix shows its
    # crest in some direction at nearly every level, stacked rings only on
    # their crest flats (1/8 of a pitch), and a 45-degree cone from the
    # crest to the 13.5 hex test spans under 2 mm, so neither the rings
    # nor a cone's wider levels move it far from the crest. The thread is the longest cluster of open levels,
    # where a cluster only ends once the groove has stayed closed for more
    # than 1.6 mm (0.8 of the pitch): stacked rings close it at each
    # ring's crest for up to two thirds of a pitch, and must reach the
    # helical check below rather than be cut into pieces here.
    tr = longest("T")
    run = []
    if tr:
        side = list(range(tr[1], tr[2] + 1))
        # Levels of a polygon (a $fn = 6 cone between thread and flange)
        # are not thread, though they reach radius 9: opposite directions
        # match on both axes while the axes differ. A helix's opposite
        # sides are half a pitch apart, so on an ISO profile they never
        # match on both axes at once. Such a cone's flats can start inside
        # the root; counted, they pulled the root 1-1.5 mm low, no level of
        # a real 12 mm thread was open, and five thread gates failed on a
        # sound part (and the cone's own levels then read as groove).
        def polygonal(i):
            return (abs(R[0][i] - R[180][i]) <= 0.05 and abs(R[90][i] - R[270][i]) <= 0.05
                    and abs(R[0][i] - R[90][i]) > 0.1)

        cand = [i for i in side if not polygonal(i)] or side
        root = sorted(rmin4[i] for i in cand)[int(0.02 * (len(cand) - 1))]
        depth = sorted(rmax4[i] for i in cand)[int(0.8 * (len(cand) - 1))] - root
        open_ = [i for i in cand if rmin4[i] <= root + 0.3 * depth]
        clusters = []
        for i in open_:
            if clusters and zs[i] - zs[clusters[-1][-1]] <= 1.6:
                clusters[-1].append(i)
            else:
                clusters.append([i])
        if clusters:
            c = max(clusters, key=lambda c: zs[c[-1]] - zs[c[0]])
            run = list(range(c[0], c[-1] + 1))
    thread_span = (edge_lo(run[0]), edge_hi(run[-1])) if run else None
    thread_len = thread_span[1] - thread_span[0] if run else 0.0

    # Thread: radius along four fixed directions as a function of z. A
    # helix shifts the profile by P/4 per quarter turn (right-hand: the
    # +90 degree direction lags by +P/4); stacked rings do not shift it.
    ti = list(run)
    margin = int(0.5 / step)
    ti = ti[margin:-margin] if len(ti) > 2 * margin + 10 else ti
    out.append(check("thread 12 long (+-1, extent of the open groove)", within(thread_len, 11, 13),
                     {"thread": r3(thread_len), "span": r3(thread_span),
                      "thread_like_levels": r3((zs[tr[1]], zs[tr[2]])) if tr else None}, 12))
    thread_names = ("thread major diameter 24 (-0.6/+0.1)", "pitch 2 (+-0.1)",
                    "real helical thread (not stacked rings), depth >= 0.8", "right-hand thread")
    if len(ti) < int(4 / step):
        # Each thread gate fails on its own, so a part without a usable
        # thread has as many gates as one with it (the round above counted
        # 7 gates for this part and 10 for the others).
        for name in thread_names:
            out.append(check(name, False, None, None, note="no thread 4 mm long to sample"))
    else:
        prof = {d: [R[d][i] for i in ti] for d in R}
        allr = sorted(r for d in prof for r in prof[d])
        major = 2 * allr[int(0.98 * (len(allr) - 1))]
        minor = 2 * allr[int(0.02 * (len(allr) - 1))]
        amp = (major - minor) / 2
        # +0.1/-0.6: printed M24 external threads are usually made slightly
        # undersize for fit (ISO 6g major is 23.62..23.96).
        out.append(check(thread_names[0], within(major, 23.4, 24.1), r3(major), 24))
        out.append(check("thread minor diameter", None if amp < 0.3 else True, r3(minor), 21.55, gate=False,
                         note="ISO M24x2 external minor d3 = 21.546; information only"))
        # Pitch: the first autocorrelation peak of one direction's profile.
        a = prof[0]
        n = len(a)
        m = sum(a) / n
        ac = []
        for L in range(0, min(n // 2, int(4.5 / step))):
            ac.append(sum((a[i] - m) * (a[i + L] - m) for i in range(n - L)) / max(1e-12, sum((x - m) ** 2 for x in a)))
        pitch = None
        lo = int(0.8 / step)
        for L in range(lo, len(ac) - 1):
            if ac[L] >= ac[L - 1] and ac[L] >= ac[L + 1] and ac[L] > 0.3:
                pitch = L * step
                break
        out.append(check(thread_names[1], within(pitch, 1.9, 2.1), r3(pitch), 2,
                         note="first autocorrelation peak of the radius profile along one direction"))
        P = pitch or 2
        maxlag = int(P / 2 / step)
        lags = {d: xcorr_lag(prof[d], prof[0], maxlag) for d in (90, 180, 270)}
        f = {d: lags[d][0] * step / P for d in lags}  # in turns of pitch
        helical = (amp >= 0.8 and 0.15 <= abs(f[90]) <= 0.35 and 0.15 <= abs(f[270]) <= 0.35
                   and f[90] * f[270] < 0 and abs(f[180]) >= 0.35)
        out.append(check(thread_names[2],
                         helical, {"depth": r3(amp), "shift_per_quarter_turn_in_pitches":
                                   {d: r3(f[d]) for d in f}, "correlations": {d: r3(lags[d][1]) for d in lags}},
                         {"90": 0.25, "180": 0.5, "270": -0.25},
                         note="profile shift between directions; ISO M24x2 depth is 1.23"))
        out.append(check(thread_names[3], helical and f[90] > 0 if helical else None,
                         r3(f[90]), 0.25, note="M threads are right-hand unless marked LH"))

    # Barb: the longest run of barb-like levels, measured from the feature
    # it springs from (the hex face or the thread's end, whichever is
    # nearer; anything between, such as a chamfer or collar, is the barb's
    # stem) to the next feature or the part's end on its other side.
    #
    # A flare is the cone by which the barb's stem widens into the feature
    # it springs from: a fillet at the barb's root, or a skirt under the
    # flange that lets the flange print without support when the barb is
    # down. It has no retaining face and grips no hose, so it is not a
    # barb (below), and the spec does not say whether it belongs to the
    # "25 long barb" or to the flange: a 1.8 mm root fillet inside the 25
    # and an 11 mm skirt below a 25 mm barb both meet the spec as a person
    # would read it, and the 11 mm skirt failed the length and the barb
    # count on a correct part. So the length passes measured either from
    # the feature or from where the flare starts. The flare is the levels
    # next to the feature that narrow away from it at FLARE_SLOPE or
    # steeper (about 27 degrees from the axis and up; a 45-degree flare is
    # 1) over at least 0.5 mm: a barb's ramp is shallower (the
    # reference's is 0.12, agents' about 0.2), so a tooth that starts at
    # the flange face stays a barb, and so does a crest behind a small
    # chamfer. A plain collar does not narrow, so it stays the barb's
    # stem, as before.
    br = longest("B")
    flare = {"lo": 0, "hi": 0}  # levels of the barb run in a flare, at each end
    barb_lens = []
    if br:
        z0, z1 = zs[br[1]], zs[br[2]]
        below = [z for z in ((fl[1] + step / 2) if fl and fl[1] < z0 else None,
                             thread_span[1] if thread_span and thread_span[1] <= z0 else None) if z is not None]
        above = [z for z in ((fl[0] - step / 2) if fl and fl[0] > z1 else None,
                             thread_span[0] if thread_span and thread_span[0] >= z1 else None) if z is not None]
        b_lo = max(below) if below else p.bmin[2]
        b_hi = min(above) if above else p.bmax[2]
        barb_len = b_hi - b_lo
        # Walk from the feature's face towards the barb while the profile
        # narrows steeply. The flare's wide end is often too wide to be
        # barb-like (a 45-degree skirt reads as thread-like past radius 9
        # and as hex past 13.5), so the walk starts at the face, not at the
        # barb run. Only an end that meets a feature can flare into it;
        # the part's own end is the hose's tip.
        lo_z, hi_z = b_lo, b_hi
        for end, inward, face in (("lo", 1, b_lo if below else None), ("hi", -1, b_hi if above else None)):
            if face is None:
                continue
            ins = [i for i in range(len(zs)) if (zs[i] > face if inward > 0 else zs[i] < face)]
            i = ins[0] if inward > 0 else ins[-1]
            while ((i + inward <= br[2] if inward > 0 else i + inward >= br[1])
                   and Rmean[i + inward] < Rmean[i] - FLARE_SLOPE * step):
                i += inward
            # It counts if it is 0.5 mm long and reaches into the barb run
            # past its first level.
            if abs(zs[i] - face) >= 0.5 and (i > br[1] if inward > 0 else i < br[2]):
                flare[end] = abs(i - (br[1] if inward > 0 else br[2]))
                if inward > 0:
                    lo_z = zs[i]
                else:
                    hi_z = zs[i]
        barb_lens = [barb_len]
        if flare["lo"] or flare["hi"]:
            barb_lens.append(hi_z - lo_z)
    else:
        barb_len = None
    out.append(check("barb 25 long (+-1, from the hex face or thread end to its end)",
                     any(within(x, 24, 26) for x in barb_lens),
                     r3(barb_len) if len(barb_lens) < 2 else {"from_feature": r3(barb_lens[0]),
                                                              "from_flare": r3(barb_lens[1])}, 25,
                     note="a flare into the hex or thread counts as the barb's root or as the feature's"))

    # Barbs: plateaus of the mean radius that stand 0.3 above the lowest
    # point on each side before the profile rises higher again (their
    # prominence). A side with no samples (the barb run ends at the crest)
    # is not counted. The earlier rule, 0.3 above anything within 3 mm,
    # took a plain stem ending in a tip chamfer for three more barbs (the
    # OpenSCAD part of cad-20260928T231444Z: three 13.39 crests plus three
    # 11.6 "peaks" on the plain stem next to the 10.4 tip).
    peaks = []
    prof = [Rmean[i] for i in range(br[1], br[2] + 1)] if br else []
    j = 0
    while j < len(prof):
        k = j
        while k + 1 < len(prof) and abs(prof[k + 1] - prof[j]) <= 1e-3:
            k += 1
        top = prof[j]
        if (j == 0 or prof[j - 1] < top) and (k == len(prof) - 1 or prof[k + 1] < top):
            sides_min = []
            for rng in (range(j - 1, -1, -1), range(k + 1, len(prof))):
                lowest = None
                for q in rng:
                    if prof[q] > top:
                        break
                    lowest = prof[q] if lowest is None else min(lowest, prof[q])
                if lowest is not None:
                    sides_min.append(lowest)
            prom = top - max(sides_min) if sides_min else 0
            z = zs[br[1] + (j + k) // 2]
            # The flare's wide end, where the run meets the feature, is
            # not a barb (see the barb's length above).
            in_flare = (j == 0 and flare["lo"]) or (k == len(prof) - 1 and flare["hi"])
            if prom >= 0.3 and not in_flare and (not peaks or z - peaks[-1][0] > 1.0):
                peaks.append((z, 2 * top))
        j = k + 1
    shank = 2 * min(prof) if prof else None
    out.append(check("three barbs", len(peaks) == 3, {"peaks": [r3(d) for _, d in peaks], "shank": r3(shank)}, 3,
                     note="plateaus of the mean radius with a prominence of 0.3 mm"))
    out.append(check("barbs grip a 12 ID hose (peak dia 12.5..16)",
                     bool(peaks) and all(12.5 <= d <= 16 for _, d in peaks), [r3(d) for _, d in peaks], "12.5..16",
                     note="the spec gives the hose, not the barb size; this range is typical for 12 ID"))

    # Thread at one end, barb at the other, hex between them. The spec does
    # not spell the order out, but "hose-barb adapter" implies it: the thread
    # must have a free end to screw into a port and the barb one to take the
    # hose. A thread between the hex and the barb (cad-20260928T231444Z's
    # NeoSCAD part) screws in only by burying the barb. Our reading; the
    # post says so.
    seg = {"hex": fl and (fl[0] + fl[1]) / 2, "thread": thread_span and sum(thread_span) / 2,
           "barb": br and (zs[br[1]] + zs[br[2]]) / 2}
    order = [k for k in sorted((k for k in seg if seg[k] is not None), key=lambda k: seg[k])]
    free = bool(thread_span) and (thread_span[0] - p.bmin[2] < 0.5 or p.bmax[2] - thread_span[1] < 0.5)
    usable = order in (["thread", "hex", "barb"], ["barb", "hex", "thread"])
    out.append(check("layout: thread and barb at opposite ends, hex between", usable,
                     {"order_along_axis": order, "thread_at_a_free_end": free}, "thread, hex, barb",
                     note="implied by 'hose-barb adapter', not stated by the spec"))
    return out


# ---------------------------------------------------------------------------
# Held-out tasks T4-T6. Written before any agent run saw these tasks, from
# the spec's numbers alone (tasks.json); like T1-T3, a check gates only on
# a number the spec states, and every tolerance is given beside it.


def at(a, along, uv):
    """The 3D point at `along` on axis a and (u, v) across it, in the
    section plane's own coordinates (stlmesh._axes)."""
    u, v = sm._axes(a)
    q = [0.0, 0.0, 0.0]
    q[a], q[u], q[v] = along, uv[0], uv[1]
    return q


def mode(xs, bin_mm=0.05):
    """The most common value to within `bin_mm`: a flat edge seen from
    most positions, which a median misses when knuckles, notches or webs
    cover half of them."""
    if not xs:
        return None
    best = max(xs, key=lambda x: sum(1 for y in xs if abs(y - x) <= bin_mm))
    return sm.median([y for y in xs if abs(y - best) <= bin_mm])


def quantile(xs, q):
    """The value a share q of xs lies at or below (nearest rank)."""
    xs = sorted(xs)
    return xs[min(len(xs) - 1, max(0, int(round(q * (len(xs) - 1)))))] if xs else None


def frange(a, b, step):
    n = int(math.floor((b - a) / step)) + 1
    return [a + i * step for i in range(max(0, n))]


def hinge(p, a, od_hint=7.0):
    """The pin hole along axis `a` and the knuckles around it.

    Sections across `a` every 0.5 mm find small holes (narrowest width
    1.2-3.5); the hole centre seen at most sections is the hinge axis.
    Knuckle outer diameter: twice the median distance from the axis to the
    vertices of the loop around the hole that lie within od_hint/2 + 1.5,
    so the knuckle's arc counts and the web or wall it joins does not.
    Knuckle spans: rays along `a` at mid-wall between hole and outside, in
    8 directions; a direction that runs along a web sees one long span,
    so the one seeing the most separate spans (each holding the hole) is
    used. Returns None without a hole."""
    stations = []
    for t in frange(p.bmin[a] + 0.25, p.bmax[a] - 0.25, 0.5):
        loops, _ = p.section(a, t)
        outer, holes = classify_loops(loops)
        for h in holes:
            wmin, wmax = sm.width_range(h, 90)
            if 1.2 <= wmin <= 3.5 and wmax <= 5:
                stations.append({"t": t, "c": sm.poly_centroid(h), "w": wmin, "outer": outer})
    clusters = []
    for s in stations:
        for c in clusters:
            if math.dist(s["c"], c[0]["c"]) <= 0.3:
                c.append(s)
                break
        else:
            clusters.append([s])
    if not clusters:
        return None
    best = max(clusters, key=len)
    c = (sm.median([s["c"][0] for s in best]), sm.median([s["c"][1] for s in best]))
    hole_d = sm.median([s["w"] for s in best])
    radii = []
    for s in best:
        around = [l for l in s["outer"] if sm.point_in_poly(c, l)]
        if not around:
            continue
        loop = min(around, key=lambda l: abs(sm.poly_area(l)))
        ds = [math.dist(c, q) for q in loop]
        near = [d for d in ds if d <= od_hint / 2 + 1.5]
        if near:
            radii.append(sm.median(near))
    od = 2 * sm.median(radii) if radii else None
    r_probe = (hole_d / 2 + (od or od_hint) / 2) / 2
    ts = [s["t"] for s in best]
    choice = None
    for k in range(8):
        th = math.pi * k / 4
        q = at(a, 0, (c[0] + r_probe * math.cos(th), c[1] + r_probe * math.sin(th)))
        spans = [iv for iv in p.intervals(a, q) if any(iv[0] <= t <= iv[1] for t in ts)]
        key = (len(spans), -sum(e - s for s, e in spans))
        if choice is None or key > choice[0]:
            choice = (key, spans)
    spans = choice[1]
    # The pin must pass every knuckle: no material on the axis inside a
    # span, and the hole seen in every span.
    on_axis = p.intervals(a, at(a, 0, c))
    blocked = [s for s, e in spans if any(x0 < e - 0.05 and x1 > s + 0.05 for x0, x1 in on_axis)]
    holed = [s for s, e in spans if any(s <= t <= e for t in ts)]
    return {"axis": c, "axis3": at(a, 0, c), "hole_d": hole_d, "od": od, "spans": spans,
            "through": not blocked and len(holed) == len(spans), "stations": len(best)}


def open_box(b):
    """Floor, walls, outer size and rim height of an open-top box, measured
    below the hinge: rays at heights from 1 above the floor to half the
    part's height, so knuckles and their webs near the rim are not in
    them."""
    H = b.bmax[2] - b.bmin[2]
    cx, cy = (b.bmin[0] + b.bmax[0]) / 2, (b.bmin[1] + b.bmax[1]) / 2
    fl = []
    for dx in lin(-0.2, 0.2, 5):
        for dy in lin(-0.2, 0.2, 5):
            iv = b.intervals(2, (cx + dx * (b.bmax[0] - b.bmin[0]), cy + dy * (b.bmax[1] - b.bmin[1]), 0))
            if iv and abs(iv[0][0] - b.bmin[2]) < 0.05 and iv[0][1] < b.bmin[2] + 0.5 * H:
                fl.append(iv[0][1] - iv[0][0])
    if len(fl) < 5:
        return None
    floor = sm.median(fl)
    lo, hi, walls, cav = {0: [], 1: []}, {0: [], 1: []}, [], {0: [], 1: []}
    for z in lin(b.bmin[2] + floor + 1, b.bmin[2] + 0.5 * H, 4):
        for axis, other in ((0, 1), (1, 0)):
            c = (b.bmin[other] + b.bmax[other]) / 2
            L = b.bmax[other] - b.bmin[other]
            for f in lin(-0.3, 0.3, 13):
                q = [0, 0, z]
                q[other] = c + f * L
                iv = b.intervals(axis, q)
                if len(iv) >= 2:
                    lo[axis].append(iv[0][0])
                    hi[axis].append(iv[-1][1])
                    walls += [iv[0][1] - iv[0][0], iv[-1][1] - iv[-1][0]]
                    cav[axis].append(iv[-1][0] - iv[0][1])
    if not lo[0] or not lo[1]:
        return None
    # Each outer face is the innermost quartile of what the rays see, not
    # the median: features outside a wall (ribs carrying the knuckles down
    # to the floor, which are hinge) can cover half of it. A median read
    # the back face 2 out on a 70 x 45 box whose knuckle ribs covered 36
    # of its 70 (a T4 part in one eval run) and failed its size
    # and its hinge axis.
    box = {"floor": floor, "wall": sm.median(walls),
           "lo": [quantile(lo[0], 0.75), quantile(lo[1], 0.75)], "hi": [quantile(hi[0], 0.25), quantile(hi[1], 0.25)]}
    w = box["wall"]
    tops = []
    for axis, other in ((0, 1), (1, 0)):
        for edge in (box["lo"][axis] + w / 2, box["hi"][axis] - w / 2):
            for t in lin(box["lo"][other] + 5, box["hi"][other] - 5, 9):
                q = [0, 0, 0]
                q[axis], q[other] = edge, t
                iv = b.intervals(2, q)
                if iv:
                    tops.append(iv[-1][1])
    box["rim"] = sm.median(tops) - b.bmin[2] if tops else None
    box["outer"] = [box["hi"][i] - box["lo"][i] for i in (0, 1)]
    return box


def side_offset(x, lo, hi):
    """Signed distance of x outward from the nearer of two faces lo < hi."""
    return max(lo - x, x - hi) if not lo <= x <= hi else -min(x - lo, hi - x)


def grade_t4(parts):
    out = []
    body, lid = parts.get("body"), parts.get("lid")
    box = open_box(body)
    if box is None:
        body = body.transformed(rot_x180)
        box = open_box(body)
    if box is None:
        return [check("body is an open box with a floor", False, None, None,
                      note="no floor found under the middle of the body from either side")]
    out.append(check("body measured", True, r3(box), gate=False))
    a = 0 if box["outer"][0] >= box["outer"][1] else 1
    bx = 1 - a
    o = sorted(box["outer"], reverse=True)
    # Flat faces measured by rays: +-0.2 for the outside (it may carry a
    # chamfer or rounded vertical edge whose facets move the median ray),
    # +-0.1 for walls and floor (T2's tolerance).
    out.append(check("body 70 x 45 x 30 outside (+-0.2)",
                     abs(o[0] - 70) <= 0.2 and abs(o[1] - 45) <= 0.2 and within(box["rim"], 29.8, 30.2),
                     r3(o + [box["rim"]]), [70, 45, 30], note="rim: median top of the walls"))
    out.append(check("walls 2 and floor 2 (+-0.1)",
                     within(box["wall"], 1.9, 2.1) and within(box["floor"], 1.9, 2.1),
                     r3([box["wall"], box["floor"]]), [2, 2]))

    # The lid: a plate whose faces and edges are the values most rays see.
    # It may be printed either face down; the knuckles stand on its top.
    la = 0 if (lid.bmax[0] - lid.bmin[0]) >= (lid.bmax[1] - lid.bmin[1]) else 1
    lb = 1 - la
    th = []
    for x in lin(lid.bmin[0], lid.bmax[0], 11)[2:-2]:
        for y in lin(lid.bmin[1], lid.bmax[1], 11)[2:-2]:
            iv = lid.intervals(2, (x, y, 0))
            if iv and abs(iv[0][0] - lid.bmin[2]) < 0.05:
                th.append(iv[0][1] - iv[0][0])
    lt = mode(th)
    edges = {}
    if lt:
        z = lid.bmin[2] + lt / 2
        for axis, other in ((la, lb), (lb, la)):
            los, his = [], []
            for t in lin(lid.bmin[other] + 2, lid.bmax[other] - 2, 33):
                q = [0, 0, z]
                q[other] = t
                iv = lid.intervals(axis, q)
                if iv:
                    los.append(iv[0][0])
                    his.append(iv[-1][1])
            edges[axis] = (mode(los), mode(his))
    lsize = [edges[i][1] - edges[i][0] if i in edges and None not in edges[i] else None for i in (la, lb)]
    out.append(check("lid plate 70 x 45 x 3 (+-0.2, thickness +-0.1)",
                     lt is not None and None not in lsize and abs(lsize[0] - 70) <= 0.2
                     and abs(lsize[1] - 45) <= 0.2 and abs(lt - 3) <= 0.1,
                     r3(lsize + [lt]), [70, 45, 3],
                     note="each value the most common one over 33 rays (thickness: 49), so knuckles do not count"))

    hb, hl = hinge(body, a), hinge(lid, la)
    out.append(check("hinge found", True, {"body": r3(hb and {k: hb[k] for k in ("axis3", "spans", "stations")}),
                                           "lid": r3(hl and {k: hl[k] for k in ("axis3", "spans", "stations")})},
                     gate=False, note="the pin hole's axis along the 70 side, and knuckle spans along it"))
    lens = {"body": [e - s for s, e in hb["spans"]] if hb else [], "lid": [e - s for s, e in hl["spans"]] if hl else []}
    out.append(check("3 body and 2 lid knuckles, 12 long (+-0.2)",
                     len(lens["body"]) == 3 and len(lens["lid"]) == 2
                     and all(abs(x - 12) <= 0.2 for x in lens["body"] + lens["lid"]), r3(lens), 12))
    ods = [h["od"] for h in (hb, hl) if h]
    out.append(check("knuckles 7 outer diameter (+-0.3)", len(ods) == 2 and all(within(d, 6.7, 7.3) for d in ods),
                     r3(ods), 7, note="median distance from the axis to the knuckle's outline near it"))
    # The pin hole: its narrowest width, which a teardrop or faceted hole
    # keeps at its nominal size (+-0.15), through every knuckle of a part.
    holes = [h["hole_d"] for h in (hb, hl) if h]
    out.append(check("2 pin hole through all knuckles on one axis (+-0.15)",
                     len(holes) == 2 and all(within(d, 1.85, 2.15) for d in holes) and hb["through"] and hl["through"],
                     {"diameters": r3(holes), "through": [h["through"] for h in (hb, hl) if h]}, 2))

    # Interleaving: lid knuckles in the body's gaps, 0.4 from each body
    # knuckle (+-0.1). The lid's spans are placed by its plate's ends,
    # which match the body's 70 when closed, either way round (a lid
    # turned over about the hinge's perpendicular reverses them).
    gaps_best = None
    if hb and hl and la in edges and None not in edges[la]:
        L0, L1 = edges[la]
        B = [(s - box["lo"][a], e - box["lo"][a], "B") for s, e in hb["spans"]]
        for lmap in ((lambda x: x - L0), (lambda x: L1 - x)):
            Ls = [tuple(sorted((lmap(s), lmap(e)))) + ("L",) for s, e in hl["spans"]]
            seq = sorted(B + Ls)
            gaps = [seq[i + 1][0] - seq[i][1] for i in range(len(seq) - 1)]
            alt = all(seq[i][2] != seq[i + 1][2] for i in range(len(seq) - 1)) and seq and seq[0][2] == "B"
            err = max((abs(g - 0.4) for g in gaps), default=9) if alt else 9
            if gaps_best is None or err < gaps_best[0]:
                gaps_best = (err, gaps, "".join(s[2] for s in seq))
    out.append(check("knuckles alternate with 0.4 between neighbours (+-0.1)",
                     gaps_best is not None and gaps_best[0] <= 0.1 + 1e-6 and gaps_best[2] == "BLBLB",
                     gaps_best and {"gaps": r3(gaps_best[1]), "order": gaps_best[2]}, 0.4,
                     note="lid spans placed by its plate's ends, either way round"))

    # Closed, the lid's axis must be the body's: the same distance out
    # from the hinge-side wall (lid: plate edge), and the same height over
    # the rim (lid: over the face that rests on the rim, which is either
    # face of a plate printed either way up). +-0.3, a third of the play a
    # 2 hole leaves around 1.75 filament, doubled for two parts.
    al = None
    if hb and hl and lt and lb in edges and None not in edges[lb]:
        ab, al3 = hb["axis3"], hl["axis3"]
        h_b = side_offset(ab[bx], box["lo"][bx], box["hi"][bx])
        v_b = ab[2] - (body.bmin[2] + box["rim"])
        h_l = side_offset(al3[lb], *edges[lb])
        v_ls = [al3[2] - lid.bmin[2], lid.bmin[2] + lt - al3[2]]
        dv = min(abs(v - v_b) for v in v_ls)
        al = {"body_out_up": r3([h_b, v_b]), "lid_out": r3(h_l), "lid_up_either_face": r3(v_ls),
              "ok": abs(h_l - h_b) <= 0.3 and dv <= 0.3}
    out.append(check("lid's hinge axis on the body's when closed (+-0.3)", bool(al and al["ok"]), al, None,
                     note="axis offset out from the hinge-side wall and up from the rim, body against lid"))
    out.append(check("lid opens and knuckles print without support", None, None, None, gate=False,
                     note="not judged: swing clearance and the knuckles' overhangs need the assembly"))
    return out


# ---------------------------------------------------------------------------
# T5: D-shaft knob


def outer_radii(loop, c, n=720):
    """Distance from c to the outline along n directions (c inside it)."""
    rs = []
    for k in range(n):
        th = 2 * math.pi * k / n
        rs.append(sm.ray2d_first_hit(c, (math.cos(th), math.sin(th)), [loop]))
    return rs


def count_lobes(rs, frac=0.5):
    """Runs of the radius profile below the midpoint between its extremes,
    around the circle: flutes (or the gaps between teeth)."""
    rs = [r for r in rs if r is not None]
    if not rs:
        return 0, 0
    lo, hi = min(rs), max(rs)
    mid = lo + frac * (hi - lo)
    below = [r < mid for r in rs]
    if all(below) or not any(below):
        return 0, hi - lo
    k = below.index(False)
    rot = below[k:] + below[:k]
    runs = sum(1 for i in range(1, len(rot)) if rot[i] and not rot[i - 1])
    return runs, hi - lo


def grade_t5(parts):
    p = parts["knob"]
    out = []
    ext = [p.bmax[i] - p.bmin[i] for i in range(3)]
    ax = min(range(3), key=lambda i: ext[i])
    if ax != 2:
        p = p.transformed(to_z(ax))
    # The bore opens on the bottom face: turn the knob so it does.
    cx, cy = (p.bmin[0] + p.bmax[0]) / 2, (p.bmin[1] + p.bmax[1]) / 2
    iv = p.intervals(2, (cx, cy, 0))
    if iv and abs(iv[0][0] - p.bmin[2]) < 0.05 and abs(iv[-1][1] - p.bmax[2]) > 0.5:
        p = p.transformed(rot_x180)
        cy = -cy
    H = p.bmax[2] - p.bmin[2]
    z0 = p.bmin[2]
    out.append(check("18 tall (+-0.1)", within(H, 17.9, 18.1), r3(H), 18, note="along the knob's shortest extent"))

    # Outside and flutes: sections at five heights, each around its
    # outline's bbox centre; the median of each measure, so one level that
    # a set-screw hole or a chamfer crosses does not decide it.
    per = []
    for f in (0.2, 0.35, 0.5, 0.65, 0.8):
        loops, _ = p.section(2, z0 + f * H)
        outer, _ = classify_loops(loops)
        if not outer:
            continue
        ol = max(outer, key=sm.poly_area)
        x0, y0, x1, y1 = sm.poly_bbox(ol)
        cc = ((x0 + x1) / 2, (y0 + y1) / 2)
        rs = outer_radii(ol, cc)
        n, depth = count_lobes(rs)
        per.append((n, depth, max(r for r in rs if r is not None), cc))
    if not per:
        return out + [check("knob section", False, None, None, note="no outline")]
    n = sm.median([q[0] for q in per])
    depth = sm.median([q[1] for q in per])
    rmax = sm.median([q[2] for q in per])
    c = per[len(per) // 2][3]
    out.append(check("30 diameter (+-0.2)", within(2 * rmax, 29.8, 30.2), r3(2 * rmax), 30,
                     note="over the lands between flutes"))
    out.append(check("20 grip flutes", n == 20, n, 20))
    out.append(check("flutes 1 deep (+-0.25)", within(depth, 0.75, 1.25), r3(depth), 1))

    # Bore depth: rays along the axis near the centre; the first material
    # above the bottom face is the bore's end. +-0.2 for a flat end, which
    # a short bridge or a 45-degree cone tip would not have (the cone is
    # not measured at the centre but up to 1 off it).
    ends, through = [], 0
    for dx, dy in ((0, 0), (0.5, 0), (-0.5, 0), (0, 0.5), (0, -0.5)):
        iv = p.intervals(2, (c[0] + dx, c[1] + dy, 0))
        if not iv:
            through += 1
        elif iv[0][0] > z0 + 0.05:
            ends.append(iv[0][0] - z0)
    bd = sm.median(ends)
    out.append(check("bore 12 deep, not through the top (+-0.2)", through == 0 and within(bd, 11.8, 12.2),
                     {"depth": r3(bd), "through_rays": through}, 12))

    # The D: the hole around the centre in sections below and above the
    # set screw (1.5, 2.5, 8 and 10 from the bottom). Calipers: widest
    # (the 6.2 across the round) and narrowest (flat to round, 4.7);
    # +-0.15 covers faceting (a 32-gon of 6.2 is 6.17 across its flats).
    mins, maxs, dirs = [], [], []
    for h in (1.5, 2.5, 8, 10):
        loops, _ = p.section(2, z0 + h)
        _, holes = classify_loops(loops)
        hs = [l for l in holes if sm.point_in_poly(c, l)]
        if not hs:
            continue
        hl = hs[0]
        best = None
        for k in range(360):
            th = math.pi * k / 360
            proj = [(x - c[0]) * math.cos(th) + (y - c[1]) * math.sin(th) for x, y in hl]
            w = max(proj) - min(proj)
            if best is None or w < best[0]:
                best = (w, th, min(proj), max(proj))
        mins.append(best[0])
        maxs.append(sm.width_range(hl, 360)[1])
        # The flat is the side nearer the centre.
        th = best[1] if abs(best[3]) < abs(best[2]) else best[1] + math.pi
        dirs.append((math.cos(th), math.sin(th), min(abs(best[2]), abs(best[3]))))
    dmax, dmin = sm.median(maxs), sm.median(mins)
    out.append(check("D bore 6.2 across, flat 4.7 from the far side (+-0.15)",
                     within(dmax, 6.05, 6.35) and within(dmin, 4.55, 4.85), r3([dmax, dmin]), [6.2, 4.7],
                     note="calipers of the bore's section at 1.5, 2.5, 8 and 10 from the bottom"))

    # Set screw: turn the knob so the flat faces +x and cut it at x = 8
    # out from the axis, between the flat and the outside; the hole there
    # is the screw's. Narrowest width 2.5 (+-0.2, a teardrop keeps it),
    # axis 5 from the bottom (+-0.3) and on the flat's middle (+-0.3), and
    # nothing in the way from the bore to the outside.
    ss = None
    if dirs:
        nx, ny = sm.median([d[0] for d in dirs]), sm.median([d[1] for d in dirs])
        ang = math.atan2(ny, nx)
        ca, sa = math.cos(-ang), math.sin(-ang)

        def rot(q):
            x, y = q[0] - c[0], q[1] - c[1]
            return (x * ca - y * sa, x * sa + y * ca, q[2])

        r = p.transformed(rot)
        loops, _ = r.section(0, 8.0)
        _, holes = classify_loops(loops)
        cands = []
        for hl in holes:
            wmin, wmax = sm.width_range(hl, 180)
            ys = [q[0] for q in hl]
            zs = [q[1] for q in hl]
            yc = (min(ys) + max(ys)) / 2
            zc = min(zs) + (max(ys) - min(ys)) / 2 - z0
            if 1.5 <= wmin <= 4:
                cands.append((abs(yc) + abs(zc - 5), wmin, yc, zc))
        if cands:
            _, wmin, yc, zc = min(cands)
            flat = sm.median([d[2] for d in dirs])
            iv = r.intervals(0, (0, yc, z0 + zc))
            clear = not any(s < 14 and e > flat + 0.3 for s, e in iv)
            if through:  # no blind end to say which face is the bottom: either will do
                zc = min(zc, H - zc, key=lambda v: abs(v - 5))
            ss = {"diameter": r3(wmin), "from_bottom": r3(zc), "off_middle": r3(yc), "clear": clear,
                  "ok": within(wmin, 2.3, 2.7) and within(zc, 4.7, 5.3) and abs(yc) <= 0.3 and clear}
    out.append(check("2.5 set-screw hole through the flat's middle, 5 from the bottom", bool(ss and ss["ok"]), ss,
                     {"diameter": 2.5, "from_bottom": 5}))
    return out


# ---------------------------------------------------------------------------
# T6: spur gears on a base plate


def gear_profile(g):
    """Section of a gear at mid-thickness: its outline, bore and centre
    (the bore's centroid), lying flat whatever axis it was printed on."""
    ext = [g.bmax[i] - g.bmin[i] for i in range(3)]
    ax = min(range(3), key=lambda i: ext[i])
    if ax != 2:
        g = g.transformed(to_z(ax))
    T = g.bmax[2] - g.bmin[2]
    loops, _ = g.section(2, g.bmin[2] + T / 2)
    outer, holes = classify_loops(loops)
    if not outer:
        return None
    ol = max(outer, key=sm.poly_area)
    x0, y0, x1, y1 = sm.poly_bbox(ol)
    mid = ((x0 + x1) / 2, (y0 + y1) / 2)
    bores = [h for h in holes if sm.point_in_poly(mid, h)]
    c = sm.poly_centroid(bores[0]) if bores else mid
    rs = outer_radii(ol, c, 1440)
    teeth, _ = count_lobes([-r for r in rs if r is not None])
    rr = [r for r in rs if r is not None]
    # The meshing test is quadratic in edges near the mesh, so a finely
    # tessellated outline (a CAD kernel's arcs can be thousands of points)
    # keeps only vertices 0.1 apart: the chord then strays 0.001 from an
    # R1 arc, against 0.18 of backlash.
    thin = []
    for q in ol:
        if not thin or math.dist(q, thin[-1]) >= 0.1:
            thin.append(q)
    return {"T": T, "outline": [(x - c[0], y - c[1]) for x, y in thin],
            "bore": sm.circum_diameter(bores[0]) if bores else None,
            "teeth": teeth, "tip": max(rr), "root": min(rr)}


def _rot(poly, ang, dx=0.0):
    ca, sa = math.cos(ang), math.sin(ang)
    return [(x * ca - y * sa + dx, x * sa + y * ca) for x, y in poly]


def _seg_cross(p1, p2, q1, q2):
    def orient(a, b, c):
        return (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    d1, d2 = orient(q1, q2, p1), orient(q1, q2, p2)
    d3, d4 = orient(p1, p2, q1), orient(p1, p2, q2)
    return (d1 > 0) != (d2 > 0) and (d3 > 0) != (d4 > 0)


def _overlap(A, B, cA, cB, rA, rB):
    """Do outlines A and B cross? Only edges in the lens where each could
    reach the other (within the other's tip radius) are tested."""
    ea = [(A[i], A[(i + 1) % len(A)]) for i in range(len(A))
          if math.dist(A[i], cB) <= rB + 0.05 or math.dist(A[(i + 1) % len(A)], cB) <= rB + 0.05]
    eb = [(B[i], B[(i + 1) % len(B)]) for i in range(len(B))
          if math.dist(B[i], cA) <= rA + 0.05 or math.dist(B[(i + 1) % len(B)], cA) <= rA + 0.05]
    # B's edges bucketed on a 0.5 mm grid by bbox, so each of A's edges
    # meets only the few that could cross it.
    cell = 0.5
    grid = {}
    for e in eb:
        (x1, y1), (x2, y2) = e
        for i in range(int(math.floor(min(x1, x2) / cell)), int(math.floor(max(x1, x2) / cell)) + 1):
            for j in range(int(math.floor(min(y1, y2) / cell)), int(math.floor(max(y1, y2) / cell)) + 1):
                grid.setdefault((i, j), []).append(e)
    for p1, p2 in ea:
        seen = set()
        for i in range(int(math.floor(min(p1[0], p2[0]) / cell)), int(math.floor(max(p1[0], p2[0]) / cell)) + 1):
            for j in range(int(math.floor(min(p1[1], p2[1]) / cell)), int(math.floor(max(p1[1], p2[1]) / cell)) + 1):
                for e in grid.get((i, j), ()):
                    if id(e) in seen:
                        continue
                    seen.add(id(e))
                    if _seg_cross(p1, p2, e[0], e[1]):
                        return True
    # A tooth wholly inside the other gear crosses nothing: catch it by a
    # vertex of each lens inside the other outline.
    for (p1, _) in ea[::7]:
        if sm.point_in_poly(p1, B):
            return True
    for (q1, _) in eb[::7]:
        if sm.point_in_poly(q1, A):
            return True
    return False


def gears_turn(big, small, dist, ratio, steps=12, phase_step_deg=0.25):
    """Place the outlines `dist` apart and look for a phase of the small
    gear at which they do not overlap and from which the big gear can turn
    through one tooth pitch, the small one following at `ratio`, in
    `steps` steps, without the outlines crossing. That is turning without
    binding, at the stated centre distance."""
    zb = big["teeth"]
    pitch_b = 2 * math.pi / max(zb, 1)
    pitch_s = 2 * math.pi / max(small["teeth"], 1)
    cA, cB = (0.0, 0.0), (dist, 0.0)
    n = int(round(math.degrees(pitch_s) / phase_step_deg))
    for k in range(n):
        phi = k * pitch_s / n
        ok = True
        for j in range(steps + 1):
            d = pitch_b * j / steps
            A = _rot(big["outline"], d)
            B = _rot(small["outline"], phi - ratio * d, dist)
            if _overlap(A, B, cA, cB, big["tip"], small["tip"]):
                ok = False
                break
        if ok:
            return math.degrees(phi)
    return None


def grade_t6(parts):
    out = []
    pl = parts["plate"]
    # The plate: thickness from rays starting on its bottom face, away from
    # the axles; outline from its section at half that.
    th = []
    for x in lin(pl.bmin[0], pl.bmax[0], 13)[1:-1]:
        for y in lin(pl.bmin[1], pl.bmax[1], 13)[1:-1]:
            iv = pl.intervals(2, (x, y, 0))
            if iv and abs(iv[0][0] - pl.bmin[2]) < 0.05:
                th.append(iv[0][1] - iv[0][0])
    t = mode(th)
    size = None
    axles = []
    if t:
        loops, _ = pl.section(2, pl.bmin[2] + t / 2)
        outer, _ = classify_loops(loops)
        if outer:
            x0, y0, x1, y1 = sm.poly_bbox(max(outer, key=sm.poly_area))
            size = sorted([x1 - x0, y1 - y0], reverse=True)
        top = pl.bmin[2] + t
        loops, _ = pl.section(2, top + 5)
        outer, _ = classify_loops(loops)
        for l in outer:
            d = sm.circum_diameter(l)
            if 3 <= d <= 8:
                cc = sm.poly_centroid(l)
                iv = pl.intervals(2, (cc[0], cc[1], 0))
                axles.append({"d": d, "c": cc, "height": (iv[-1][1] - top) if iv else None})
    out.append(check("plate 80 x 55 x 4 (+-0.2, thickness +-0.1)",
                     size is not None and abs(size[0] - 80) <= 0.2 and abs(size[1] - 55) <= 0.2 and within(t, 3.9, 4.1),
                     r3((size or []) + [t]), [80, 55, 4]))
    # Axles: circumscribed diameter (a faceted 5 is 5 at its corners),
    # +-0.15; height to their top above the plate, +-0.2.
    out.append(check("two 5 axles standing 10 above the plate (+-0.15, height +-0.2)",
                     len(axles) == 2 and all(within(x["d"], 4.85, 5.15) and within(x["height"], 9.8, 10.2) for x in axles),
                     [{"d": r3(x["d"]), "height": r3(x["height"])} for x in axles], {"d": 5, "height": 10}))
    dist = math.dist(axles[0]["c"], axles[1]["c"]) if len(axles) == 2 else None
    out.append(check("axle centres 34 apart (+-0.1)", within(dist, 33.9, 34.1), r3(dist), 34))

    big, small = gear_profile(parts["large_gear"]), gear_profile(parts["small_gear"])
    # Teeth: dips of the radius profile between tips; outside diameter
    # m (z + 2), +-0.3 (a tip shortened for clearance or rounded is fine;
    # a module 1.6 gear is 51 and 27.2 across).
    for g, z, name in ((big, 30, "large"), (small, 15, "small")):
        out.append(check(f"{name} gear: {z} teeth", bool(g) and g["teeth"] == z, g and g["teeth"], z))
        od = 1.5 * (z + 2)
        out.append(check(f"{name} gear: outside diameter {od} (module 1.5, +-0.3)",
                         bool(g) and abs(2 * g["tip"] - od) <= 0.3, g and r3(2 * g["tip"]), od))
    out.append(check("gears 6 thick (+-0.1)", bool(big and small) and within(big["T"], 5.9, 6.1) and within(small["T"], 5.9, 6.1),
                     r3([g["T"] for g in (big, small) if g]), 6))
    out.append(check("5.4 bores (+-0.15)",
                     bool(big and small) and all(within(g["bore"], 5.25, 5.55) for g in (big, small)),
                     r3([g["bore"] for g in (big, small) if g]), 5.4, note="circumscribed diameter"))
    # Meshing: at the stated 34 (not the plate's, which has its own gate)
    # the outlines must turn through a tooth pitch together at 30:15
    # without crossing, and engage at least one module deep.
    mesh = None
    if big and small and big["teeth"] > 0 and small["teeth"] > 0:
        engage = big["tip"] + small["tip"] - 34
        phase = gears_turn(big, small, 34, big["teeth"] / small["teeth"])
        mesh = {"engagement": r3(engage), "free_phase_deg": r3(phase),
                "ok": phase is not None and engage >= 1.5 - 1e-6}
    out.append(check("gears mesh at 34 and turn a tooth pitch without binding", bool(mesh and mesh["ok"]), mesh, None,
                     note="mid-thickness outlines, big gear turned in 12 steps through one pitch, small at the "
                          "tooth ratio; engagement = tip radii - 34, at least the module"))
    if big and small:
        tt = []
        for g, z in ((big, 30), (small, 15)):
            rp = 1.5 * z / 2
            ins = sum(1 for k in range(3600) if sm.point_in_poly(
                (rp * math.cos(2 * math.pi * k / 3600), rp * math.sin(2 * math.pi * k / 3600)), g["outline"]))
            tt.append(ins / 3600 * 2 * math.pi * rp / max(g["teeth"], 1))
        out.append(check("tooth thickness at the pitch circle", None, r3(tt), r3(math.pi * 1.5 / 2), gate=False,
                         note="information: half the circular pitch less backlash for a standard tooth"))
    return out


GRADERS = {"T0": grade_t0, "T1": grade_t1, "T2": grade_t2, "T3": grade_t3,
           "T4": grade_t4, "T5": grade_t5, "T6": grade_t6}


def grade(task, files):
    """files: {part: path}. Returns the grade record."""
    res = {"task": task, "parts": {}, "checks": []}
    meshes = {}
    for name in PARTS[task]:
        path = files.get(name)
        if not path or not Path(path).exists():
            res["parts"][name] = {"missing": True, "clean": False}
            continue
        try:
            v, t = sm.load_stl(path)
        except (sm.MeshError, OSError) as e:
            res["parts"][name] = {"error": str(e), "clean": False}
            continue
        res["parts"][name] = sm.topology(v, t)
        meshes[name] = Part(v, t)
    if len(meshes) == len(PARTS[task]):
        try:
            res["checks"] = GRADERS[task](meshes)
        except Exception as e:  # a grader bug must not be scored as the model's fault silently
            res["checks"] = [check("grader ran", None, None, None, note=f"grader error: {type(e).__name__}: {e}")]
            res["grader_error"] = f"{type(e).__name__}: {e}"
    res["clean"] = all(p.get("clean") for p in res["parts"].values())
    gates = [c for c in res["checks"] if c["gate"]]
    res["gates_passed"] = sum(1 for c in gates if c["ok"] is True)
    res["gates_total"] = len(gates)
    res["failed_gates"] = [c["name"] for c in gates if c["ok"] is not True]
    res["pass"] = res["clean"] and bool(gates) and not res["failed_gates"]
    return res


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--task", required=True, choices=sorted(GRADERS))
    ap.add_argument("files", nargs="+", help="part=path.stl")
    args = ap.parse_args()
    files = dict(f.split("=", 1) for f in args.files)
    print(json.dumps(grade(args.task, files), indent=1))


if __name__ == "__main__":
    main()
