// Renders packaging/macos/dmg-background.svg into the 1x and 2x PNGs beside it, which
// scripts/bundle-macos.sh joins into the disk image's retina TIFF. Run it after editing the SVG:
//   swift packaging/macos/render-background.swift
import AppKit

let dir = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent()
guard let svg = NSImage(contentsOf: dir.appendingPathComponent("dmg-background.svg")) else {
    FileHandle.standardError.write("dmg-background.svg could not be read\n".data(using: .utf8)!)
    exit(1)
}
for (scale, name) in [(1, "dmg-background.png"), (2, "dmg-background@2x.png")] {
    let width = Int(svg.size.width) * scale
    let height = Int(svg.size.height) * scale
    let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
        samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
        bytesPerRow: 0, bitsPerPixel: 0)!
    rep.size = svg.size
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    svg.draw(in: NSRect(origin: .zero, size: svg.size))
    NSGraphicsContext.restoreGraphicsState()
    try! rep.representation(using: .png, properties: [:])!.write(to: dir.appendingPathComponent(name))
    print("\(name): \(width)x\(height)")
}
