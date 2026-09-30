# Viewport stripes on lavapipe (Linux app, Vulkan)

Audit of the striped black "staircase hatching" seen near the origin after
orbiting `web/examples/CSG.scad` in the Linux app (crates/linux-app) under
Xvfb in Docker, rendering with wgpu on Mesa lavapipe.

## Verdict

- **It is a lavapipe rasterization defect, triggered by our input.** The
  marks are fragments of a single axis line that lavapipe fills as an area.
  Metal draws the same frame clean, and so does Mesa's llvmpipe through
  wgpu's GL backend. Our input is valid: a line whose far end is a point at
  infinity (`w = 0`). OpenSCAD draws exactly the same line (below). Every
  conformant Vulkan driver has to clip it.
- **Real GPUs should not show it.** Metal was tested and is clean. DX12 and
  hardware Vulkan drivers were not tested in the failing orbit (see
  "Not verified").
- **Still worth a cheap workaround in `crates/render`.** lavapipe is the
  renderer for the Linux app in CI, in Docker and on GPU-less machines. The
  CLI's PNG export shows the same defect there (reproduced below). A CPU clip
  of model-space overlay lines to the view frustum removes it in every view
  tested and leaves the image unchanged elsewhere.

## What the marks are

- **The filled area is one axis line.** It is whichever of the six axis
  lines (`overlay.rs:173-187`, `pen.line4(origin, d)` with `d = [±1,0,0,0]`
  etc.) points towards the camera steeply enough that its projected end runs
  far off screen. In the reported orbit that is −X. lavapipe rasterizes it as
  a solid region between the line and a staircase boundary.
- **The staircase steps are 64 px.** In the repro images the steps fall on
  x = 64, 128, 192, 320 and 384. This is consistent with llvmpipe's 64×64
  binning tiles. The tile size is from memory of Mesa's `lp_limits.h`, not
  checked against its source.
- **The hatching is our stipple pattern painted over that area.** The
  negative axes are stippled (`shader.wgsl:231-241`: counter =
  `max(|dx|,|dy|)` from the line's first point, the origin, three pixels on
  and three off). Over an area, a Chebyshev-distance counter draws nested
  square rings centred on the origin. Those rings are the L-shaped "staircase
  hatching". A positive axis in the same situation fills solid black: with
  `rz = 220` the +X axis points at the camera (`vp_lavapipe_rz220.png`).
- **It also shows at other angles, differently.** Near top-down
  (`object_rot = [80,0,30]`), lavapipe drops the visible parts of the −X and
  −Y dashed axes and draws a thin black wedge instead
  (`montage_lowelev_metal.png`, left lavapipe, right Metal).

## Cause, narrowed by experiment

All runs below are 710×520 with perspective projection, `viewer_distance`
140 and `object_rot = [60,0,rz]`, on lavapipe (Mesa 25.2.8, LLVM 20.1.2,
arm64 Ubuntu 24.04 container). Each experiment changed only the overlay
lines, in a throwaway copy of `crates/render` inside the container.

| Axis line from origin | Result |
|---|---|
| to the point at infinity (shipped) | bad |
| to a finite point, length 60, 100, 125, 200 or 300 | clean |
| to a finite point, length 380, 400 or 1000 | bad |
| to infinity, clipped on the CPU to the **near plane only** | bad |
| to infinity, clipped on the CPU to **near + four side planes** | clean |

- **The first `w <= 0` guess was wrong.** At this camera the near plane is
  crossed at a length of about 390. Length 380 is still in front of the near
  plane and already fails, and near-plane clipping alone does not help.
- **What trips lavapipe is an endpoint with a small positive `w`, which
  projects far outside the viewport.** A rough estimate from the camera
  gives about 12k px off-centre at length 300 (clean) and about 36k px at
  length 380 (bad). The exact limit and the Mesa code path were not
  identified.
- **Why the default view is clean:** no axis end there projects that far off
  screen.

The other candidates in the brief were looked at and ruled out:

- **Not MSAA or alpha-to-coverage.** The CLI draws with 1 sample
  (`offscreen.rs:99`) and reproduces it (`cli_lavapipe.png`). The app
  viewport draws with 4 samples (`viewport.rs:44`) and reproduces it too.
  Alpha-to-coverage is not enabled: `gpu.rs:408-411` leaves
  `MultisampleState` at its defaults apart from `count`.
- **Not depth bias or depth format.** The axis pipelines use no bias. The
  fill is drawn at full colour over the grid, not z-fighting.
- **Not an uninitialised readback region.** The fill follows the axis line
  and our stipple phase, and the CLI (no `attach_texture`) shows it too.
- **Not a degenerate camera.** It happens over a wide range of orbits
  whenever an axis points towards the eye.

## OpenSCAD comparison

