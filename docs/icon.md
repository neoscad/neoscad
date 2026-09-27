# App icon and hero image

NeoSCAD's icon and its hero image are NeoSCAD models, rendered by
NeoSCAD. The sources are in `apple/Icon/`. Everything built from them
goes to `apple/Icon/build/`, which is gitignored by `apple/.gitignore`'s
`build/` rule. None of it is wired into `apple/project.yml` yet. That
waits until a concept is picked (see [Adopting a concept](#adopting-a-concept)).

## Concepts

All three nod to OpenSCAD's idea, a solid made by boolean operations,
without borrowing its logo: no sphere pierced by three cylinders, and no
yellow. Each file has its camera and tile colours in header comments
(`// camera:`, `// tile:`), and `build-icon.sh` reads them from there.
The times are for a full render to STL, measured with `/usr/bin/time`
on an Apple M4 Pro.

| | Model | Idea | Silhouette at 16 px | Render |
|-|-|-|-|-|
| A | `concept-a.scad`, "Lattice cube" | A rounded cube (a hull of 8 spheres) minus one oversized sphere per octant. The spheres break through the faces and into each other, which leaves a Schwarz-P-like frame of saddle surfaces. A coral sphere floats in each cell and shows through the openings. Teal and coral. | A rounded block. The holes blur into texture. | 0.35 s |
| B | `concept-b.scad`, "Cutaway core" | A hollow rounded cube around a hollow sphere around a solid rounded cube, with the octant facing the viewer subtracted from all three. It reads like an engineering section. Indigo, cyan and coral. | A block with a bright notch. This one is the most legible. | 0.10 s |
| C | `concept-c.scad`, "Threaded ring" | A torus carved by one smooth helical channel (a chain of sphere hulls), then cut into 36 wedges, each in its own colour, which gives a cyan to violet to magenta sweep. | A colourful ring. It is the least like OpenSCAD's disc. | 1.35 s |

Concept C went through two versions that failed:

- **Two crossing helices.** A braid read as noise.
- **Carving each colour wedge with only the nearby part of the channel.**
  This left ragged, half-cut grooves, because the channel is wide enough
  on the inside of the ring to reach wedges well beyond its own angle.
  The nightly OpenSCAD rendered the same wrong result, so it was a bug
  in the model, not in NeoSCAD. The file now carves the whole ring once
  and intersects it with each wedge.

## How the images are made

**Transparency.** NeoSCAD's PNG export has no alpha channel
(`sips -g hasAlpha` reports `no` on a `-o x.png` render), so
transparency is recovered from two renders with different backgrounds:
Starnight (`#000000`) and Nature (`#fafafa`). A pixel with coverage `a`
shows `a*F + (1-a)*B`, so the difference between the two renders gives
`a`, and the black render is the premultiplied colour. Keying out a
single background colour would leave a fringe around every edge.

The matte step also counts partly covered pixels, to catch a scheme that
changes the object's colours as well as the background. It found 0%
because every concept colours its parts with `color()`. It also shows
that the renders have hard edges, which is the next point.

**Anti-aliasing.** The PNG path draws with `sample_count: 1` (no MSAA,
`crates/render/src/offscreen.rs:220`). So icons are rendered at
4096 px and downscaled by repeated halving to 1024, 512, 128, 32 and
16 (16x supersampling at 1024). The hero is rendered at 7200x4050 and
downscaled to 2400x1350.

**Tool.** The image steps are in `scripts/apple/icon-tool.swift`, which
uses only CoreGraphics, ImageIO and CoreText. The scripts compile it
with `swiftc` into `apple/Icon/build/icon-tool` on first use, so there
are no new dependencies.

**Contact sheets.** Each size is shown at 1:1 over a checkerboard (to
show transparency) and over a dark grey. The 32 and 16 px versions are
then shown again at 8x with nearest-neighbour sampling, so you can judge
their actual pixels.

## Icon formats (checked on this machine, Xcode 27.0, 27A266a)

**What Xcode 27 expects.** The Icon Composer `.icon` document is the
current format. The Xcode file template says: "Icon Composer icons are
the modern way to provide application icons for iOS, macOS, and watchOS
apps." (`Xcode_27.0.0.app/Contents/Developer/Library/Xcode/Templates/File Templates/MultiPlatform/Resource/Icon Composer Icon.xctemplate/TemplateInfo.plist`).

Apple's article, [Creating your app icon using Icon Composer](https://developer.apple.com/documentation/Xcode/creating-your-app-icon-using-icon-composer),
retrieved 2026-09-26, adds the following:

- **Where it goes:** the `.icon` file goes in the project, not in the
  asset catalog.
- **How it is selected:** the target's App Icon name, which is
  `ASSETCATALOG_COMPILER_APPICON_NAME`, must match the file name.
- **Older systems:** Xcode generates images for older deployment
  targets.
- **Precedence:** a `.icon` "replaces any existing icon asset catalog",
  and "The latest version of Xcode uses the Icon Composer file instead
  of an existing `AppIcon` asset catalog".

**What is installed.**

- **Icon Composer 27.0** is in the Xcode bundle
  (`Contents/Applications/Icon Composer.app`). It ships a command-line
  renderer, `Contents/Executables/ictool`, whose `--export-image` takes
  `--platform`, `--rendition`, `--tint-color`, `--tint-strength` and
  `--design-generation 26|27`.
- **`actool` 27.0 accepts a `.icon` directly.** Compiling
  `AppIcon.icon` with `--app-icon AppIcon` produces `Assets.car`, a
  fallback `AppIcon.icns`, and a partial Info.plist that sets both
  `CFBundleIconFile` and `CFBundleIconName` to `AppIcon`.
  `assetutil --info` shows the icon stored as an `IconGroup` in the
  `NSAppearanceNameAqua`, `NSAppearanceNameDarkAqua` and
  `ISAppearanceTintable` appearances.
- **A classic `.appiconset` still compiles.** It produces flat
  `Icon Image` renditions.

**The `.icon` document.** It is a directory holding `icon.json` and an
`Assets/` folder of PNG or SVG layers. The generated document uses these
`icon.json` keys:

- `fill-specializations`: a light gradient, plus a dark gradient keyed
  by `"appearance": "dark"`.
- `groups[].layers[]`, with `image-name`, `name` and `glass`.
- On the group: `shadow`, `translucency` and `specular`.
- `supported-platforms.squares: ["macOS"]`.

These keys were confirmed in three ways: they appear as strings in the
Icon Composer frameworks, `ictool` renders the document, and actool
compiles it. The Default and Dark previews show the two fills, and the
tinted and clear previews come out correctly. The mask, Liquid Glass,
and the tinted and clear variants are all applied by the system. The
document carries only the fill and an untiled, transparent layer. The
layer is framed at 72% of the canvas so that the rounded-square mask
does not clip it.

**What the pipeline produces**, per concept, in `apple/Icon/build/concept-X/`:

- `art-{4096,1024,512,128,32,16}.png`: the render on transparent.
- `sheet.png`: the contact sheet.
- `AppIcon.icon/`: the recommended asset.
- `preview-{Default,Dark,TintedDark,ClearLight}.png`: `ictool` renders
  of the `.icon`.
- `AppIcon.appiconset/`: the classic asset, all ten mac sizes. The
  rounded tile is baked in, because macOS does not mask
  `.appiconset` images.
- `actool/`: both assets compiled, as a check that they build.

## Hero

`apple/Icon/hero.scad` is a BOSL2 herringbone planetary gearbox:

- The gears are laid out by `planetary_gears()`: a sun, four planets
  and a ring with a quarter cut away.
- It sits on a plinth of gyroid lattice made by BOSL2's `isosurface()`,
  which is marching cubes written in the OpenSCAD language.
- The model has 134,537 vertices and 269,894 facets.

Measured by `scripts/apple/build-hero.sh`, best of 3 wall-clock runs of
a full render to STL, on an Apple M4 Pro (14 cores), 2026-09-26:

| | Best of 3 |
|-|-|
| neoscad 0.1.0 (`target/release`) | 3.66 s |
| OpenSCAD nightly 2026.09.23, `--backend=manifold` | 6.05 s |

The caption on `apple/Icon/build/hero/hero.png` is written from these
measurements every time the script runs, and they are also saved to
`times.txt`. They are never typed in by hand.

## Regenerating

```sh
scripts/apple/build-icon.sh a        # or b, c, or any .scad with camera/tile headers
scripts/apple/build-hero.sh [RUNS]   # default 3; needs .reference/BOSL2 and the nightly
```

Both scripts use `target/release/neoscad`. From a worktree, they use the
main checkout's copy, or whatever `NEOSCAD` is set to. `build-hero.sh`
sets `OPENSCADPATH` to `.reference`.

## Adopting a concept

This is not done yet, and it touches `apple/project.yml`:

1. Copy `apple/Icon/build/concept-X/AppIcon.icon` to `apple/App/AppIcon.icon`.
   It is a small, reviewable directory: one JSON file and one PNG.
2. Add the file to the app target's sources.
3. Set `ASSETCATALOG_COMPILER_APPICON_NAME: AppIcon`.
4. The existing `apple/App/Assets.xcassets/AppIcon.appiconset` is
   superseded by the `.icon`, so remove it to avoid two sources of
   truth.
