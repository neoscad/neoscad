#!/usr/bin/env python3
"""Translate SolveSpace's constraint and request regression files into the
sketch solver's validation corpus (crates/sketch/tests/corpus/README.md).

    scripts/sketch-corpus-slvs.py SOLVESPACE_CHECKOUT > \
        crates/sketch/tests/corpus/solvespace/regression.json

SolveSpace's tests under test/constraint and test/request load a .slvs
file, regenerate it and save it again, so each file holds a solved sketch:
the entities, the constraints, and every parameter at its solved value.
A case here keeps those values as `solved` and as the drawing; the corpus
test then checks that our equations hold at SolveSpace's solution, and
solves again from a perturbed drawing.

Only 2D sketches in the XY plane with constraints from the solver's v1
vocabulary are translated; everything else is listed under `skipped` with
the reason. The `_v20`/`_v22` files are older-format copies of the same
sketches (migration tests) and are left out.
"""

import json
import math
import os
import subprocess
import sys

XY = "80020000"
ORIGIN = "00010001"


def parse(path):
    """The records of a .slvs file: (kind, {key: value}) per Add* line."""
    records, cur = [], {}
    with open(path, "rb") as f:
        for raw in f:
            line = raw.decode("latin-1").strip()
            if not line:
                continue
            if line.startswith("Add") and "=" not in line:
                records.append((line[3:], cur))
                cur = {}
            elif "=" in line:
                k, v = line.split("=", 1)
                cur[k] = v
    return records


class Skip(Exception):
    pass


def convert(path, rel):
    recs = parse(path)
    groups = {r["Group.h.v"]: r for k, r in recs if k == "Group"}
    requests = {r["Request.h.v"]: r for k, r in recs if k == "Request"}
    entities = {r["Entity.h.v"]: r for k, r in recs if k == "Entity"}
    constraints = [r for k, r in recs if k == "Constraint"]
    for g in groups.values():
        if g.get("Group.type") not in ("5000", "5001"):
            raise Skip(f"has a group of type {g.get('Group.type')} (not a 2D sketch)")
    names, out_entities, solved, construction = {}, [], {}, []
    point_xy = {}

    def point(h):
        if h in names:
            return names[h]
        if h == ORIGIN:
            name = "origin"
            names[h] = name
            out_entities.append([name, "point", [0.0, 0.0]])
            solved[name] = [0.0, 0.0]
            point_xy[name] = (0.0, 0.0)
            fixed.append(name)
            return name
        e = entities.get(h)
        if e is None or e.get("Entity.type") not in ("2000", "2001"):
            raise Skip(f"refers to entity {h}, which is not a sketch point")
        if e.get("Entity.type") == "2001" and e.get("Entity.workplane.v") != XY:
            raise Skip("has a point in a workplane other than XY")
        if abs(float(e.get("Entity.actPoint.z", "0"))) > 1e-9:
            raise Skip("has a point off the XY plane")
        x = float(e.get("Entity.actPoint.x", "0"))
        y = float(e.get("Entity.actPoint.y", "0"))
        name = "p" + h.lstrip("0")
        names[h] = name
        out_entities.append([name, "point", [x, y]])
        solved[name] = [x, y]
        point_xy[name] = (x, y)
        return name

    fixed = []
    # Requests in the sketch group, in handle order.
    for rh in sorted(requests):
        r = requests[rh]
        if r.get("Request.group.v") != "00000002":
            continue
        t = r["Request.type"]
        eh = rh[-4:] + "0000"
        e = entities.get(eh)
        cons = r.get("Request.construction") == "1"
        if t == "101":
            point(rh[-4:] + "0000")
            continue
        if t not in ("200", "400", "500"):
            raise Skip(f"has a request of type {t} (only points, lines, circles and arcs are in v1)")
        if e is None:
            raise Skip(f"request {rh} has no entity")
        pts = [e.get(f"Entity.point[{i}].v") for i in range(3)]
        name = {"200": "l", "400": "c", "500": "a"}[t] + rh.lstrip("0")
        if t == "200":
            out_entities.append([name, "line", point(pts[0]), point(pts[1])])
        elif t == "500":
            out_entities.append([name, "arc", point(pts[0]), point(pts[1]), point(pts[2])])
        else:
            d = entities.get(e.get("Entity.distance.v"))
            r_val = abs(float(d.get("Entity.actDistance", "0")))
            out_entities.append([name, "circle", point(pts[0]), r_val])
            solved[name] = r_val
        names[eh] = name
        if cons:
            construction.append(name)
    kinds = {e[0]: e[1] for e in out_entities}
    lines = {e[0]: (e[2], e[3]) for e in out_entities if e[1] == "line"}

    def ent(h, *allowed):
        n = names.get(h)
        if n is None or kinds.get(n) not in allowed:
            raise Skip(f"constraint refers to {h}, not a sketch {'/'.join(allowed)}")
        return n

    out_c = []
    for c in constraints:
        t = int(c["Constraint.type"])
        if c.get("Constraint.group.v") != "00000002":
            raise Skip("has constraints outside the sketch group")
        wp = c.get("Constraint.workplane.v", "00000000")
        if wp not in (XY, "00000000"):
            raise Skip(f"has a constraint in workplane {wp}")
        if c.get("Constraint.reference") == "1":
            # A reference dimension measures; it does not constrain.
            continue
        val = float(c.get("Constraint.valA", "0"))
        pa, pb = c.get("Constraint.ptA.v"), c.get("Constraint.ptB.v")
        ea, eb = c.get("Constraint.entityA.v"), c.get("Constraint.entityB.v")
        other = c.get("Constraint.other") == "1"
        if t == 1000:
            continue
        elif t == 20:
            out_c.append(["coincident", point(pa), point(pb)])
        elif t == 30:
            out_c.append(["distance", point(pa), point(pb), abs(val)])
        elif t == 32:
            out_c.append(["distance", point(pa), ent(ea, "line"), abs(val)])
        elif t == 42:
            out_c.append(["on", point(pa), ent(ea, "line")])
        elif t == 50:
            out_c.append(["equal", ent(ea, "line"), ent(eb, "line")])
        elif t == 61 or t == 62:
            if wp != XY:
                raise Skip("horizontal/vertical symmetry outside a workplane")
            # Mirror about the workplane's v axis (61) or u axis (62):
            # a fixed construction line through the origin.
            axis = "axis_v" if t == 61 else "axis_u"
            if axis not in kinds:
                a0, a1 = axis + "0", axis + "1"
                end = [0.0, 1.0] if t == 61 else [1.0, 0.0]
                out_entities.extend([[a0, "point", [0.0, 0.0]], [a1, "point", end], [axis, "line", a0, a1]])
                solved[a0], solved[a1] = [0.0, 0.0], end
                kinds[axis] = "line"
                construction.append(axis)
                out_c.append(["fix", axis])
            out_c.append(["symmetric", point(pa), point(pb), axis])
        elif t == 63:
            out_c.append(["symmetric", point(pa), point(pb), ent(ea, "line")])
        elif t == 70:
            if names.get(ea) is None:
                raise Skip("midpoint on a plane")
            out_c.append(["midpoint", point(pa), ent(ea, "line")])
        elif t in (80, 81):
            word = "horizontal" if t == 80 else "vertical"
            if ea and ea != "00000000":
                out_c.append([word, ent(ea, "line")])
            else:
                out_c.append([word, point(pa), point(pb)])
        elif t == 90:
            out_c.append(["diameter", ent(ea, "arc", "circle"), abs(val)])
        elif t == 100:
            out_c.append(["on", point(pa), ent(ea, "arc", "circle")])
        elif t == 120:
            la, lb = ent(ea, "line"), ent(eb, "line")
            (a0, a1), (b0, b1) = lines[la], lines[lb]
            u = (point_xy[a1][0] - point_xy[a0][0], point_xy[a1][1] - point_xy[a0][1])
            v = (point_xy[b1][0] - point_xy[b0][0], point_xy[b1][1] - point_xy[b0][1])
            # SolveSpace's angle is unsigned, between −A and B when `other`;
            # ours is signed from A to B, on the side SolveSpace solved to.
            mag = 180.0 - val if other else val
            sign = 1.0 if u[0] * v[1] - u[1] * v[0] >= 0 else -1.0
            out_c.append(["angle", la, lb, sign * mag])
        elif t == 121:
            out_c.append(["parallel", ent(ea, "line"), ent(eb, "line")])
        elif t == 122:
            out_c.append(["perpendicular", ent(ea, "line"), ent(eb, "line")])
        elif t == 123:
            out_c.append(["tangent", ent(ea, "arc"), ent(eb, "line")])
        elif t == 125:
            out_c.append(["tangent", ent(ea, "arc"), ent(eb, "arc")])
        elif t == 130:
            out_c.append(["equal", ent(ea, "arc", "circle"), ent(eb, "arc", "circle")])
        elif t == 200:
            out_c.append(["fix", point(pa)])
        else:
            raise Skip(f"has constraint type {t}, outside the v1 vocabulary")
    for f in fixed:
        out_c.insert(0, ["fix", f])
    case = {
        "name": "solvespace/" + rel[:-5],
        "from": "test/" + rel,
        "entities": out_entities,
        "construction": construction,
        "constraints": out_c,
        "solved": solved,
        "expect": {"status": "solved"},
    }
    if rel in UNSOLVED:
        # Not a saved solution: the test loads it unsolved and asserts that
        # it solves (test.cpp beside it).
        del case["solved"]
        case["note"] = UNSOLVED[rel]
    return case


