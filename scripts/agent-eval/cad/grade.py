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

PARTS = {"T0": ["plate"], "T1": ["bracket"], "T2": ["base", "lid"], "T3": ["adapter"]}
SQ2M1 = math.sqrt(2) - 1


def check(name, ok, value=None, expected=None, gate=True, note=None):
    c = {"name": name, "gate": gate, "ok": ok, "value": value, "expected": expected}
    if note:
        c["note"] = note
    return c


def r3(x):
    if x is None:
        return None
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

    # Posts: M2 holes in the section 1 mm above the floor.
    loops, _ = base.section(2, cav["floor_top"] + 1.0)
    _, holes = classify_loops(loops)
    post_holes = [sm.equiv_diameter(h) for h in holes if 1.4 <= sm.equiv_diameter(h) <= 3.7]
    out.append(check("four M2 posts (holes 1.5..3.6 dia, 1 mm above the floor)",
                     len(post_holes) == 4 and all(1.5 <= d <= 3.6 for d in post_holes),
                     r3(post_holes), 4, note="a post counts by its hole: pilot (1.5-1.8), clearance "
                                              "(2.2-2.4) or heat-set insert (3.2-3.6); solid posts are not counted"))

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
    # windows or screw holes of another size are not counted as vents.
    slots = [o for o in opens if o not in usb[:1] and o["size"][1] >= 0.5 and o["size"][0] >= 2 * o["size"][1]]
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
                     note="slots: through-openings at least twice as long as wide, grouped by size (+-0.2)"))

    # Lid lip clearance: rings of the lid above its plate, measured with
    # rays at 9 positions x 4 levels, against the base's cavity (lip inside)
    # or outer wall (skirt outside).
    res = {0: [], 1: []}
    widths = {0: [], 1: []}
    kind = None
    if plate_t is not None and lid.bmax[2] - (lid.bmin[2] + plate_t) > 0.8:
        z0 = lid.bmin[2] + plate_t
        Lx, Ly = lid.bmax[0] - lid.bmin[0], lid.bmax[1] - lid.bmin[1]
        cx, cy = (lid.bmin[0] + lid.bmax[0]) / 2, (lid.bmin[1] + lid.bmax[1]) / 2
        for z in lin(z0 + 0.4, lid.bmax[2] - 0.3, 4):
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
                        res[axis].append((cavd - (r[1] - r[0])) / 2)
                        widths[axis].append(r[1] - r[0])
                        kind = kind or "lip inside the walls"
                    elif outside:
                        r = outside[-1]
                        res[axis].append(((r[3] - r[2]) - outd) / 2)
                        widths[axis].append(r[3] - r[2])
                        kind = kind or "skirt outside the walls"
    cl = [sm.median(res[0]), sm.median(res[1])]
    out.append(check("lid lip clearance 0.2 per side (+-0.05)", all(within(c, 0.15, 0.25) for c in cl),
                     {"clearance_per_axis": r3(cl), "kind": kind, "lid_plate": r3(plate_t)}, 0.2,
                     note="median over positions, so local snap bumps do not count"))
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

    Rmean = [sum(R[d][i] for d in R) / 4 for i in range(len(zs))]
    fl = [i for i, r in enumerate(Rmean) if r >= 13.5]
    if not fl:
        out.append(check("hex flange found", False, r3(max(Rmean)), 15,
                         note="no level wider than the thread (mean radius >= 13.5)"))
        return out
    f0, f1 = zs[fl[0]], zs[fl[-1]]
    afs, ratios = [], []
    for z in lin(f0 + 0.2, f1 - 0.2, 8):
        loops, _ = p.section(2, z)
        outer, _ = classify_loops(loops)
        if outer:
            mn, mx = sm.width_range(max(outer, key=sm.poly_area), 180)
            afs.append(mn)
            ratios.append(mx / mn)
    af, ratio = sm.median(afs), sm.median(ratios)
    # A regular hexagon's corners are 2/sqrt3 = 1.1547 times its flats.
    out.append(check("hex flange 30 across flats (+-0.2)", within(af, 29.8, 30.2) and within(ratio, 1.13, 1.18),
                     {"across_flats": r3(af), "corners_over_flats": r3(ratio), "flange_thickness": r3(f1 - f0)}, 30))

    sides = {"low": [i for i, z in enumerate(zs) if z < f0 - 0.1],
             "high": [i for i, z in enumerate(zs) if z > f1 + 0.1]}
    spans = {"low": f0 - p.bmin[2], "high": p.bmax[2] - f1}

    def rmax(idx):
        return sorted(max(R[d][i] for d in R) for i in idx)[int(0.95 * (len(idx) - 1))] if idx else 0

    thread_side = min(sides, key=lambda s: abs(rmax(sides[s]) - 12))
    barb_side = "high" if thread_side == "low" else "low"

    # Thread: radius along four fixed directions as a function of z. A
    # helix shifts the profile by P/4 per quarter turn (right-hand: the
    # +90 degree direction lags by +P/4); stacked rings do not shift it.
    ti = sides[thread_side]
    margin = int(0.5 / step)
    ti = ti[margin:-margin] if len(ti) > 2 * margin + 10 else ti
    out.append(check("thread 12 long (+-1, flange face to end)", within(spans[thread_side], 11, 13),
                     r3(spans[thread_side]), 12))
    if len(ti) < int(4 / step):
        out.append(check("M24x2 helical thread", False, None, None, note="thread side too short to sample"))
    else:
        prof = {d: [R[d][i] for i in ti] for d in R}
        allr = sorted(r for d in prof for r in prof[d])
        major = 2 * allr[int(0.98 * (len(allr) - 1))]
        minor = 2 * allr[int(0.02 * (len(allr) - 1))]
        amp = (major - minor) / 2
        # +0.1/-0.6: printed M24 external threads are usually made slightly
        # undersize for fit (ISO 6g major is 23.62..23.96).
        out.append(check("thread major diameter 24 (-0.6/+0.1)", within(major, 23.4, 24.1), r3(major), 24))
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
        out.append(check("pitch 2 (+-0.1)", within(pitch, 1.9, 2.1), r3(pitch), 2,
                         note="first autocorrelation peak of the radius profile along one direction"))
        P = pitch or 2
        maxlag = int(P / 2 / step)
        lags = {d: xcorr_lag(prof[d], prof[0], maxlag) for d in (90, 180, 270)}
        f = {d: lags[d][0] * step / P for d in lags}  # in turns of pitch
        helical = (amp >= 0.8 and 0.15 <= abs(f[90]) <= 0.35 and 0.15 <= abs(f[270]) <= 0.35
                   and f[90] * f[270] < 0 and abs(f[180]) >= 0.35)
        out.append(check("real helical thread (not stacked rings), depth >= 0.8",
                         helical, {"depth": r3(amp), "shift_per_quarter_turn_in_pitches":
                                   {d: r3(f[d]) for d in f}, "correlations": {d: r3(lags[d][1]) for d in lags}},
                         {"90": 0.25, "180": 0.5, "270": -0.25},
                         note="profile shift between directions; ISO M24x2 depth is 1.23"))
        out.append(check("right-hand thread", helical and f[90] > 0 if helical else None,
                         r3(f[90]), 0.25, note="M threads are right-hand unless marked LH"))

    # Barb: three peaks of the mean radius on the barb side.
    bi = sides[barb_side]
    out.append(check("barb 25 long (+-1, flange face to end)", within(spans[barb_side], 24, 26),
                     r3(spans[barb_side]), 25))
    prof = [Rmean[i] for i in bi]
    w = int(0.6 / step)
    peaks = []
    for i in range(len(prof)):
        seg = prof[max(0, i - w):i + w + 1]
        wide = prof[max(0, i - int(3 / step)):i + int(3 / step) + 1]
        if prof[i] >= max(seg) and prof[i] - min(wide) >= 0.3:
            z = zs[bi[i]]
            if not peaks or z - peaks[-1][0] > 1.0:
                peaks.append((z, 2 * prof[i]))
    shank = 2 * min(prof) if prof else None
    out.append(check("three barbs", len(peaks) == 3, {"peaks": [r3(d) for _, d in peaks], "shank": r3(shank)}, 3,
                     note="local maxima of the mean radius standing 0.3 mm above their surroundings"))
    out.append(check("barbs grip a 12 ID hose (peak dia 12.5..16)",
                     bool(peaks) and all(12.5 <= d <= 16 for _, d in peaks), [r3(d) for _, d in peaks], "12.5..16",
                     note="the spec gives the hose, not the barb size; this range is typical for 12 ID"))
    return out


GRADERS = {"T0": grade_t0, "T1": grade_t1, "T2": grade_t2, "T3": grade_t3}


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
