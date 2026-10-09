#!/usr/bin/env python3
"""Tests for the progress snapshots and the hero image's layout (stdlib
unittest, synthetic runs only: no model calls, no recorded results).

    scripts/agent-eval/cad/test_progress.py [-v]

1. The representative-run rule (progress.representative).
2. The shared time axis: sample times, the axis mapping, the state shown
   at a clock time, the animation clock.
3. Stream accounting and the replay of Write/Edit calls that backfill
   rebuilds sources from.
4. Source frames: which saved .scad states get a frame or are
   incomplete, what a render may enable, the STL/source timeline, and
   hero.render_run driving a stand-in neoscad (no real rendering).
5. The capture side: run_cad's Watcher keeping source versions and
   write_progress writing progress.json.
"""

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import progress as pg  # noqa: E402


def run(i, passed, wall):
    return {"id": f"r{i}", "pass": passed, "wall_s": wall}


class Representative(unittest.TestCase):
    def test_median_of_passing_runs(self):
        runs = [run(1, True, 300), run(2, False, 10), run(3, True, 100), run(4, True, 200), run(5, False, 999)]
        chosen, rule = pg.representative(runs)
        self.assertEqual(chosen["id"], "r4")  # passing walls 100, 200, 300
        self.assertIn("passing", rule)

    def test_even_count_takes_the_lower_median(self):
        runs = [run(1, True, 400), run(2, True, 100), run(3, True, 300), run(4, True, 200)]
        self.assertEqual(pg.representative(runs)[0]["id"], "r4")  # 100 200 | 300 400 -> 200

    def test_no_passing_run_falls_back_to_all_runs(self):
        runs = [run(1, False, 50), run(2, None, 70), run(3, False, 60)]
        chosen, rule = pg.representative(runs)
        self.assertEqual(chosen["id"], "r3")
        self.assertIn("no passing run", rule)

    def test_single_passing_run_wins_over_faster_failures(self):
        runs = [run(1, False, 10), run(2, False, 20), run(3, True, 900)]
        self.assertEqual(pg.representative(runs)[0]["id"], "r3")

    def test_ties_go_to_the_smaller_id_whatever_the_input_order(self):
        a = [run(2, True, 100), run(1, True, 100)]
        self.assertEqual(pg.representative(a)[0]["id"], "r1")
        self.assertEqual(pg.representative(list(reversed(a)))[0]["id"], "r1")

    def test_missing_wall_time_sorts_last(self):
        runs = [run(1, True, None), run(2, True, 100), run(3, True, 200)]
        self.assertEqual(pg.representative(runs)[0]["id"], "r3")

    def test_empty(self):
        self.assertEqual(pg.representative([]), (None, None))


