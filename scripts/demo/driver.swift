// Drives the demo: moves the real pointer, clicks and types, for scripts/record-demo.sh.
//
//   driver --display-index   prints the built-in display's index for `screencapture -D`
//   driver --duration        prints how long the tour takes, in seconds
//   driver                   runs the tour (needs Accessibility permission for the terminal)
//
// Positions come from the app's layout (src/view.rs, src/settings.rs) and the demo constants in
// src/demo.rs. When the UI changes, update `Spot` below and add steps to `tour`.

import CoreGraphics
import Foundation

// MARK: - Where things are, in points from the built-in display's top-left

let display: CGDirectDisplayID = {
    var ids = [CGDirectDisplayID](repeating: 0, count: 16)
    var count: UInt32 = 0
    CGGetActiveDisplayList(16, &ids, &count)
    return ids[0..<Int(count)].first { CGDisplayIsBuiltin($0) != 0 } ?? CGMainDisplayID()
}()
let bounds = CGDisplayBounds(display)
let cx = bounds.midX

/// The open panel is 660 pt wide, centred; its top bar is the notch's height (32 pt).
/// Settings (src/demo.rs): 680 × 500 pt, its top 380 pt down; content runs under the title bar.
enum Spot {
    static let desk = CGPoint(x: cx, y: 560)
    static let notch = CGPoint(x: cx, y: 12)
    static let onCallTab = CGPoint(x: cx - 285, y: 16)
    static let incidentsTab = CGPoint(x: cx - 210, y: 16)
    static let connect = CGPoint(x: cx + 85, y: 16)
    static let panelWeek = CGPoint(x: cx - 120, y: 240)
    static func settings(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: cx - 340 + x, y: 380 + y) }
    static let apiKey = settings(400, 135)
    static let save = settings(245, 404)
    static let belowSettings = CGPoint(x: cx + 420, y: 700)
}

// MARK: - Input

func global(_ p: CGPoint) -> CGPoint { CGPoint(x: bounds.minX + p.x, y: bounds.minY + p.y) }
var pointer = global(Spot.desk)

func post(_ type: CGEventType, _ p: CGPoint) {
    CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: .left)?.post(tap: .cghidEventTap)
}

/// Glides the pointer to `spot` with an ease-in-out curve, as a hand would.
func move(_ spot: CGPoint, _ seconds: Double = 0.7) {
    let (from, to) = (pointer, global(spot))
    let steps = max(1, Int(seconds * 120))
    for i in 1...steps {
        let t = Double(i) / Double(steps)
        let e = t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
        let p = CGPoint(x: from.x + (to.x - from.x) * e, y: from.y + (to.y - from.y) * e)
        post(.mouseMoved, p)
        usleep(useconds_t(1_000_000 / 120))
    }
    pointer = to
}

func click() {
    post(.leftMouseDown, pointer); usleep(70_000); post(.leftMouseUp, pointer)
}

func type(_ text: String, every seconds: Double = 0.045) {
    for ch in text {
        var units = Array(String(ch).utf16)
        for down in [true, false] {
            let e = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: down)
            e?.keyboardSetUnicodeString(stringLength: units.count, unicodeString: &units)
            e?.post(tap: .cghidEventTap)
        }
        usleep(useconds_t(seconds * 1_000_000))
    }
}

func wait(_ seconds: Double) { usleep(useconds_t(seconds * 1_000_000)) }

// MARK: - The tour

struct Step { let seconds: Double; let run: () -> Void }
func step(_ seconds: Double, _ run: @escaping () -> Void) -> Step { Step(seconds: seconds, run: run) }

let tour: [Step] = [
    step(1.5) { post(.mouseMoved, pointer); wait(1.5) },                          // the collapsed island
    step(3.3) { move(Spot.notch, 0.9); wait(2.4) },                               // hover: it opens
    step(2.8) { move(Spot.incidentsTab, 0.6); click(); wait(2.2) },               // incidents
    step(2.0) { move(Spot.onCallTab, 0.5); click(); wait(1.5) },                  // back to on-call
    step(2.2) { move(Spot.panelWeek, 0.6); wait(1.6) },                           // the rotation strip
    step(2.2) { move(Spot.desk, 0.7); wait(1.5) },                                // leave: it closes
    step(2.0) { move(Spot.notch, 0.8); wait(1.2) },                               // back
    step(2.3) { move(Spot.connect, 0.6); wait(0.4); click(); wait(1.3) },         // "Mock data · Connect" opens Settings
    step(3.4) { move(Spot.apiKey, 0.8); click(); wait(0.4); type("fhb-demo-7c1e9a2b4d6f8e0a3b5c"); wait(0.8) },
    step(3.6) { move(Spot.save, 0.6); click(); wait(3.0) },                       // save, test passes
    step(3.6) { move(Spot.notch, 1.0); wait(2.6) },                               // the panel now says Live
    step(2.0) { move(Spot.belowSettings, 0.8); wait(1.2) },                       // done
]

// MARK: - Main

let args = CommandLine.arguments.dropFirst()
if args.contains("--display-index") {
    var ids = [CGDirectDisplayID](repeating: 0, count: 16)
    var count: UInt32 = 0
    CGGetActiveDisplayList(16, &ids, &count)
    print((ids[0..<Int(count)].firstIndex(of: display) ?? 0) + 1)
} else if args.contains("--duration") {
    print(Int(tour.reduce(0) { $0 + $1.seconds }.rounded(.up)))
} else {
    let saved = CGEvent(source: nil)?.location
    for s in tour { s.run() }
    if let saved { CGWarpMouseCursorPosition(saved) }
}