Our axes match OpenSCAD's. `GLView::showAxes`
(`.reference/openscad/src/glview/GLView.cc:423-450`) draws
`glVertex4d(0,0,0,1)` to `glVertex4d(±1,0,0,0)` ("w = 0 goes to infinity"),
with `glLineStipple(3, 0xAAAA)` on the negative half. NeoSCAD's
`overlay.rs:171-188` is a faithful port. The workaround below changes only
what reaches the GPU, not what is drawn, so it is not a divergence from
OpenSCAD.

## Evidence (commands)

The images are saved in the session scratchpad, not the repo: `repro/*.png`
and a copy of the probe crate.

1. **Metal, CLI, clean.** CLI binary is `target/release/neoscad`, built at
   the audited commit:

       ./target/release/neoscad web/examples/CSG.scad -o metal_rz320.png \
         --camera=0,0,0,30,0,320,140 --view axes,scales --imgsize 710,520 --projection p

2. **Metal, app viewport path, clean** (`vp_metal.png`). The probe is a
   30-line scratch crate. It depends on `neoscad-render` by path, calls
   `Gpu::new_blocking`, `Viewport::new`, `attach_texture(710,520,1.0)` (as
   linux-app `view.rs:37` does), `set_settings(ViewSettings::default())`,
   `with_camera`, `draw` and `read_pixels_blocking`, and writes a PNG. Run as
   `vp-probe out.png 60 40 140`.
3. **lavapipe.** The container was `ubuntu:24.04` with `mesa-vulkan-drivers`
   and Rust 1.98.1, with the repo mounted read-only.
   - `vp_lavapipe.png`: the app viewport path. The bug reproduces, matching
     the reported screenshot.
   - `cli_lavapipe.png`: the CLI with the same command as step 1. The bug
     reproduces.
   - `vp_llvmpipe_gl.png`: the same probe with wgpu's GL backend on
     llvmpipe. Clean.
4. **Length, clipping and fix experiments.** The results are the table
   above; images are `montage_len.png`, `montage_clip.png`,
   `montage_frustum.png` and `montage_lowelev_fixed.png` (fixed lavapipe vs
   Metal, matching).

The Docker image and volumes were removed afterwards.

## Proposed fix

Clip model-space overlay segments to the frustum's near and four side
planes on the CPU, once per frame, where the line vertices are uploaded.
That is `Renderer::draw`, which builds `lines(&overlay.before)`,
`lines(&overlay.behind)` and `lines(&overlay.after)` (`gpu.rs:659-661`) and
already has `frame.clip_from_model` (`gpu.rs:238`).

- **Algorithm.** Liang–Barsky on each vertex pair with `space == Model`.
  Take the clip coordinates `c = clip_from_model · p` of both ends. The
  planes are `w+x`, `w−x`, `w+y`, `w−y` and `z` (depth 0..1). Interpolate the
  model-space homogeneous positions: they are linear in the same parameter,
  so a `w = 0` end becomes a finite point.
- **Keep `start` unchanged**, so the stipple phase does not move.
- **Drop segments that are wholly outside**, and draw with the clipped
  counts rather than `overlay.before.len()` (`gpu.rs:750`, `775`, `780`).
- **This covers every model-space line in one place:** axes, scale ticks,
  labels, the grid and annotation lines. The grid spans ±`viewer_distance`
  and can pass close to the eye at grazing angles.
- **Cost:** a few thousand segments per frame, which is trivial.
- **Output is unchanged on conformant drivers**, because the GPU would have
  clipped at the same planes. This should be confirmed with the existing
  CLI PNG conformance cases.
- **Test.** The probe version (`clip_frustum`, about 40 lines) made every
  failing view here match Metal. A unit test can assert that after clipping,
  every model-space vertex has `w > 0` and `|x|,|y| <= w` in clip space.

## Decisions for the owner

- **Workaround at all.** Carry the workaround for a driver bug, or treat
  lavapipe output as best-effort. It is a small, contained change that also
  fixes CLI PNGs on GPU-less Linux. I recommend doing it.
- **Report upstream.** Whether to file the defect with Mesa (lavapipe line
  rasterization with far off-screen endpoints). No existing report was
  searched for.
- **Backend on software adapters.** Whether the Linux app should prefer GL
  when the adapter is a software rasterizer. llvmpipe GL was clean here, but
  that is one view, not a general result.

## Not verified

- **DX12 and hardware Vulkan in the failing orbit.** Only default-view DX12
  screenshots were available, and no hardware Vulkan was run. The claim that
  real GPUs are clean rests on Metal and on the Vulkan clipping requirement,
  not on a run.
- **The Mesa-side cause.** Not identified: the exact coordinate limit,
  whether guard band or fixed-point overflow, or why the GL path differs.
  No Mesa source was read.
- **The 64 px tile size** is from memory, not source.
