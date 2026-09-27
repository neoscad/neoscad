// icon-tool: the image steps of the icon and hero pipeline
// (scripts/apple/build-icon.sh, scripts/apple/build-hero.sh), in plain
// CoreGraphics so the pipeline needs nothing beyond Xcode.
//
//   icon-tool matte DARK.png LIGHT.png LIGHT_GREY OUT.png
//   icon-tool resize IN.png SIZE OUT.png            (square; SIZE px)
//   icon-tool fit IN.png FRACTION OUT.png           (crop to content, centre, pad)
//   icon-tool tile IN.png TOP_HEX BOTTOM_HEX OUT.png   (macOS 1024 tile)
//   icon-tool sheet OUT.png TITLE IMG1024 IMG512 IMG128 IMG32 IMG16
//   icon-tool backdrop IN.png TOP_HEX BOTTOM_HEX OUT.png
//   icon-tool resize-to IN.png W H OUT.png
//   icon-tool caption IN.png OUT.png TITLE SUBTITLE
//
// Why a matte: neoscad's PNG export has no alpha channel, so a transparent
// render is recovered from two renders that differ only in background
// colour (the Starnight scheme's black and the Nature scheme's #fafafa).
// A pixel of coverage a over background B shows a*F + (1-a)*B, so the
// difference between the two renders is (1-a)*(B_light - B_dark), which
// gives a exactly, anti-aliased edges included; the dark render is then
// the premultiplied colour. Keying out one background colour instead
// leaves a fringe of that colour round every edge.

import AppKit
import CoreGraphics
import CoreText
import Foundation
import ImageIO
import UniformTypeIdentifiers

let space = CGColorSpace(name: CGColorSpace.sRGB)!

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(("icon-tool: " + msg + "\n").data(using: .utf8)!)
    exit(1)
}

func load(_ path: String) -> CGImage {
    guard let src = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
        let img = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else { fail("cannot read \(path)") }
    return img
}

func save(_ img: CGImage, _ path: String) {
    let url = URL(fileURLWithPath: path)
    guard let dst = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil)
    else { fail("cannot write \(path)") }
    CGImageDestinationAddImage(dst, img, nil)
    if !CGImageDestinationFinalize(dst) { fail("cannot write \(path)") }
}

/// An RGBA8 premultiplied context, the one pixel format every step uses.
func context(_ w: Int, _ h: Int) -> CGContext {
    guard
        let ctx = CGContext(
            data: nil, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4,
            space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
    else { fail("cannot make a \(w)x\(h) context") }
    ctx.interpolationQuality = .high
    return ctx
}

func pixels(_ img: CGImage) -> (CGContext, UnsafeMutablePointer<UInt8>) {
    let ctx = context(img.width, img.height)
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: img.width, height: img.height))
    return (ctx, ctx.data!.assumingMemoryBound(to: UInt8.self))
}

func color(_ hex: String) -> CGColor {
    let h = hex.hasPrefix("#") ? String(hex.dropFirst()) : hex
    guard h.count == 6, let v = UInt32(h, radix: 16) else { fail("bad colour \(hex)") }
    return CGColor(
        colorSpace: space,
        components: [
            CGFloat((v >> 16) & 0xff) / 255, CGFloat((v >> 8) & 0xff) / 255, CGFloat(v & 0xff) / 255, 1,
        ])!
}

/// Downscale by repeated halving and one final high-quality draw. A single
/// 4096 -> 16 draw samples only a few source pixels per output pixel and
/// aliases badly; halving averages every pixel on the way down.
func scaled(_ img: CGImage, _ w: Int, _ h: Int) -> CGImage {
    var cur = img
    while cur.width >= 2 * w && cur.height >= 2 * h {
        let ctx = context(cur.width / 2, cur.height / 2)
        ctx.draw(cur, in: CGRect(x: 0, y: 0, width: cur.width / 2, height: cur.height / 2))
        cur = ctx.makeImage()!
    }
    let ctx = context(w, h)
    ctx.draw(cur, in: CGRect(x: 0, y: 0, width: w, height: h))
    return ctx.makeImage()!
}

