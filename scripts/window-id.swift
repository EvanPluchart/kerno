// Prints the CGWindowID of the main window of the named process (default
// "kerno"), for `screencapture -l <id>`: among its normal-layer windows, the
// one on screen with the largest area. (GPUI also owns a small off-screen
// helper window, which must not be picked.)
// Usage: swift scripts/window-id.swift [owner-name]
import CoreGraphics
import Foundation

let owner = (CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "kerno").lowercased()
let windows = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]] ?? []

var best: (id: Int, rank: (Int, Double))?
for window in windows {
    let name = (window[kCGWindowOwnerName as String] as? String ?? "").lowercased()
    let layer = window[kCGWindowLayer as String] as? Int ?? -1
    guard name == owner, layer == 0 else { continue }
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    let area = (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
    let onScreen = (window[kCGWindowIsOnscreen as String] as? Bool) ?? false
    let id = window[kCGWindowNumber as String] as? Int ?? 0
    let rank = (onScreen ? 1 : 0, area)
    if let current = best, current.rank >= rank { continue }
    best = (id, rank)
}

if let best {
    print(best.id)
} else {
    FileHandle.standardError.write("no window owned by \(owner)\n".data(using: .utf8)!)
    exit(1)
}
