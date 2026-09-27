# Progress video

`conformance video` turns what `progress/` records into a 1920x1080 H.264
video of the project's progress. It reads only recorded data and git, so
the same `progress/` gives the same video byte for byte (checked with
ffmpeg 8 on macOS by rendering twice, once with `RAYON_NUM_THREADS=1`, and
comparing SHA-256s).

    cargo build --release -p neoscad-conformance
    ./target/release/conformance video                       # progress/video/progress.mp4
    ./target/release/conformance video --fps 30 --hold 2 --frames-dir /tmp/frames --out talk.mp4
    ./target/release/conformance video --progress /path/to/other/progress --ffmpeg /opt/homebrew/bin/ffmpeg

It needs no reference checkout, so it also runs from a worktree; there,
`--progress` points at the main tree's `progress/`, because `progress/` is
gitignored and only exists where the runs were recorded.

## What it shows

1. **Title card:** "NeoSCAD — rebuilding OpenSCAD", the span from the first
   snapshot to the last, and the suite's size.
2. **One scene per snapshot** in `progress/index.jsonl`, in order:
   - the test grid, regenerated from the snapshot's `scoreboard.json` and
     the manifest at its commit (`grid::cells_for`), laid out by the same
     code as `conformance grid` in a smaller area;
   - a caption with the UTC date and time, the short sha (`-DIRTY` for a
     run on uncommitted changes) and the commit subject;
   - a panel with total passes, per-tier passes out of the tier's tests that
     are not skipped, and a line chart of total passes against time.

   Between snapshots (0.75 s) cell colours blend, the counters count and
   the chart's newest point slides into place. When the manifest changed
   between two snapshots the cells no longer correspond, so the grid
   switches at the midpoint instead of blending unrelated tests.
3. **Benchmark interludes:** a run in `progress/bench` is shown after the
   snapshot at its commit, else the snapshot nearest in time, as its
   `bench-chart` (whose footer carries the geometric means). Runs with no
   geometric mean (an edit-loop-only run) are left out, and a quick run is
   dropped when a full run lands on the same snapshot.
4. **Agent-eval interlude:** the latest `progress/agent-eval/*.json` that
   ran both conditions, as a table of MCP (A) against Bash (B) per task,
   labelled "pilot, n=1".
5. **End card:** final passes and per-tier passes, the newest full
   benchmark's geometric-mean speedups, the commit count up to the last
   snapshot (`git rev-list --count`) and the snapshot count.

Scenes are joined by 0.5 s cross-fades. `--hold` (default 2 s) sets how long
a snapshot stays after its transition; interludes hold twice that, the
title 1.5 times and the end card 2.5 times.

## Encoding

Frames are PNGs written in parallel to `--frames-dir` (kept) or a temporary
directory (deleted). A held frame is drawn once and hard-linked for its
repeats. ffmpeg then encodes `libx264`, `yuv420p`, CRF 20, preset medium,
with a fixed thread count (x264's output depends on it), `bitexact` flags
and no metadata. The run of 2026-09-26 (19 snapshots, one benchmark and one
agent-eval interlude) gave 2,154 frames, 71.8 s and 4.2 MB, in about 8 s.

## Not done: `--showcase`

The showcase scenes would build historical milestone commits in temporary
worktrees and render the 26 models of `conformance/showcase.json` with each
binary, beside OpenSCAD's expected PNGs. Each milestone is a full release
build of the workspace (minutes, and gigabytes of target directory unless
one directory is shared and rebuilt in turn), and the early milestones have
no renderer at all (`neoscad` gained PNG export in the render crate, commit
9336b1c), so most of the grid would be empty. It was left as a followup
rather than built into the default path. A cheaper variant: record the
showcase renders at `run --record` time from now on (as the architecture's
"Progress recording" plans), and have the video read them from the
snapshot directories.
