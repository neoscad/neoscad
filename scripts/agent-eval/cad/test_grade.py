#!/usr/bin/env python3
"""Tests for the CAD comparison grader (stdlib unittest).

    scripts/agent-eval/cad/test_grade.py [-v]
    NEOSCAD=/path/to/neoscad scripts/agent-eval/cad/test_grade.py

1. Synthetic meshes with known answers: a cube, a cube with a hole, two
   disjoint shells, a flipped face, an open mesh, a part off the bed.
2. The references in refs/ (rendered with the OpenSCAD nightly), which
   must pass, and wrong variants of them, which must fail the check the
   variant breaks and only that one.
3. Each condition's toolchain (OpenSCAD, NeoSCAD, CadQuery) making the
   same small part, graded alike; skipped when a tool is missing.

Every tool process runs under guard.run (2 GB, 120 s).
"""

import os
import struct
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
import grade  # noqa: E402
import guard  # noqa: E402
import stlmesh as sm  # noqa: E402

OPENSCAD = os.environ.get("OPENSCAD", "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD")
NEOSCAD = os.environ.get("NEOSCAD", str(ROOT / "target" / "release" / "neoscad"))
CQ_PY = os.environ.get("CADQUERY_PYTHON", str(ROOT / ".cache" / "agent-eval" / "cadquery-venv" / "bin" / "python"))
TMP = Path(tempfile.mkdtemp(prefix="cad-grade-test-"))


def write_stl(path, tris):
    with open(path, "wb") as f:
        f.write(b"\0" * 80 + struct.pack("<I", len(tris)))
        for t in tris:
            f.write(struct.pack("<12fH", 0, 0, 0, *t[0], *t[1], *t[2], 0))


def voxels(cells, offset=(0, 0, 0)):
    """The boundary of a union of unit voxels, outward-wound, two
    triangles per exposed face."""
    cells = set(cells)
    tris = []
    ox, oy, oz = offset
    for (x, y, z) in cells:
        for axis in range(3):
            for s in (-1, 1):
                n = [x, y, z]
                n[axis] += s
                if tuple(n) in cells:
                    continue
                u, v = (axis + 1) % 3, (axis + 2) % 3
                base = [x, y, z]
                if s > 0:
                    base[axis] += 1

                def corner(du, dv):
                    p = list(base)
                    p[u] += du
                    p[v] += dv
                    return (p[0] + ox, p[1] + oy, p[2] + oz)

                a, b, c, d = corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1)
                # (u, v) is right-handed about +axis, so a-b-c is
                # counter-clockwise seen from +axis.
                tris += [(a, b, c), (a, c, d)] if s > 0 else [(a, c, b), (a, d, c)]
    return tris


def topo(tris):
    p = TMP / "m.stl"
    write_stl(p, tris)
    return sm.topology(*sm.load_stl(p))


class Synthetic(unittest.TestCase):
    def test_cube(self):
        t = topo(voxels([(0, 0, 0)]))
        self.assertTrue(t["clean"])
        self.assertEqual((t["triangles"], t["components"], t["boundary_edges"], t["flipped_faces"]), (12, 1, 0, 0))
        self.assertAlmostEqual(t["volume"], 1)
        self.assertEqual(t["genus"], 0)

    def test_cube_with_hole(self):
        ring = [(x, y, 0) for x in range(3) for y in range(3) if (x, y) != (1, 1)]
        t = topo(voxels(ring))
        self.assertTrue(t["clean"])
        self.assertAlmostEqual(t["volume"], 8)
        self.assertEqual(t["genus"], 1)
        p = grade.Part(*sm.load_stl(TMP / "m.stl"))
        self.assertEqual(p.intervals(2, (1.5, 1.5, 0)), [])
        iv = p.intervals(0, (0, 1.5, 0.5))
        self.assertEqual([(round(a, 6), round(b, 6)) for a, b in iv], [(0, 1), (2, 3)])
        loops, chains = p.section(2, 0.5)
        self.assertEqual(sorted(round(sm.poly_area(l), 6) for l in loops), [-1, 9])
        self.assertEqual(chains, [])

    def test_two_shells(self):
        t = topo(voxels([(0, 0, 0)]) + voxels([(0, 0, 0)], offset=(3, 0, 0)))
        self.assertEqual(t["components"], 2)
        self.assertTrue(t["watertight"])
        self.assertFalse(t["clean"])
        self.assertAlmostEqual(t["volume"], 2)

    def test_flipped_face(self):
        tris = voxels([(0, 0, 0), (1, 0, 0)])
        a, b, c = tris[3]
        tris[3] = (a, c, b)
        t = topo(tris)
        self.assertEqual(t["flipped_faces"], 1)
        self.assertTrue(t["watertight"])  # topology alone cannot see a flip

    def test_inside_out(self):
        tris = [(a, c, b) for a, b, c in voxels([(0, 0, 0)])]
        t = topo(tris)
        self.assertEqual(t["flipped_faces"], 12)
        self.assertAlmostEqual(t["volume"], -1)

    def test_open_mesh(self):
        t = topo(voxels([(0, 0, 0)])[2:])
        self.assertFalse(t["watertight"])
        self.assertEqual(t["boundary_edges"], 4)
        self.assertFalse(t["clean"])

    def test_nonmanifold_edge(self):
        # Two cubes sharing only an edge.
        t = topo(voxels([(0, 0, 0)]) + voxels([(0, 0, 0)], offset=(1, 1, 0)))
        self.assertEqual(t["nonmanifold_edges"], 1)
        self.assertFalse(t["clean"])

    def test_off_bed(self):
        t = topo(voxels([(0, 0, 0)], offset=(0, 0, 0.5)))
        self.assertFalse(t["on_bed"])
        self.assertFalse(t["clean"])

    def test_ascii_welds_like_binary(self):
        tris = voxels([(0, 0, 0)])
        p = TMP / "a.stl"
        with open(p, "w") as f:
            f.write("solid t\n")
            for tri in tris:
                f.write(" facet normal 0 0 0\n  outer loop\n")
                for v in tri:
                    f.write(f"   vertex {v[0]:.6e} {v[1]:.6e} {v[2]:.6e}\n")
                f.write("  endloop\n endfacet\n")
            f.write("endsolid t\n")
        t = sm.topology(*sm.load_stl(p))
        self.assertTrue(t["clean"])
        self.assertEqual(t["vertices"], 8)


