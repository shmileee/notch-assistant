// Draws the app icon: the island expanded from a notch, on the demo's gradient.
//
//   swift scripts/icon.swift assets/AppIcon.icns
//
// Renders every size an .icns needs into an .iconset, then runs iconutil.

import AppKit

func draw(_ px: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let s = CGFloat(px) / 1024
    func r(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> NSRect {
        NSRect(x: x * s, y: (1024 - y - h) * s, width: w * s, height: h * s) // top-left coordinates
    }
    func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> NSColor {
        NSColor(srgbRed: CGFloat(hex >> 16 & 0xff) / 255, green: CGFloat(hex >> 8 & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: a)
    }
    // macOS icon grid: an 824 pt rounded square, 100 pt in.
    let tile = NSBezierPath(roundedRect: r(100, 100, 824, 824), xRadius: 185 * s, yRadius: 185 * s)
    NSGradient(colors: [rgb(0x1b234a), rgb(0x3a2a6b), rgb(0x8a4b7d)])!.draw(in: tile, angle: -60)
    // The island, hanging open from the top edge (clipped to the tile).
    NSGraphicsContext.saveGraphicsState()
    tile.addClip()
    let island = NSBezierPath(roundedRect: r(212, 0, 600, 450), xRadius: 96 * s, yRadius: 96 * s)
    rgb(0x000000).setFill(); island.fill()
    NSGraphicsContext.restoreGraphicsState()
    // A status dot and an avatar, then two lines of "text".
    rgb(0x34c773).setFill(); NSBezierPath(ovalIn: r(282, 236, 52, 52)).fill()
    rgb(0x5b8cff).setFill(); NSBezierPath(ovalIn: r(668, 224, 76, 76)).fill()
    rgb(0xffffff, 0.85).setFill(); NSBezierPath(roundedRect: r(282, 336, 340, 36), xRadius: 18 * s, yRadius: 18 * s).fill()
    rgb(0xffffff, 0.4).setFill(); NSBezierPath(roundedRect: r(282, 392, 220, 30), xRadius: 15 * s, yRadius: 15 * s).fill()
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let out = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "assets/AppIcon.icns")
let set = FileManager.default.temporaryDirectory.appendingPathComponent("AppIcon-\(getpid()).iconset")
try? FileManager.default.removeItem(at: set)
try! FileManager.default.createDirectory(at: set, withIntermediateDirectories: true)
for pt in [16, 32, 128, 256, 512] {
    try! draw(pt).write(to: set.appendingPathComponent("icon_\(pt)x\(pt).png"))
    try! draw(pt * 2).write(to: set.appendingPathComponent("icon_\(pt)x\(pt)@2x.png"))
}
let p = Process()
p.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
p.arguments = ["-c", "icns", set.path, "-o", out.path]
try! p.run(); p.waitUntilExit()
try? FileManager.default.removeItem(at: set)
exit(p.terminationStatus)