class TimeAxis(unittest.TestCase):
    VERSIONS = [{"n": 1, "t": 10.0}, {"n": 2, "t": 50.0}, {"n": 3, "t": 90.0}]

    def test_shared_axis_ends_at_the_slowest_run(self):
        self.assertEqual(pg.shared_t_max([{"wall_s": 30}, {"wall_s": 120}, {"wall_s": None}]), 120)
        self.assertEqual(pg.shared_t_max([]), 1.0)

    def test_sample_times_are_shared_and_end_at_t_max(self):
        self.assertEqual(pg.sample_times(120, 4), [30, 60, 90, 120])
        self.assertEqual(pg.sample_times(120, 0), [])

    def test_axis_x_is_linear_and_clamped(self):
        self.assertEqual(pg.axis_x(0, 100, 20, 300), 20)
        self.assertEqual(pg.axis_x(50, 100, 20, 300), 170)
        self.assertEqual(pg.axis_x(100, 100, 20, 300), 320)
        self.assertEqual(pg.axis_x(250, 100, 20, 300), 320)
        self.assertEqual(pg.axis_x(-5, 100, 20, 300), 20)

    def test_a_shorter_run_has_a_proportionally_shorter_bar(self):
        t_max = pg.shared_t_max([{"wall_s": 200}, {"wall_s": 50}])
        full = pg.axis_x(200, t_max, 0, 400)
        quarter = pg.axis_x(50, t_max, 0, 400)
        self.assertEqual((full, quarter), (400, 100))

    def test_state_at(self):
        self.assertIsNone(pg.state_at(self.VERSIONS, 9.9))
        self.assertEqual(pg.state_at(self.VERSIONS, 10.0)["n"], 1)
        self.assertEqual(pg.state_at(self.VERSIONS, 89.0)["n"], 2)
        self.assertEqual(pg.state_at(self.VERSIONS, 1e9)["n"], 3)

    def test_animation_clock_sweeps_then_holds(self):
        c = pg.animation_clock(600, duration_s=10, fps=10, hold_s=2)
        self.assertEqual(len(c), 100 + 20)
        self.assertEqual(c[0], 0)
        self.assertEqual(c[99], 600)
        self.assertTrue(all(a <= b for a, b in zip(c, c[1:])))
        self.assertEqual(set(c[100:]), {600})
        self.assertAlmostEqual(pg.speedup(600, 10), 60)

    def test_columns_freeze_at_their_own_finish(self):
        # A run that finished at 40 s shows its last version at every later
        # clock time; one still running shows what it had.
        runs = {"fast": {"wall_s": 40, "versions": [{"n": 1, "t": 30}]},
                "slow": {"wall_s": 100, "versions": [{"n": 1, "t": 20}, {"n": 2, "t": 95}]}}
        for t in pg.animation_clock(100, 2, 10, 1):
            fast = pg.state_at(runs["fast"]["versions"], min(t, runs["fast"]["wall_s"]))
            slow = pg.state_at(runs["slow"]["versions"], min(t, runs["slow"]["wall_s"]))
            if t >= 40:
                self.assertEqual(fast["n"], 1)
            if 20 <= t < 95:
                self.assertEqual(slow["n"], 1)
            if t >= 95:
                self.assertEqual(slow["n"], 2)

    def test_fmt_clock(self):
        self.assertEqual(pg.fmt_clock(0), "0:00")
        self.assertEqual(pg.fmt_clock(59.6), "1:00")
        self.assertEqual(pg.fmt_clock(754), "12:34")
        self.assertEqual(pg.fmt_clock(3725), "1:02:05")
        self.assertEqual(pg.fmt_clock(None), "–")

    def test_hero_columns_fit_the_card(self):
        import hero
        for n in (1, 2, 3):
            boxes = hero.column_boxes(n)
            self.assertEqual(len(boxes), n)
            self.assertGreaterEqual(boxes[0][0], 0)
            x, w = boxes[-1]
            self.assertLessEqual(x + w, hero.W)
            self.assertTrue(all(b[0] + b[1] < c[0] for b, c in zip(boxes, boxes[1:])))


WORK = "/real/tmp/cad-T1-openscad-abc"


def line(obj):
    return json.dumps(obj)


def assistant(mid, content, out_tokens):
    return line({"type": "assistant", "message": {"id": mid, "content": content,
                                                  "usage": {"output_tokens": out_tokens}}})


def tool_use(i, name, **inp):
    return {"type": "tool_use", "id": f"c{i}", "name": name, "input": inp}


