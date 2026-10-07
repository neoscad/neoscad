#!/usr/bin/env python3
"""Writes the hand-translated corpus files:

    freecad/sketcher-solver.json      from FreeCAD's TestSketcherSolver.py
    solvespace/python-binding.json    from SolveSpace's python/tests/test.py

    python3 crates/sketch/tests/corpus/translate.py

Each case is a transcription of one upstream test: its geometry, its
constraints (in the solver's vocabulary, with the mapping noted where it
is not one to one), and what the upstream test asserts. The numbers are
the upstream ones; where an upstream test computes geometry (an arc from
a centre, radius and angles), this script computes it the same way.
SolveSpace's regression files are translated by
scripts/sketch-corpus-slvs.py instead.
"""

import json
import math
import os

HERE = os.path.dirname(os.path.abspath(__file__))

FREECAD_COMMIT = "e326ee2f07df04d4293035d65a98c96c3eb23380"
SOLVESPACE_COMMIT = "6909d19753817cd8b0b2a2f44acc225835232bc5"


class Case:
    """A sketch in corpus form, built FreeCAD-style: each curve has its own
    end points, joined by coincident constraints."""

    def __init__(self, name, source, test, note=""):
        self.d = {
            "name": name,
            "from": source,
            "test": test,
            "entities": [],
            "construction": [],
            "constraints": [],
            "expect": {"status": "solved"},
        }
        if note:
            self.d["note"] = note
        self.n = 0

    def point(self, name, x, y):
        self.d["entities"].append([name, "point", [x, y]])
        return name

    def line(self, name, a, b):
        p = self.point(name + ".start", *a)
        q = self.point(name + ".end", *b)
        self.d["entities"].append([name, "line", p, q])
        return name

    def arc(self, name, c, r, a0, a1):
        """FreeCAD's ArcOfCircle(Circle(c, r), a0, a1): counter-clockwise
        from angle a0 to a1 (radians)."""
        self.point(name + ".center", *c)
        self.point(name + ".start", c[0] + r * math.cos(a0), c[1] + r * math.sin(a0))
        self.point(name + ".end", c[0] + r * math.cos(a1), c[1] + r * math.sin(a1))
        self.d["entities"].append([name, "arc", name + ".center", name + ".start", name + ".end"])
        return name

    def circle(self, name, c, r):
        self.point(name + ".center", *c)
        self.d["entities"].append([name, "circle", name + ".center", r])
        return name

    def fixed_point(self, name, x, y):
        self.point(name, x, y)
        self.c("fix", name)
        return name

    def axis(self, which):
        """FreeCAD's H_Axis (-1) or V_Axis (-2), and the root point (-1, 1):
        fixed construction geometry through the origin."""
        if which == "root":
            if "root" not in [e[0] for e in self.d["entities"]]:
                self.fixed_point("root", 0.0, 0.0)
            return "root"
        name = {"h": "H_Axis", "v": "V_Axis"}[which]
        if name not in [e[0] for e in self.d["entities"]]:
            end = (1.0, 0.0) if which == "h" else (0.0, 1.0)
            self.line(name, (0.0, 0.0), end)
            self.d["construction"].append(name)
            self.c("fix", name)
        return name

    def c(self, *args):
        self.d["constraints"].append(list(args))
        return len(self.d["constraints"]) - 1

    def pos(self, geo, pos):
        """FreeCAD's (geometry, position) pair: 1 start, 2 end, 3 centre."""
        return geo + {1: ".start", 2: ".end", 3: ".center"}[pos]