def render(tool, src, out, defines=()):
    if tool == "openscad":
        cmd = [OPENSCAD, "--backend=manifold", "-q", "-o", str(out)]
    else:
        cmd = [NEOSCAD, "-o", str(out)]
    for d in defines:
        cmd += ["-D", d]
    rc, _, err, _, g = guard.run(cmd + [str(src)], timeout=120)
    if rc != 0:
        raise RuntimeError(f"{tool} failed: {err[-800:]} {g}")
    return out


def graded(task, files):
    return grade.grade(task, {k: str(v) for k, v in files.items()})


@unittest.skipUnless(Path(OPENSCAD).exists(), "no OpenSCAD nightly")
class References(unittest.TestCase):
    """The references pass; each variant fails exactly the checks it breaks."""

    def ref(self, task, defines=(), tag="ref"):
        src = {"T1": "t1_bracket.scad", "T2": "t2_enclosure.scad", "T3": "t3_adapter.scad"}[task]
        if task == "T2":
            files = {part: render("openscad", HERE / "refs" / src, TMP / f"{tag}-{part}.stl",
                                  [f'part="{part}"', *defines]) for part in ("base", "lid")}
        else:
            files = {grade.PARTS[task][0]: render("openscad", HERE / "refs" / src, TMP / f"{tag}.stl", defines)}
        return graded(task, files)

    def assertFails(self, g, *names):
        failed = g["failed_gates"]
        for n in names:
            self.assertTrue(any(n in f for f in failed), f"expected {n!r} to fail; failed: {failed}")
        self.assertEqual(len(failed), len(names), f"only {names} should fail; failed: {failed}")

    def test_t1(self):
        g = self.ref("T1")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        self.assertFails(self.ref("T1", ["t=5"], "t1-thick"), "4 thick")
        self.assertFails(self.ref("T1", ['cs_face="outer"'], "t1-csout"), "inner face")
        self.assertFails(self.ref("T1", ["fillet=0"], "t1-nofillet"), "R4")
        self.assertFails(self.ref("T1", ["head=11"], "t1-head"), "head 9")

    def test_t2(self):
        g = self.ref("T2")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        self.assertFails(self.ref("T2", ["lip_clear=0.4"], "t2-lip"), "clearance")
        self.assertFails(self.ref("T2", ["wall=2.5"], "t2-wall"), "walls 2")
        self.assertFails(self.ref("T2", ["vents=4"], "t2-vents"), "vent")
        # The reference's lip follows the cavity, so only the cavity fails.
        self.assertFails(self.ref("T2", ["clear=0.2"], "t2-cav"), "cavity")

    def test_t3(self):
        g = self.ref("T3")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        self.assertFails(self.ref("T3", ["rings=true"], "t3-rings"), "helical", "right-hand")
        self.assertFails(self.ref("T3", ["hand=-1"], "t3-lh"), "right-hand")
        self.assertFails(self.ref("T3", ["pitch=1.5"], "t3-pitch"), "pitch")
        self.assertFails(self.ref("T3", ["af=32"], "t3-af"), "across flats")


T0_SCAD = "difference() { cube([20, 10, 4]); translate([10, 5, -1]) cylinder(d = 3, h = 6, $fn = 64); }\n"
T0_PY = """import cadquery as cq
r = cq.Workplane("XY").box(20, 10, 4, centered=False).faces(">Z").workplane(centerOption="CenterOfBoundBox").hole(3)
cq.exporters.export(r, "plate.stl")
"""


class Toolchains(unittest.TestCase):
    """The same small part from each condition's tool grades the same."""

    def check_plate(self, path):
        g = graded("T0", {"plate": path})
        self.assertTrue(g["pass"], g)
        self.assertAlmostEqual(g["parts"]["plate"]["volume"], 800 - 4 * 3.14159 * 1.5**2, delta=0.5)

    @unittest.skipUnless(Path(OPENSCAD).exists(), "no OpenSCAD nightly")
    def test_openscad(self):
        (TMP / "plate.scad").write_text(T0_SCAD)
        self.check_plate(render("openscad", TMP / "plate.scad", TMP / "plate-os.stl"))

    @unittest.skipUnless(Path(NEOSCAD).exists(), "no neoscad binary")
    def test_neoscad(self):
        (TMP / "plate.scad").write_text(T0_SCAD)
        self.check_plate(render("neoscad", TMP / "plate.scad", TMP / "plate-neo.stl"))

    @unittest.skipUnless(Path(CQ_PY).exists(), "no CadQuery venv (scripts/agent-eval/cad/setup-cadquery.sh)")
    def test_cadquery(self):
        d = TMP / "cq"
        d.mkdir(exist_ok=True)
        (d / "plate.py").write_text(T0_PY)
        rc, _, err, _, _ = guard.run([CQ_PY, "plate.py"], cwd=d, timeout=120)
        self.assertEqual(rc, 0, err)
        self.check_plate(d / "plate.stl")


def tearDownModule():
    import shutil
    shutil.rmtree(TMP, ignore_errors=True)


if __name__ == "__main__":
    unittest.main()