func matte(_ darkPath: String, _ lightPath: String, _ grey: Int, _ out: String) {
    let dark = load(darkPath)
    let light = load(lightPath)
    guard dark.width == light.width, dark.height == light.height else { fail("sizes differ") }
    let (dctx, d) = pixels(dark)
    // Bind the context, not `_`: the pointer is into its buffer, which is
    // freed with it.
    let (lctx, l) = pixels(light)
    defer { withExtendedLifetime(lctx) {} }
    var partial = 0
    for i in 0..<(dark.width * dark.height) {
        let p = i * 4
        var sum = 0.0
        for c in 0..<3 { sum += Double(Int(l[p + c]) - Int(d[p + c])) }
        let a = max(0, min(1, 1 - sum / 3 / Double(grey)))
        let a8 = UInt8((a * 255).rounded())
        for c in 0..<3 { d[p + c] = min(d[p + c], a8) }
        d[p + 3] = a8
        if a8 > 5 && a8 < 250 { partial += 1 }
    }
    // Only edge pixels should be partly covered. A large share means the
    // two renders differ in more than background (a scheme that also
    // changes the object's colours), and the matte is wrong.
    let share = Double(partial) / Double(dark.width * dark.height)
    print(String(format: "matte: %.2f%% partly covered pixels", share * 100))
    if share > 0.05 { fail("renders differ beyond the background; is the model coloured with color()?") }
    save(dctx.makeImage()!, out)
}

/// The bounding box of the pixels with any coverage, in CG coordinates.
func contentBox(_ img: CGImage) -> CGRect {
    let (ctx, p) = pixels(img)
    defer { withExtendedLifetime(ctx) {} }
    var x0 = img.width, y0 = img.height, x1 = -1, y1 = -1
    for y in 0..<img.height {
        for x in 0..<img.width where p[(y * img.width + x) * 4 + 3] > 2 {
            x0 = min(x0, x); x1 = max(x1, x); y0 = min(y0, y); y1 = max(y1, y)
        }
    }
    if x1 < 0 { fail("image is empty") }
    // Rows in the buffer run top to bottom; CG's y axis runs bottom to top.
    return CGRect(x: x0, y: img.height - 1 - y1, width: x1 - x0 + 1, height: y1 - y0 + 1)
}

/// Crop to the content and centre it in a square canvas, the content's
/// longer side filling FRACTION of it, so every concept is framed the
/// same whatever the camera distance was.
func fit(_ inPath: String, _ fraction: Double, _ out: String) {
    let img = load(inPath)
    let box = contentBox(img)
    let side = img.width
    let s = Double(side) * fraction / Double(max(box.width, box.height))
    let w = Double(box.width) * s, h = Double(box.height) * s
    let ctx = context(side, side)
    let crop = img.cropping(to: CGRect(x: box.minX, y: CGFloat(img.height) - box.maxY, width: box.width, height: box.height))!
    ctx.draw(crop, in: CGRect(x: (Double(side) - w) / 2, y: (Double(side) - h) / 2, width: w, height: h))
    save(ctx.makeImage()!, out)
}

/// A vertical gradient filling `rect`, top colour first.
func gradient(_ ctx: CGContext, _ rect: CGRect, _ top: String, _ bottom: String) {
    let g = CGGradient(colorsSpace: space, colors: [color(top), color(bottom)] as CFArray, locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: rect.midX, y: rect.maxY), end: CGPoint(x: rect.midX, y: rect.minY), options: [])
}