def freecad():
    src = "src/Mod/Sketcher/SketcherTests/TestSketcherSolver.py"
    cases = []

    def box(k, lines):
        for i in range(4):
            k.c("coincident", k.pos(lines[i], 2), k.pos(lines[(i + 1) % 4], 1))
        k.c("horizontal", lines[0])
        k.c("horizontal", lines[2])
        k.c("vertical", lines[1])
        k.c("vertical", lines[3])

    # testBoxCase: CreateBoxSketchSet, then DistanceX/DistanceY on line 1's
    # end. The test drags a point between the two (moveGeometry), which is
    # interactive and has no counterpart; the final constraint set is solved
    # from the original drawing.
    k = Case("freecad/testBoxCase", src, "TestSketcherSolver.testBoxCase")
    tl, tr = (-99.230339, 36.960674), (69.432587, 36.960674)
    br, bl = (69.432587, -53.196629), (-99.230339, -53.196629)
    ls = [k.line("l0", tl, tr), k.line("l1", tr, br), k.line("l2", br, bl), k.line("l3", bl, tl)]
    box(k, ls)
    k.c("length", "l1", 81.370787)
    k.c("length", "l0", 187.573036)
    root = k.axis("root")
    k.c("distance_x", root, "l1.end", 90.0)
    k.c("distance_y", root, "l1.end", -50.0)
    k.d["note"] = "FreeCAD's 'Distance' on one line is its length; DistanceX/Y on one point is measured from the root point."
    k.d["expect"].update(
        dof=0,
        points={
            "l1.end": [90.0, -50.0],
            "l1.start": [90.0, -50.0 + 81.370787],
            "l0.start": [90.0 - 187.573036, -50.0 + 81.370787],
            "l2.end": [90.0 - 187.573036, -50.0],
        },
        tol=1e-9,
    )
    cases.append(k.d)

    # testSlotCase, first half: CreateSlotPlateSet.
    k = Case("freecad/testSlotCase/SlotPlateSet", src, "TestSketcherSolver.testSlotCase")
    k.line("l0", (60.029362, -30.279360), (-120.376335, -30.279360))
    k.c("horizontal", "l0")
    k.line("l1", (-120.376335, -30.279360), (-70.193062, 38.113884))
    k.c("coincident", "l0.end", "l1.start")
    k.line("l2", (-70.193062, 38.113884), (60.241116, 37.478645))
    k.c("coincident", "l1.end", "l2.start")
    k.c("horizontal", "l2")
    k.arc("a3", (60.039921, 3.811391), 35.127132, -1.403763, 1.419522)
    # Tangent(2, 2, 3, 2): endpoint-to-endpoint tangency.
    k.c("coincident", "l2.end", "a3.end")
    k.c("tangent", "l2", "a3")
    k.c("coincident", "l0.start", "a3.start")
    k.c("tangent", "l0", "a3")
    # Angle(0, 2, 1, 1) at the shared vertex, set to 0.872665 rad (50°)
    # between the rays from the vertex. Ours is signed from l0's direction
    # (start to end, pointing away from that ray) to l1's: 50° - 180°.
    k.c("angle", "l0", "l1", math.degrees(0.872665) - 180.0)
    k.c("length", "l0", 200.0)
    k.c("radius", "a3", 40.0)
    root = k.axis("root")
    k.c("distance_x", root, "l0.end", 0.0)
    k.c("distance_y", root, "l0.end", 0.0)
    k.d["note"] = (
        "Datums as finally set (setDatum 7 = 200, 8 = 40, 6 = 0.872665 rad, 9 = 0, 10 = 0); "
        "the test's moveGeometry drags are interactive and are not translated. "
        "Endpoint tangency is a coincident plus a tangent. Upstream asserts the shape has 4 edges, i.e. that it solves."
    )
    k.d["expect"].update(dof=0, points={"l0.end": [0.0, 0.0], "l0.start": [200.0, 0.0]}, tol=1e-9)
    cases.append(k.d)

    # testSlotCase, second half: CreateSlotPlateInnerSet added (geometry 4..8).
    inner = Case("freecad/testSlotCase/SlotPlateInnerSet", src, "TestSketcherSolver.testSlotCase")
    inner.d["entities"] = [list(e) for e in k.d["entities"]]
    inner.d["constraints"] = [list(c) for c in k.d["constraints"]]
    inner.n = k.n
    inner.circle("c4", (195.055893, 39.562252), 29.846098)
    inner.line("l5", (150.319031, 13.449363), (36.700474, 13.139774))
    inner.c("horizontal", "l5")
    inner.line("l6", (36.700474, 13.139774), (77.566010, 63.292927))
    inner.c("coincident", "l5.end", "l6.start")
    inner.line("l7", (77.566010, 63.292927), (148.151917, 63.602505))
    inner.c("coincident", "l6.end", "l7.start")
    inner.c("horizontal", "l7")
    inner.c("parallel", "l1", "l6")
    inner.arc("a8", (192.422913, 38.216347), 45.315174, 2.635158, 3.602228)
    inner.c("coincident", "l7.end", "a8.start")
    inner.c("coincident", "a8.end", "l5.start")
    inner.d["note"] = k.d["note"] + " Upstream asserts 9 edges after adding the inner set, i.e. that it still solves."
    # Inner set: circle 3 + lines 12 + arc 6 = 21 unknowns; 1 + 2 + 2 + 1 + 1 + 2 + 2 + 1 (arc) = 12.
    inner.d["expect"] = {"status": "solved", "dof": 9}
    cases.append(inner.d)

    # testIssue3245: three horizontal lines starting on the V axis, with
    # DistanceX expressions 60, 65 and 70 (the expression engine is not
    # part of the solver; the expressions' values are used).
    k = Case("freecad/testIssue3245", src, "TestSketcherSolver.testIssue3245")
    v = k.axis("v")
    k.line("l0", (-1.195999, 56.041161), (60.654316, 56.382877))
    k.c("on", "l0.start", v)
    k.c("horizontal", "l0")
    k.line("l1", (0.512583, 32.121155), (60.654316, 31.779440))
    k.c("horizontal", "l1")
    k.line("l2", (0.170867, 13.326859), (61.679455, 13.326859))
    k.c("on", "l2.start", v)
    k.c("horizontal", "l2")
    k.c("on", "l1.start", v)
    k.c("distance_x", "l0.start", "l0.end", 60.0)
    k.c("distance_x", "l1.start", "l1.end", 65.0)
    k.c("distance_x", "l2.start", "l2.end", 70.0)
    k.d["note"] = "Upstream tests the expression engine after delGeometry; the solve before it is what is translated."
    k.d["expect"].update(dof=3, checks=[["x", "l0.end", 60.0], ["x", "l1.end", 65.0], ["x", "l2.end", 70.0]])
    cases.append(k.d)

    # testIssue3245_2: a box with DistanceY(3, 1, 3, 2) = 5 (the DistanceX
    # is deleted before the final recompute).
    k = Case("freecad/testIssue3245_2", src, "TestSketcherSolver.testIssue3245_2")
    a, b = (-23.574591, 42.399727), (81.949776, 42.399727)
    c, d = (81.949776, -19.256901), (-23.574591, -19.256901)
    ls = [k.line("l0", a, b), k.line("l1", b, c), k.line("l2", c, d), k.line("l3", d, a)]
    box(k, ls)
    k.c("distance_y", "l3.start", "l3.end", 5.0)
    k.d["expect"].update(dof=3, checks=[["dy", "l3.start", "l3.end", 5.0]])
    cases.append(k.d)

    # testMissingVerticalHorizontalConstraints: the solve after the
    # constraints exist (detection itself is not solver behaviour).
    k = Case("freecad/testMissingVerticalHorizontalConstraints", src, "TestSketcherSolver.testMissingVerticalHorizontalConstraints")
    k.line("l0", (0.0, 0.0), (10.0, 0.0))
    k.line("l1", (20.0, 0.0), (20.0, 10.0))
    k.c("horizontal", "l0")
    k.c("vertical", "l1")
    k.d["expect"].update(dof=6, points={"l0.end": [10.0, 0.0], "l1.end": [20.0, 10.0]}, tol=0.0)
    cases.append(k.d)

    # testPointToLineDistanceSigned: a signed point-line distance must not
    # flip when DistanceX goes from 25 to 100.
    k = Case("freecad/testPointToLineDistanceSigned", src, "TestSketcherSolver.testPointToLineDistanceSigned")
    k.line("lv", (0.0, -25.0), (0.0, 0.0))
    k.line("lh", (0.0, 0.0), (25.0, 0.0))
    k.line("ln", (0.0, -25.0), (25.0, 0.0))
    k.point("pt", 20.0, -20.0)
    k.c("on", "lv.start", k.axis("v"))
    k.c("coincident", "lv.end", k.axis("root"))
    k.c("coincident", "lh.start", "lv.end")
    k.c("on", "lh.end", k.axis("h"))
    k.c("coincident", "ln.start", "lv.start")
    k.c("coincident", "ln.end", "lh.end")
    k.c("equal", "lv", "lh")
    i = k.c("distance_x", "lh.start", "lh.end", 25.0)
    k.c("distance", "pt", "ln", 5.0)
    orient = ["cw", "pt", "ln.start", "ln.end"]
    k.d["expect"].update(checks=[orient])
    k.d["stages"] = [{"set": [[i, 100.0]], "expect": {"status": "solved", "checks": [orient]}}]
    k.d["note"] = (
        "Upstream re-solves from the previous solution (FreeCAD keeps geometry between solves); "
        "this solver always starts from the drawing, which is the harder test. "
        "'ccw == False' upstream is the 'cw' check here."
    )
    cases.append(k.d)

    # testCircleLineTangentOriented: arcs and circles tangent to the axes
    # must stay in their quadrants (FreeCAD issue 29007).
    k = Case("freecad/testCircleLineTangentOriented", src, "TestSketcherSolver.testCircleLineTangentOriented")
    k.arc("q1", (25.0, 25.0), 10.0, -1.403763, 1.419522)
    k.arc("q2", (-25.0, 25.0), 10.0, -1.403763, 1.419522)
    k.circle("q3", (-25.0, -25.0), 10.0)
    k.circle("q4", (25.0, -25.0), 10.0)
    v, h = k.axis("v"), k.axis("h")
    k.c("tangent", "q1", v)
    k.c("tangent", "q2", v)
    k.c("tangent", "q3", h)
    k.c("tangent", "q4", h)
    k.d["expect"].update(
        checks=[
            ["x>", "q1.center", 0.0], ["y>", "q1.center", 0.0],
            ["x<", "q2.center", 0.0], ["y>", "q2.center", 0.0],
            ["x<", "q3.center", 0.0], ["y<", "q3.center", 0.0],
            ["x>", "q4.center", 0.0], ["y<", "q4.center", 0.0],
        ],
        # Not an upstream assertion: the arcs' radii are free, and as q2's
        # grows (10 to 16.3) its end points stay near where they were drawn,
        # so its sweep goes from 162° to 225° and the solver reports the
        # arc as crossing 180°. FreeCAD keeps an arc's angles as unknowns,
        # so its arcs keep their extent instead.
        allow_flipped=["ArcSweep"],
    )
    cases.append(k.d)

    skipped = [
        {"test": "testCircleToLineDistance_Driving_Passant, _Driving_Secant, _Reference_Secant, _Legacy_Negative, testCircleToLineDistanceOriented",
         "why": "circle-to-line distance is not in the v1 vocabulary (distance is point-point, point-line, line-line)"},
        {"test": "testCircleToCircleDistanceOriented", "why": "circle-to-circle distance is not in the v1 vocabulary"},
        {"test": "testBlockConstraintEllipse", "why": "ellipses and Block constraints are not in v1"},
        {"test": "testPointGeometryExtension, testThreeLinesWithCoincidences_1/_2", "why": "geometry extensions and coincidence detection, not solving"},
        {"test": "testLegacyExternalGeometryOrientationMigration, testSymmetricKeepsConstraintOnTheMirroredSide, testReversedExternalGeometryLeavesSketchInPlace, testRemovedExternalGeometryReference, testSaveLoadWithExternalGeometryReference, testTNPExternalGeometryStored, testConstructionToggleTNP",
         "why": "external geometry and document persistence; not in v1"},
        {"test": "src/Mod/Sketcher/SketcherTests/TestSketchFillet.py", "why": "FreeCAD's fillet tool (trimming plus new constraints) is an editing operation; NeoSCAD's fillet is applied after the solve (stage 2)"},
        {"test": "src/Mod/Sketcher/SketcherTests/TestSketchValidateCoincidents.py", "why": "validation tooling, not solving"},
        {"test": "tests/src/Mod/Sketcher/App/planegcs/Constraints.cpp (tangentBSplineAndArc)", "why": "B-splines are not in v1"},
        {"test": "tests/src/Mod/Sketcher/App/planegcs/GCS.cpp (clearConstraints)", "why": "API bookkeeping, no solve"},
        {"test": "tests/src/Mod/Sketcher/App/SketchObject*.cpp, Constraint.cpp", "why": "geometry editing, element naming and serialization; no solver assertions"},
    ]
    return {
        "origin": "FreeCAD",
        "upstream": "https://github.com/FreeCAD/FreeCAD",
        "commit": FREECAD_COMMIT,
        "licence": "LGPL-2.1-or-later",
        "translated_by": "crates/sketch/tests/corpus/translate.py",
        "cases": cases,
        "skipped": skipped,
    }


