"""Render an STL to a PNG for the CadQuery condition of the CAD comparison.

    <cadquery venv python> view_stl.py MESH.stl OUT.png [--azimuth 30]
        [--elevation 25] [--zoom 1] [--size 800x800] [--edges]

Why this exists: the OpenSCAD agent makes images with `openscad -o x.png`
and the NeoSCAD agent with `snapshot`, but CadQuery's own viewer
(cadquery.vis.show(..., screenshot=...)) cannot get an OpenGL context in
Claude Code's Bash sandbox ("No OpenGL context whatsoever could be
created!"). The harness therefore lets exactly this script run outside the
sandbox (run_cad.py, `excludedCommands`), and so it only reads and writes
files under the current directory. It uses VTK, which CadQuery installs.
"""

import argparse
import os
import sys
from pathlib import Path


def inside_cwd(p):
    cwd = Path.cwd().resolve()
    r = Path(p).resolve()
    if r != cwd and cwd not in r.parents:
        sys.exit(f"view_stl.py: {p} is outside the current directory")
    return r


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mesh")
    ap.add_argument("png")
    ap.add_argument("--azimuth", type=float, default=30)
    ap.add_argument("--elevation", type=float, default=25)
    ap.add_argument("--zoom", type=float, default=1.0)
    ap.add_argument("--size", default="800x800")
    ap.add_argument("--edges", action="store_true", help="draw triangle edges")
    a = ap.parse_args()
    mesh, png = inside_cwd(a.mesh), inside_cwd(a.png)
    if png.suffix.lower() != ".png":
        sys.exit("view_stl.py: the output must be a .png")
    if not mesh.exists():
        sys.exit(f"view_stl.py: no such file: {a.mesh}")
    w, h = (int(x) for x in a.size.lower().split("x"))

    from vtkmodules.vtkIOGeometry import vtkSTLReader
    from vtkmodules.vtkIOImage import vtkPNGWriter
    from vtkmodules.vtkRenderingCore import (vtkActor, vtkPolyDataMapper, vtkRenderer, vtkRenderWindow,
                                             vtkWindowToImageFilter)
    from vtkmodules.vtkRenderingAnnotation import vtkAxesActor
    import vtkmodules.vtkRenderingOpenGL2  # noqa: F401  (registers the OpenGL backend)

    reader = vtkSTLReader()
    reader.SetFileName(str(mesh))
    mapper = vtkPolyDataMapper()
    mapper.SetInputConnection(reader.GetOutputPort())
    actor = vtkActor()
    actor.SetMapper(mapper)
    actor.GetProperty().SetColor(0.85, 0.65, 0.2)
    if a.edges:
        actor.GetProperty().EdgeVisibilityOn()
    ren = vtkRenderer()
    ren.AddActor(actor)
    axes = vtkAxesActor()
    b = reader.GetOutput()
    reader.Update()
    bounds = b.GetBounds()
    size = max(bounds[1] - bounds[0], bounds[3] - bounds[2], bounds[5] - bounds[4], 1)
    axes.SetTotalLength(size / 4, size / 4, size / 4)
    ren.AddActor(axes)
    ren.SetBackground(1, 1, 1)
    win = vtkRenderWindow()
    win.SetOffScreenRendering(1)
    win.AddRenderer(ren)
    win.SetSize(w, h)
    cam = ren.GetActiveCamera()
    cam.SetViewUp(0, 0, 1)
    cam.SetPosition(1, 0, 0)
    cam.SetFocalPoint(0, 0, 0)
    ren.ResetCamera()
    cam.Azimuth(a.azimuth - 90)
    cam.Elevation(a.elevation)
    cam.OrthogonalizeViewUp()
    ren.ResetCamera()
    cam.Zoom(a.zoom)
    win.Render()
    grab = vtkWindowToImageFilter()
    grab.SetInput(win)
    grab.Update()
    out = vtkPNGWriter()
    out.SetFileName(str(png))
    out.SetInputConnection(grab.GetOutputPort())
    out.Write()
    print(f"wrote {os.path.relpath(png)} ({w}x{h}, bbox {[round(x, 3) for x in bounds]})")


if __name__ == "__main__":
    main()