def result(i, error=False):
    return line({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": f"c{i}", "is_error": error, "content": "ok"}]}})


def transcript():
    """A synthetic stream: init, a Write, an Edit, a failed Edit, a Write
    of a non-source file, a MultiEdit, an Edit to a file made elsewhere,
    and a result. Times are on a made-up harness clock."""
    return [
        (0.1, line({"type": "system", "subtype": "init", "cwd": WORK})),
        (2.0, assistant("m1", [{"type": "text", "text": "plan"}], 5)),
        (3.0, assistant("m1", [tool_use(1, "Write", file_path=f"{WORK}/bracket.scad", content="a = 1;\ncube(a);\n")],
                        40)),
        (3.5, result(1)),
        (5.0, assistant("m2", [tool_use(2, "Edit", file_path=f"{WORK}/bracket.scad", old_string="a = 1;",
                                        new_string="a = 2;")], 30)),
        (5.4, result(2)),
        (6.0, assistant("m3", [tool_use(3, "Edit", file_path=f"{WORK}/bracket.scad", old_string="nope",
                                        new_string="x")], 10)),
        (6.1, result(3, error=True)),
        (7.0, assistant("m4", [tool_use(4, "Write", file_path=f"{WORK}/notes.txt", content="hi")], 10)),
        (7.1, result(4)),
        # The init cwd can name the real temp path and a tool input a symlink to it (macOS: /var vs its target).
        (8.0, assistant("m5", [tool_use(5, "MultiEdit", file_path=WORK.replace("/real", "/link") + "/bracket.scad",
                                        edits=[{"old_string": "cube(a)", "new_string": "sphere(a)"},
                                               {"old_string": "2", "new_string": "3", "replace_all": True}])],
                        20)),
        (8.2, result(5)),
        (9.0, assistant("m6", [tool_use(6, "Edit", file_path=f"{WORK}/made_by_bash.py", old_string="x",
                                        new_string="y")], 10)),
        (9.1, result(6)),
        (10.0, line({"type": "result", "subtype": "success", "num_turns": 6, "total_cost_usd": 1.5})),
    ]


class Stream(unittest.TestCase):
    def test_stream_points(self):
        pts = pg.stream_points(transcript())
        asst = [p for p in pts if not p.get("result")]
        self.assertEqual([p["turn"] for p in asst], [1, 2, 3, 4, 5, 6])
        self.assertEqual(asst[0]["t"], 2.0)
        # m1's usage is superseded by its later block (40), not summed (45).
        self.assertEqual(asst[0]["output_tokens"], 40)
        self.assertEqual(asst[-1]["output_tokens"], 40 + 30 + 10 + 10 + 20 + 10)
        self.assertEqual(pts[-1], {"t": 10.0, "result": True, "turns": 6, "cost_usd": 1.5})
        self.assertEqual(pg.point_at(pts, 5.2)["turn"], 2)
        self.assertIsNone(pg.point_at(pts, 1.0))

    def test_replay_source_writes(self):
        w = pg.replay_source_writes(transcript())
        self.assertEqual([(x["t"], x["path"], x["via"]) for x in w],
                         [(3.5, "bracket.scad", "Write"), (5.4, "bracket.scad", "Edit"),
                          (8.2, "bracket.scad", "MultiEdit"), (9.1, "made_by_bash.py", "Edit")])
        self.assertEqual(w[0]["text"], "a = 1;\ncube(a);\n")
        self.assertEqual(w[1]["text"], "a = 2;\ncube(a);\n")
        self.assertEqual(w[2]["text"], "a = 3;\nsphere(a);\n")
        self.assertIsNone(w[3]["text"])  # the file's earlier text is unknown

    def test_build_progress(self):
        lines = transcript()
        versions = [{"n": 1, "t": 5.5, "state": {"bracket": "000-out__bracket.stl"}, "changed": ["bracket"]}]
        rec = pg.build_progress(task="T1", condition="openscad", rep=1, parts=["bracket"], wall_s=10.0,
                                timed_out=False, passed=True, versions=versions, sources=[],
                                points=pg.stream_points(lines), timing={"source": "test"}, stl_root="history")
        self.assertEqual(rec["versions"][0]["turn"], 2)
        self.assertTrue(rec["versions"][0]["complete"])
        self.assertEqual(rec["final"], {"t": 10.0, "turns": 6, "cost_usd": 1.5, "assistant_messages": 6})
        json.dumps(rec)


def src(t, path, text, copy=True):
    return {"t": t, "path": path, "text": text, "copy": f"sources/{t}-{path}" if copy else None}


def read_text(s):
    return s["text"] if s.get("copy") else None


class SourceFrames(unittest.TestCase):
    def test_scad_refs_ignore_comments_and_strings(self):
        text = ('include <a.scad>\n// use <gone.scad>\n/* include <also_gone.scad> */\n'
                'echo("use <not_a_ref.scad>");\nuse <lib/b.scad>;\n')
        self.assertEqual(pg.scad_refs(text), ["a.scad", "lib/b.scad"])

    def test_resolve_ref_is_relative_to_the_including_file(self):
        self.assertEqual(pg.resolve_ref("bracket.scad", "lib/x.scad"), "lib/x.scad")
        self.assertEqual(pg.resolve_ref("lib/x.scad", "y.scad"), "lib/y.scad")
        self.assertEqual(pg.resolve_ref("lib/x.scad", "../z.scad"), "z.scad")
        self.assertIsNone(pg.resolve_ref("bracket.scad", "../outside.scad"))
        self.assertIsNone(pg.resolve_ref("bracket.scad", "/abs/lib.scad"))

    def test_versions_follow_part_saves_and_their_includes(self):
        sources = [
            src(1.0, "check.scad", "cube(1);"),             # not a part: no frame yet
            src(2.0, "base.scad", "include <dims.scad>\ncube(w);"),  # needs dims.scad: incomplete
            src(3.0, "dims.scad", "w = 2;"),                # now complete
            src(4.0, "check.scad", "cube(3);"),             # check edits change nothing shown
            src(5.0, "notes.py", "x = 1"),                  # not the condition's extension
            src(6.0, "lid.scad", "sphere(1);"),             # second part appears
            src(7.0, "dims.scad", "w = 2;"),                # same text again: no new frame
            src(8.0, "dims.scad", "w = 4;"),                # an include changed: a new frame
        ]
        vs = pg.source_versions(sources, ["base", "lid"], ".scad", read_text)
        self.assertEqual([(v["n"], v["t"]) for v in vs], [(1, 2.0), (2, 3.0), (3, 6.0), (4, 8.0)])
        self.assertEqual(vs[0]["parts"], ["base"])
        self.assertEqual(vs[0]["problems"], ["dims.scad: not in the run directory"])
        self.assertEqual(vs[1]["problems"], [])
        self.assertEqual(sorted(vs[1]["closure"]), ["base.scad", "dims.scad"])
        self.assertEqual(vs[2]["parts"], ["base", "lid"])
        self.assertEqual(vs[3]["closure"]["dims.scad"], "w = 4;")

    def test_unknown_text_or_outside_reference_is_incomplete(self):
        unknown = pg.source_versions([src(1.0, "a.scad", None, copy=False)], ["a"], ".scad", read_text)
        self.assertEqual(len(unknown), 1)
        self.assertIn("text unknown", unknown[0]["problems"][0])
        lib = pg.source_versions([src(1.0, "a.scad", "use <MCAD/gears.scad>\ncube(1);")], ["a"], ".scad",
                                 read_text)
        self.assertIn("MCAD/gears.scad: not in the run directory", lib[0]["problems"])
        out = pg.source_versions([src(1.0, "a.scad", "include <../x.scad>")], ["a"], ".scad", read_text)
        self.assertIn("outside the run directory", out[0]["problems"][0])

    def test_enabled_features(self):
        cfg = {"mcpServers": {"neoscad": {"command": "neoscad",
                                          "args": ["mcp", "--root", "/w", "--enable", "sketch", "--enable=fillet"]}}}
        plain = {"mcpServers": {"neoscad": {"command": "neoscad", "args": ["mcp", "--root", "/w"]}}}
        parts_call = [(1.0, assistant("m1", [tool_use(1, "mcp__neoscad__snapshot", file="a.scad", parts=True)], 1))]
        self.assertEqual(pg.enabled_features("neoscad", cfg, []), ["fillet", "sketch"])
        self.assertEqual(pg.enabled_features("neoscad", plain, []), [])
        self.assertEqual(pg.enabled_features("neoscad", plain, parts_call), ["part"])
        self.assertEqual(pg.enabled_features("neoscad", None, []), [])
        # The OpenSCAD condition is plain OpenSCAD whatever the transcript says.
        self.assertEqual(pg.enabled_features("openscad", cfg, parts_call), [])

    def test_timeline_shows_the_newest_and_an_export_wins_a_tie(self):
        line = pg.timeline([{"n": 1, "t": 50.0}, {"n": 2, "t": 90.0}],
                           [{"n": 1, "t": 10.0}, {"n": 2, "t": 50.0}, {"n": 3, "t": 70.0}])
        at = {t: (lambda x: x and (x["kind"], x["n"]))(pg.state_at(line, t)) for t in (5, 10, 50, 69, 70, 95)}
        self.assertEqual(at, {5: None, 10: ("src", 1), 50: ("stl", 1), 69: ("stl", 1), 70: ("src", 3),
                              95: ("stl", 2)})
        self.assertEqual(pg.timeline([], []), [])


FAKE_NEOSCAD = '''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ["FAKE_NEOSCAD_LOG"], "a") as f:
    f.write(json.dumps({"argv": a, "files": sorted(os.listdir(os.getcwd()))}) + "\\n")
out = a[a.index("-o") + 1]
src = a[-1]
if out.endswith(".stl"):
    if "BROKEN" in open(src).read():
        sys.exit("ERROR: Parser error")
    open(out, "w").write("solid x\\nfacet normal 0 0 1\\nouter loop\\nvertex 0 0 0\\nvertex 1 0 0\\n"
                         "vertex 0 1 1\\nendloop\\nendfacet\\nendsolid x\\n")
else:
    open(out, "wb").write(b"png")
'''


class SourceRender(unittest.TestCase):
    """hero.render_run's source frames, with a stand-in neoscad that
    records its arguments: what gets exported, with which flags and
    files, and what each frame's status becomes. No real rendering."""

    def setUp(self):
        import hero
        self.hero = hero
        self.tmp = tempfile.TemporaryDirectory()
        d = Path(self.tmp.name)
        self.log = d / "calls.jsonl"
        fake = d / "neoscad"
        fake.write_text(FAKE_NEOSCAD)
        fake.chmod(0o755)
        self.src_root, self.out = d / "src", d / "out"
        keep = self.src_root / "cad-x" / "T1-neoscad-1"
        keep.mkdir(parents=True)
        (keep / "mcp.json").write_text(json.dumps({"mcpServers": {"neoscad": {
            "command": "neoscad", "args": ["mcp", "--root", "/w", "--enable", "sketch"]}}}))
        (keep / "transcript.jsonl").write_text("")
        (keep / "sources").mkdir()
        texts = [("bracket.scad", "include <dims.scad>\ncube(w);"), ("dims.scad", "w = 2;"),
                 ("bracket.scad", "BROKEN")]
        sources = []
        for i, (p, text) in enumerate(texts):
            (keep / "sources" / f"{i:03d}-{p}").write_text(text)
            sources.append({"t": float(i + 1), "path": p, "copy": f"sources/{i:03d}-{p}"})
        self.run = {"id": "cad-x/T1-neoscad-1", "dir": str(keep), "condition": "neoscad", "parts": ["bracket"],
                    "versions": [], "sources": sources, "stl_root": "history"}

        class A:
            pass
        self.args = A()
        self.args.neoscad, self.args.src, self.args.out = str(fake), self.src_root, self.out
        os.environ["FAKE_NEOSCAD_LOG"] = str(self.log)

    def tearDown(self):
        os.environ.pop("FAKE_NEOSCAD_LOG", None)
        self.tmp.cleanup()

    def calls(self):
        return [json.loads(x) for x in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_statuses_flags_and_files(self):
        idx = self.hero.render_run(self.args, self.run, 10.0)
        status = {k: v["status"] for k, v in idx.items() if not k.startswith("_")}
        self.assertEqual(status, {"s01-still.png": "incomplete", "s01-anim.png": "incomplete",
                                  "s02-still.png": "ok", "s02-anim.png": "ok",
                                  "s03-still.png": "failed", "s03-anim.png": "failed"})
        self.assertEqual(idx["_source_render"], {"enable": ["sketch"], "versions": 3})
        exports = [c for c in self.calls() if c["argv"][c["argv"].index("-o") + 1].endswith(".stl")]
        # The incomplete version is never exported; the others are, with the
        # server's own --enable and the include beside the part.
        self.assertEqual(len(exports), 2)
        for c in exports:
            self.assertEqual(c["argv"][c["argv"].index("--enable") + 1], "sketch")
        self.assertIn("dims.scad", exports[0]["files"])
        n = len(self.calls())
        self.hero.render_run(self.args, self.run, 10.0)  # cached: nothing runs again
        self.assertEqual(len(self.calls()), n)

    def test_compose_side_badges_and_placeholders(self):
        self.hero.render_run(self.args, self.run, 10.0)
        line = self.hero.timeline(self.run)
        first, ok, broken = line
        self.assertEqual(self.hero.frame_for(self.args, self.run, first, "still"),
                         (None, "incomplete\n(a used file is missing)"))
        self.assertEqual(self.hero.frame_for(self.args, self.run, broken, "anim"), (None, "render error"))
        self.assertTrue(str(self.hero.frame_for(self.args, self.run, ok, "anim")[0]).endswith("s02-anim.png"))
        self.assertEqual(self.hero.badge_for(ok), "src")
        self.assertEqual(self.hero.badge_for(ok, final=True), "final src")
        self.assertEqual(self.hero.badge_for({"kind": "stl", "n": 3}, done=True), "v3 done")
        # With no STL ever exported, the large cell shows the newest source.
        self.assertEqual(self.hero.final_item(self.run)["kind"], "src")
        self.assertEqual(self.hero.waiting_text(self.run, 0.5), "no model yet")

    def test_cadquery_sources_are_never_run(self):
        self.run["condition"] = "cadquery"
        self.run["sources"] = [{"t": 1.0, "path": "bracket.py", "copy": "sources/000-bracket.scad"}]
        idx = self.hero.render_run(self.args, self.run, 10.0)
        self.assertEqual(idx, {})
        self.assertEqual(self.calls(), [])
        self.assertEqual(self.hero.waiting_text(self.run, 5.0), "no STL yet\n1 source edit")


class Capture(unittest.TestCase):
    def test_watcher_keeps_source_versions_and_write_progress(self):
        import run_cad
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            work, keep = d / "work", d / "rundir" / "T1-openscad-1"
            (work / "out").mkdir(parents=True)
            keep.mkdir(parents=True)
            w = run_cad.Watcher(work, keep / "sources", 0.0, globs=run_cad.SOURCE_GLOBS,
                                max_bytes=run_cad.MAX_SOURCE_BYTES)
            w._stop.set()  # drive the scans by hand, so the test does not depend on the poll's timing
            w.thread.join()
            (work / "bracket.scad").write_text("cube(1);\n")
            (work / "out" / "bracket.stl").write_text("solid x\nendsolid x\n")
            w._scan()
            w._scan()
            (work / "bracket.scad").write_text("cube(2);\n\nsphere(1);\n")
            w._scan()
            w._scan()
            self.assertEqual([e["path"] for e in w.events], ["bracket.scad", "bracket.scad"])
            run_cad.write_progress(keep, "T1", "openscad", 1, ["bracket"], transcript(), [], w.events, 12.0,
                                   False, None)
            rec = json.loads((keep / "progress.json").read_text())
            self.assertEqual(rec["timing"]["source"], "live")
            self.assertEqual([s["lines"] for s in rec["sources"]], [1, 2])
            self.assertEqual((keep / rec["sources"][1]["copy"]).read_text(), "cube(2);\n\nsphere(1);\n")
            self.assertEqual(rec["stl_root"], "history")
            self.assertEqual(rec["record"], "rundir")


if __name__ == "__main__":
    unittest.main()