def solvespace():
    src = "python/tests/test.py"
    cases = []

    k = Case("solvespace/python/test_crank_rocker", src, "CoreTest.test_crank_rocker")
    k.fixed_point("p0", 0.0, 0.0)
    k.fixed_point("p1", 90.0, 0.0)
    k.d["entities"].append(["line0", "line", "p0", "p1"])
    k.point("p2", 20.0, 20.0)
    k.point("p3", 0.0, 10.0)
    k.point("p4", 30.0, 20.0)
    k.c("distance", "p2", "p3", 40.0)
    k.c("distance", "p2", "p4", 40.0)
    k.c("distance", "p3", "p4", 70.0)
    k.c("distance", "p0", "p3", 35.0)
    k.c("distance", "p1", "p4", 70.0)
    k.d["entities"].append(["line1", "line", "p0", "p3"])
    # slvs.angle is unsigned; the drawing turns counter-clockwise from
    # line0 to line1, so ours is +45.
    k.c("angle", "line0", "line1", 45.0)
    k.d["expect"].update(dof=0, points={"p2": [39.54852, 61.91009]}, tol=1e-4)
    cases.append(k.d)

    k = Case("solvespace/python/test_involute", src, "CoreTest.test_involute")
    k.fixed_point("p0", 0.0, 0.0)
    k.point("p1", 0.0, 10.0)
    k.c("distance", "p0", "p1", 10.0)
    k.d["entities"].append(["line0", "line", "p0", "p1"])
    k.point("p2", 10.0, 10.0)
    k.d["entities"].append(["line1", "line", "p1", "p2"])
    k.c("distance", "p1", "p2", 10.0 * math.radians(45.0))
    k.c("perpendicular", "line0", "line1")
    k.fixed_point("p3", 10.0, 0.0)
    k.d["entities"].append(["line_base", "line", "p0", "p3"])
    # Unsigned 45° upstream; drawn clockwise from line0 to line_base.
    k.c("angle", "line0", "line_base", -45.0)
    k.d["expect"].update(dof=0, points={"p2": [12.62467, 1.51746]}, tol=1e-4)
    cases.append(k.d)

    k = Case("solvespace/python/test_jansen_linkage", src, "CoreTest.test_jansen_linkage")
    k.fixed_point("p0", 0.0, 0.0)
    k.point("p1", 0.0, 20.0)
    k.c("distance", "p0", "p1", 15.0)
    k.d["entities"].append(["line0", "line", "p0", "p1"])
    k.fixed_point("p2", -38.0, -7.8)
    k.point("p3", -50.0, 30.0)
    k.point("p4", -70.0, -15.0)
    k.c("distance", "p2", "p3", 41.5)
    k.c("distance", "p3", "p4", 55.8)
    k.c("distance", "p2", "p4", 40.1)
    k.point("p5", -50.0, -50.0)
    k.point("p6", -10.0, -90.0)
    k.point("p7", -20.0, -40.0)
    k.c("distance", "p5", "p6", 65.7)
    k.c("distance", "p6", "p7", 49.0)
    k.c("distance", "p5", "p7", 36.7)
    k.c("distance", "p1", "p3", 50.0)
    k.c("distance", "p1", "p7", 61.9)
    k.point("p8", 20.0, 0.0)
    k.d["entities"].append(["line_base", "line", "p0", "p8"])
    k.c("angle", "line0", "line_base", -45.0)
    k.d["note"] = (
        "Upstream asserts p8 = (18.93036, 13.63778), but the linkage is under-constrained "
        "(the crank, and p8 along line_base, are free), so that value is where SolveSpace's "
        "solver happens to leave it: informational only."
    )
    k.d["informational"] = {"points": {"p8": [18.93036, 13.63778]}}
    cases.append(k.d)

    k = Case("solvespace/python/test_nut_cracker", src, "CoreTest.test_nut_cracker")
    h0, b0, r0, n1, n2 = 0.5, 0.75, 0.25, 1.5, 2.3
    k.fixed_point("p0", 0.0, 0.0)
    k.point("p1", 2.0, 2.0)
    k.point("p2", 2.0, 0.0)
    k.d["entities"].append(["line0", "line", "p0", "p2"])
    k.c("horizontal", "line0")
    k.d["entities"].append(["line1", "line", "p1", "p2"])
    k.fixed_point("p3", b0 / 2, h0)
    k.c("distance", "p3", "line1", r0)
    k.c("distance", "p0", "p1", n1)
    k.c("distance", "p1", "p2", n2)
    k.d["expect"].update(dof=0, checks=[["x", "p2", 1.01576 + b0 / 2, 1e-4]])
    cases.append(k.d)

    k = Case("solvespace/python/test_pydemo", src, "CoreTest.test_pydemo")
    k.fixed_point("p101", 0.0, 0.0)
    k.point("p301", 10.0, 20.0)
    k.point("p302", 20.0, 10.0)
    k.d["entities"].append(["l400", "line", "p301", "p302"])
    k.point("p303", 100.0, 120.0)
    k.point("p304", 120.0, 110.0)
    k.point("p305", 115.0, 115.0)
    k.d["entities"].append(["a401", "arc", "p303", "p304", "p305"])
    k.point("p306", 200.0, 200.0)
    k.d["entities"].append(["c402", "circle", "p306", 30.0])
    k.c("distance", "p301", "p302", 30.0)
    k.c("distance", "p101", "l400", 10.0)
    k.c("vertical", "l400")
    k.c("distance", "p301", "p101", 15.0)
    k.c("equal", "a401", "c402")
    k.c("diameter", "a401", 34.0)
    k.d["note"] = (
        "Group 1's workplane is the fixed origin p101. The arc's points are under-constrained "
        "(only its radius is set), so their upstream values depend on SolveSpace's choice among "
        "solutions: informational only."
    )
    k.d["expect"].update(
        dof=6,
        points={"p301": [10.0, 11.18030], "p302": [10.0, -18.81966], "p306": [200.0, 200.0]},
        radii={"c402": 17.0, "a401": 17.0},
        tol=1e-4,
    )
    k.d["informational"] = {
        "points": {
            "p303": [101.11418, 119.04153],
            "p304": [116.47661, 111.76171],
            "p305": [117.40922, 114.19676],
        }
    }
    cases.append(k.d)

    return {
        "origin": "SolveSpace",
        "upstream": "https://github.com/solvespace/solvespace",
        "commit": SOLVESPACE_COMMIT,
        "licence": "GPL-3.0-or-later",
        "translated_by": "crates/sketch/tests/corpus/translate.py",
        "cases": cases,
        "skipped": [],
    }


def write(rel, data):
    with open(os.path.join(HERE, rel), "w") as f:
        json.dump(data, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    write("freecad/sketcher-solver.json", freecad())
    write("solvespace/python-binding.json", solvespace())