/// The classic (pre-Icon Composer) macOS icon: on a 1024 canvas, an
/// 824 pt rounded square at (100, 100) with a soft drop shadow, the
/// artwork inside it. macOS since Big Sur does not mask app icons, so an
/// .appiconset has to carry its own tile or it shows as a bare cut-out.
func tile(_ inPath: String, _ top: String, _ bottom: String, _ out: String) {
    let art = load(inPath)
    let ctx = context(1024, 1024)
    let rect = CGRect(x: 100, y: 100, width: 824, height: 824)
    let path = CGPath(roundedRect: rect, cornerWidth: 185, cornerHeight: 185, transform: nil)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 28, color: CGColor(gray: 0, alpha: 0.35))
    ctx.addPath(path)
    ctx.setFillColor(color(bottom))
    ctx.fillPath()
    ctx.restoreGState()
    ctx.saveGState()
    ctx.addPath(path)
    ctx.clip()
    gradient(ctx, rect, top, bottom)
    // The artwork arrives already framed by `fit` on a square canvas;
    // drawn over the tile at 90% it keeps a margin inside the corners.
    let inset = rect.insetBy(dx: rect.width * 0.05, dy: rect.height * 0.05)
    ctx.draw(art, in: inset)
    ctx.restoreGState()
    save(ctx.makeImage()!, out)
}

func text(_ ctx: CGContext, _ s: String, _ size: CGFloat, _ weight: NSFont.Weight, _ col: CGColor, _ at: CGPoint) {
    let font = NSFont.systemFont(ofSize: size, weight: weight)
    let attr = NSAttributedString(string: s, attributes: [.font: font, .foregroundColor: NSColor(cgColor: col)!])
    let line = CTLineCreateWithAttributedString(attr)
    ctx.textPosition = at
    CTLineDraw(line, ctx)
}

/// A checkerboard, the usual way to show that a PNG is transparent.
func checker(_ ctx: CGContext, _ rect: CGRect, _ cell: CGFloat) {
    ctx.saveGState()
    ctx.clip(to: rect)
    ctx.setFillColor(CGColor(gray: 0.86, alpha: 1))
    ctx.fill(rect)
    ctx.setFillColor(CGColor(gray: 0.74, alpha: 1))
    var y = rect.minY
    var row = 0
    while y < rect.maxY {
        var x = rect.minX + (row % 2 == 0 ? 0 : cell)
        while x < rect.maxX {
            ctx.fill(CGRect(x: x, y: y, width: cell, height: cell))
            x += 2 * cell
        }
        y += cell
        row += 1
    }
    ctx.restoreGState()
}

/// Each size at 1:1 in two rows, over a checkerboard (it is transparent)
/// and over a dark desktop-like grey, then the 32 and 16 px versions
/// magnified 8x with nearest-neighbour sampling so their actual pixels
/// can be judged, which is the test a small-size silhouette has to pass.
func sheet(_ out: String, _ title: String, _ paths: [String]) {
    let imgs = paths.map(load)
    let gap: CGFloat = 32
    let widths = imgs.map { CGFloat($0.width) }
    let zoom: CGFloat = 8
    let zoomed = [imgs[3], imgs[4]]
    let zoomW = zoomed.reduce(0) { $0 + CGFloat($1.width) * zoom } + gap
    let rowW = widths.reduce(0, +) + gap * CGFloat(imgs.count + 1)
    let W = Int(max(rowW, zoomW + 2 * gap))
    let rowH: CGFloat = 1024 + 2 * gap
    let header: CGFloat = 96
    let zoomH = 256 + 2 * gap + 40
    let H = Int(header + 2 * rowH + zoomH)
    let ctx = context(W, H)
    ctx.setFillColor(CGColor(gray: 0.97, alpha: 1))
    ctx.fill(CGRect(x: 0, y: 0, width: W, height: H))
    text(ctx, title, 40, .semibold, CGColor(gray: 0.1, alpha: 1), CGPoint(x: gap, y: CGFloat(H) - 64))
    for (row, dark) in [false, true].enumerated() {
        let y0 = CGFloat(H) - header - CGFloat(row + 1) * rowH
        let band = CGRect(x: 0, y: y0, width: CGFloat(W), height: rowH)
        if dark {
            ctx.setFillColor(color("#2b2b30"))
            ctx.fill(band)
        } else {
            checker(ctx, band, 16)
        }
        var x = gap
        for (i, img) in imgs.enumerated() {
            let r = CGRect(x: x, y: y0 + gap, width: widths[i], height: CGFloat(img.height))
            ctx.draw(img, in: r)
            let label = "\(img.width)"
            let lc = dark ? CGColor(gray: 0.85, alpha: 1) : CGColor(gray: 0.15, alpha: 1)
            text(ctx, label, 18, .regular, lc, CGPoint(x: x, y: y0 + 8))
            x += widths[i] + gap
        }
    }
    var x = gap
    ctx.interpolationQuality = .none
    for img in zoomed {
        let w = CGFloat(img.width) * zoom, h = CGFloat(img.height) * zoom
        let r = CGRect(x: x, y: 40 + gap, width: w, height: h)
        checker(ctx, r, 8)
        ctx.draw(img, in: r)
        text(ctx, "\(img.width) px, 8x nearest", 18, .regular, CGColor(gray: 0.15, alpha: 1), CGPoint(x: x, y: 16))
        x += w + gap
    }
    save(ctx.makeImage()!, out)
}

