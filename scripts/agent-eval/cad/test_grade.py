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

    def test_collapsed_sliver_is_not_a_component(self):
        # A sliver 3e-9 wide in the exporter's doubles (as where a
        # Clipper-snapped 13.200000002980232 meets a cube face at 13.2)
        # welds to a zero-area face through float32. It is degenerate, but
        # not a second body.
        tris = voxels([(0, 0, 0)])
        a, b = tris[0][0], tris[0][1]
        tris.append((a, b, (b[0], b[1] + 3e-9, b[2])))
        p = TMP / "sliver.stl"
        with open(p, "w") as f:
            f.write("solid t\n")
            for tri in tris:
                f.write(" facet normal 0 0 0\n  outer loop\n")
                for v in tri:
                    f.write(f"   vertex {v[0]!r} {v[1]!r} {v[2]!r}\n")
                f.write("  endloop\n endfacet\n")
            f.write("endsolid t\n")
        t = sm.topology(*sm.load_stl(p))
        self.assertEqual((t["degenerate_faces"], t["components"], t["genus"]), (1, 1, 0))
        self.assertTrue(t["clean"], t["reasons"])

    def test_collapsed_face_keeps_section_loops(self):
        # Exported meshes carry faces that weld to two corners through
        # float32 (a bracket's countersunk holes had 272). One on a hole's
        # wall, across the plane, must not split the hole's loop into open
        # chains, or the hole is not counted.
        ring = [(x, y, 0) for x in range(3) for y in range(3) if (x, y) != (1, 1)]
        tris = voxels(ring)
        a, b = (1, 1, 0), (1, 1, 1)  # a vertical edge of the hole
        tris.append((a, (a[0] + 3e-9, a[1], a[2]), b))
        p = TMP / "collapsed.stl"
        with open(p, "w") as f:
            f.write("solid t\n")
            for tri in tris:
                f.write(" facet normal 0 0 0\n  outer loop\n")
                for v in tri:
                    f.write(f"   vertex {float(v[0])!r} {float(v[1])!r} {float(v[2])!r}\n")
                f.write("  endloop\n endfacet\n")
            f.write("endsolid t\n")
        verts, faces = sm.load_stl(p)
        self.assertEqual(sum(1 for t in faces if len(set(t)) < 3), 1)
        loops, chains = grade.Part(verts, faces).section(2, 0.5)
        self.assertEqual(sorted(round(sm.poly_area(l), 6) for l in loops), [-1, 9])
        self.assertEqual(chains, [])

    def test_circumscribed_diameter_of_a_faceted_circle(self):
        import math
        for n in (6, 16, 64):
            poly = [(10 + 0.75 * math.cos(2 * math.pi * i / n), 5 + 0.75 * math.sin(2 * math.pi * i / n))
                    for i in range(n)]
            self.assertAlmostEqual(sm.circum_diameter(poly), 1.5, places=9)
            self.assertLess(sm.equiv_diameter(poly), 1.5)

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
        src = {"T1": "t1_bracket.scad", "T2": "t2_enclosure.scad", "T3": "t3_adapter.scad",
               "T4": "t4_hinge.scad", "T5": "t5_knob.scad", "T6": "t6_gears.scad"}[task]
        if len(grade.PARTS[task]) > 1:
            files = {part: render("openscad", HERE / "refs" / src, TMP / f"{tag}-{part}.stl",
                                  [f'part="{part}"', *defines]) for part in grade.PARTS[task]}
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
        # A lip relieved 0.8 per side for snap clearance, with a 1 mm catch
        # band at 0.2, is located by that band (cad-20260928T231444Z's
        # OpenSCAD lid); relieved with its band at 0.4, it still fails.
        g = self.ref("T2", ["lip_relief=0.8"], "t2-relief")
        self.assertTrue(g["pass"], g["failed_gates"])
        self.assertFails(self.ref("T2", ["lip_relief=0.8", "lip_clear=0.4"], "t2-relief-wide"), "clearance")

    def test_t2_faceted_pilots_and_short_slots(self):
        # A 1.5 pilot is 1.5 across its facets' corners at any $fn; its
        # area-equivalent diameter (1.499 at $fn = 64, 1.481 at 16) once
        # failed the 1.5-1.8 window on a correct part. 1.4 is too small.
        for fn in (64, 16):
            g = self.ref("T2", ["post_hole=1.5", f"post_fn={fn}"], f"t2-pilot-{fn}")
            self.assertTrue(g["pass"], (fn, g["failed_gates"]))
            posts = next(c for c in g["checks"] if "M2 posts" in c["name"])["value"]
            self.assertTrue(all(abs(d - 1.5) < 0.005 for d in posts), (fn, posts))
        self.assertFails(self.ref("T2", ["post_hole=1.4"], "t2-pilot-small"), "M2 posts")
        # Vents 4.5 x 2.5 (1.8:1) are slots; 3 x 2.5 (1.2:1) are not.
        g = self.ref("T2", ["vent=[4.5, 2.5]"], "t2-vent-short")
        self.assertTrue(g["pass"], g["failed_gates"])
        self.assertFails(self.ref("T2", ["vent=[3, 2.5]"], "t2-vent-square"), "vent")

    def test_t3(self):
        g = self.ref("T3")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        self.assertFails(self.ref("T3", ["rings=true"], "t3-rings"), "helical", "right-hand")
        self.assertFails(self.ref("T3", ["hand=-1"], "t3-lh"), "right-hand")
        self.assertFails(self.ref("T3", ["pitch=1.5"], "t3-pitch"), "pitch")
        self.assertFails(self.ref("T3", ["af=32"], "t3-af"), "across flats")
        # A 45-degree skirt under the flange is the thread's neighbour, not
        # thread: its first mm are narrower than the flange test's radius,
        # and counting them failed length, major and pitch on correct
        # threads. These are shapes agents really made: a cone from the
        # root, a hull of root circle and hexagon, and a cone starting 1 mm
        # inside the thread. A six-sided cone whose flats start inside the
        # root once pulled the root estimate under the thread's and failed
        # five thread gates on correct parts.
        for defs, tag in ((['skirt="cone"'], "t3-cone"), (['skirt="hull"'], "t3-hull"),
                          (['skirt="cone"', "skirt_dz=-1"], "t3-overlap"),
                          (['skirt="hexcone"'], "t3-hexcone")):
            g = self.ref("T3", defs, tag)
            self.assertTrue(g["pass"], (tag, g["failed_gates"]))
            length = next(c for c in g["checks"] if c["name"].startswith("thread 12 long"))["value"]
            self.assertGreater(length["thread_like_levels"][1] - length["thread_like_levels"][0], 13, tag)
        self.assertFails(self.ref("T3", ['skirt="cone"', "thread_len=10"], "t3-short"), "12 long")

    def test_t3_layouts(self):
        # A thread between the hex and the barb (cad-20260928T231444Z's
        # NeoSCAD part) meets every stated number but has no free end, so it
        # fails the layout gate and only that. Upright or turned over, the
        # usable order passes, and the stacked part's other checks still work.
        hx = ['order="hex-thread-barb"']
        g = self.ref("T3", ["flip=true"], "t3-flip")
        self.assertTrue(g["pass"], g["failed_gates"])
        g = self.ref("T3", hx, "t3-htb")
        self.assertFails(g, "layout")
        layout = next(c for c in g["checks"] if c["name"].startswith("layout"))["value"]
        self.assertEqual(layout["order_along_axis"], ["hex", "thread", "barb"])
        self.assertFalse(layout["thread_at_a_free_end"])
        self.assertFails(self.ref("T3", hx + ["thread_len=10"], "t3-htb-short"), "12 long", "layout")
        self.assertFails(self.ref("T3", hx + ["barb_len=20"], "t3-htb-barb"), "barb 25", "layout")
        self.assertFails(self.ref("T3", hx + ["pitch=1.5"], "t3-htb-pitch"), "pitch", "layout")
        self.assertFails(self.ref("T3", hx + ["af=32"], "t3-htb-af"), "across flats", "layout")
        self.assertFails(self.ref("T3", hx + ["rings=true"], "t3-htb-rings"), "helical", "right-hand", "layout")
        # Corners chamfered over 5 of the flange's 8 mm (CadQuery's part
        # there) are still a 30 hex; a round flange is not.
        g = self.ref("T3", ["hex_cham=5"], "t3-cham")
        self.assertTrue(g["pass"], g["failed_gates"])
        self.assertFails(self.ref("T3", ["hex_cham=16"], "t3-round"), "across flats")
        # A plain stem and tip chamfer after the barbs is not three more
        # barbs (OpenSCAD's part there); two barbs are not three.
        g = self.ref("T3", ["barb_stem=5"], "t3-stem")
        self.assertTrue(g["pass"], g["failed_gates"])
        self.assertFails(self.ref("T3", ["barb_stem=5", "barbs=2"], "t3-two"), "three barbs")

    def test_t3_flares(self):
        # A 45-degree flare between the flange and the barbs is neither a
        # barb nor, necessarily, barb length: an 8 mm skirt before 25 mm of
        # barbs and a 1.8 mm root fillet inside the 25 both pass.
        for defs, tag in ((["barb_flare=8"], "t3-flare"), (["barb_fillet=1.8"], "t3-fillet"),
                          (["barb_flare=8", "flip=true"], "t3-flare-flip")):
            g = self.ref("T3", defs, tag)
            self.assertTrue(g["pass"], (tag, g["failed_gates"]))
            barbs = next(c for c in g["checks"] if c["name"] == "three barbs")["value"]
            self.assertEqual(len(barbs["peaks"]), 3, (tag, barbs))
        # It does not hide a fourth barb or a short barb.
        self.assertFails(self.ref("T3", ["barb_flare=8", "barbs=4"], "t3-flare-four"), "three barbs")
        self.assertFails(self.ref("T3", ["barb_flare=8", "barb_len=20"], "t3-flare-short"), "barb 25")

    # Held-out tasks T4-T6: each reference passes, also turned or printed
    # the other way up, and each wrong variant fails its own check.

    def test_t4(self):
        g = self.ref("T4")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        # The hinge along y instead of x, and the box turned round.
        for turn in (90, 180):
            g = self.ref("T4", [f"turn={turn}"], f"t4-turn{turn}")
            self.assertTrue(g["pass"], (turn, g["failed_gates"]))
        # Knuckle ribs down the back wall are hinge, and the box is still
        # 70 x 45 when they cover most of it (20 of every 24.8 here, where
        # a median of the rays read the back face 2 out). A 47 deep box
        # (and lid) is not.
        for ribs in (12, 20):
            g = self.ref("T4", [f"ribs={ribs}"], f"t4-ribs{ribs}")
            self.assertTrue(g["pass"], (ribs, g["failed_gates"]))
        self.assertFails(self.ref("T4", ["size=[70, 47, 30]"], "t4-deep"), "body 70 x 45", "lid plate")
        self.assertFails(self.ref("T4", ["wall=2.5"], "t4-wall"), "walls 2")
        self.assertFails(self.ref("T4", ["kn_d=8"], "t4-od"), "outer diameter")
        self.assertFails(self.ref("T4", ["hole=3"], "t4-hole"), "pin hole")
        self.assertFails(self.ref("T4", ["gap=0.8"], "t4-gap"), "0.4 between")
        # Lid knuckles 1 lower when closed: each part is right on its own,
        # but the pin cannot pass both.
        self.assertFails(self.ref("T4", ["lid_dz=-1"], "t4-axis"), "axis on the body")

    def test_t5(self):
        g = self.ref("T5")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        g = self.ref("T5", ["flip=false"], "t5-bore-down")
        self.assertTrue(g["pass"], g["failed_gates"])
        self.assertFails(self.ref("T5", ["flutes=18"], "t5-flutes"), "20 grip flutes")
        self.assertFails(self.ref("T5", ["flute_d=2"], "t5-flute-depth"), "1 deep")
        self.assertFails(self.ref("T5", ["flat=4.2"], "t5-flat"), "D bore")
        self.assertFails(self.ref("T5", ["depth=14"], "t5-depth"), "12 deep")
        self.assertFails(self.ref("T5", ["depth=18"], "t5-through"), "12 deep")
        self.assertFails(self.ref("T5", ["screw_z=7"], "t5-screw"), "set-screw")

    def test_t6(self):
        g = self.ref("T6")
        self.assertTrue(g["pass"], g["failed_gates"] or g["parts"])
        # Thinner teeth (more backlash) still turn; a 25-degree pressure
        # angle is not measured (the spec's 20 is not gated).
        for d in ("fat=-0.5", "pa=25"):
            g = self.ref("T6", [d], f"t6-{d}")
            self.assertTrue(g["pass"], (d, g["failed_gates"]))
        self.assertFails(self.ref("T6", ["dist=36"], "t6-dist"), "34 apart")
        self.assertFails(self.ref("T6", ["axle_d=6"], "t6-axle"), "axles")
        self.assertFails(self.ref("T6", ["bore=5"], "t6-bore"), "bores")
        self.assertFails(self.ref("T6", ["thick=7"], "t6-thick"), "6 thick")
        self.assertFails(self.ref("T6", ["plate_t=5"], "t6-plate"), "plate 80")
        # Teeth 0.1 mm too thick on the big gear bind at 34.
        self.assertFails(self.ref("T6", ["fat=0.25"], "t6-fat"), "mesh")
        self.assertFails(self.ref("T6", ["small_z=16"], "t6-z16"), "15 teeth", "outside diameter 25.5", "mesh")


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