# Fixtures stored unsolved on purpose, with what their test asserts.
UNSOLVED = {
    "constraint/large_dimensions/perpendicular_4m.slvs": (
        "The reporter's file as uploaded (issue #1354), not at its solution; "
        "upstream asserts that it solves (both lengths 4000, perpendicular, the "
        "second line's start on the first)."
    ),
}


def main():
    root = sys.argv[1]
    commit = subprocess.run(
        ["git", "-C", root, "rev-parse", "HEAD"], capture_output=True, text=True, check=True
    ).stdout.strip()
    cases, skipped = [], []
    test = os.path.join(root, "test")
    for sub in ("constraint", "request"):
        for dirpath, _, files in sorted(os.walk(os.path.join(test, sub))):
            for f in sorted(files):
                if not f.endswith(".slvs") or "_v20" in f or "_v22" in f:
                    continue
                rel = os.path.relpath(os.path.join(dirpath, f), test)
                try:
                    cases.append(convert(os.path.join(dirpath, f), rel))
                except Skip as e:
                    skipped.append({"from": "test/" + rel, "why": str(e)})
    json.dump(
        {
            "origin": "SolveSpace",
            "upstream": "https://github.com/solvespace/solvespace",
            "commit": commit,
            "licence": "GPL-3.0-or-later",
            "translated_by": "scripts/sketch-corpus-slvs.py",
            "cases": cases,
            "skipped": skipped,
        },
        sys.stdout,
        indent=1,
    )
    print()


if __name__ == "__main__":
    main()
