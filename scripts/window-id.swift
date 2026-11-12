// Prints the CGWindowID of the first normal window owned by the named
// process (default "kerno"), for `screencapture -l <id>`.
// Usage: swift scripts/window-id.swift [owner-name]
import CoreGraphics
import Foundation

let owner = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "kerno"
let options: CGWindowListOption = [.optionAll]
let windows = CGWindowListCopyWindowInfo(options, kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
    let name = (window[kCGWindowOwnerName as String] as? String ?? "").lowercased()
    let layer = window[kCGWindowLayer as String] as? Int ?? -1
    if name == owner.lowercased() && layer == 0 {
        print(window[kCGWindowNumber as String] as? Int ?? 0)
        exit(0)
    }
}
FileHandle.standardError.write("no window owned by \(owner)\n".data(using: .utf8)!)
exit(1)