/// The model (transparent) over a vertical gradient, same size.
func backdrop(_ inPath: String, _ top: String, _ bottom: String, _ out: String) {
    let img = load(inPath)
    let ctx = context(img.width, img.height)
    let rect = CGRect(x: 0, y: 0, width: img.width, height: img.height)
    gradient(ctx, rect, top, bottom)
    ctx.draw(img, in: rect)
    save(ctx.makeImage()!, out)
}

/// A caption band along the bottom: a title and a smaller subtitle.
func caption(_ inPath: String, _ out: String, _ title: String, _ subtitle: String) {
    let img = load(inPath)
    let ctx = context(img.width, img.height)
    let W = CGFloat(img.width)
    ctx.draw(img, in: CGRect(x: 0, y: 0, width: img.width, height: img.height))
    let bandH = W * 0.085
    let g = CGGradient(
        colorsSpace: space, colors: [CGColor(gray: 0, alpha: 0.55), CGColor(gray: 0, alpha: 0)] as CFArray,
        locations: [0, 1])!
    ctx.drawLinearGradient(g, start: CGPoint(x: 0, y: 0), end: CGPoint(x: 0, y: bandH * 1.6), options: [])
    let m = W * 0.03
    text(ctx, title, W * 0.026, .semibold, CGColor(gray: 1, alpha: 1), CGPoint(x: m, y: m + W * 0.02))
    text(ctx, subtitle, W * 0.0145, .regular, CGColor(gray: 1, alpha: 0.8), CGPoint(x: m, y: m * 0.9))
    save(ctx.makeImage()!, out)
}

let a = CommandLine.arguments
guard a.count > 1 else { fail("usage: see the header of scripts/apple/icon-tool.swift") }
switch (a[1], a.count) {
case ("matte", 6): matte(a[2], a[3], Int(a[4]) ?? 250, a[5])
case ("resize", 5): let n = Int(a[3])!; save(scaled(load(a[2]), n, n), a[4])
case ("resize-to", 6): save(scaled(load(a[2]), Int(a[3])!, Int(a[4])!), a[5])
case ("fit", 5): fit(a[2], Double(a[3])!, a[4])
case ("tile", 6): tile(a[2], a[3], a[4], a[5])
case ("sheet", 9): sheet(a[2], a[3], Array(a[4...8]))
case ("backdrop", 6): backdrop(a[2], a[3], a[4], a[5])
case ("caption", 6): caption(a[2], a[3], a[4], a[5])
default: fail("bad arguments: \(a.dropFirst().joined(separator: " "))")
}
