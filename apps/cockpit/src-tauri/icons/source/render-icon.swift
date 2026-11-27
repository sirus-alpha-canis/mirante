// Render an SVG mark centred on a square background into a PNG (AppKit's own SVG support,
// macOS 14+; offline). Usage: render-icon.swift <in.svg> <out.png> <size> <#RRGGBB> <margin 0..0.5>
//
// The app icon (from apps/cockpit/src-tauri/icons/source/; the ribbon is Vibely's mark, used
// by permission, see src/assets/brand/README.md):
//   swift render-icon.swift ribbon.svg icon-light-1024.png 1024 '#FFFFFF' 0.1
//   swift render-icon.swift ribbon.svg icon-dark-1024.png 1024 '#111111' 0.1
//   pnpm --dir apps/cockpit tauri icon apps/cockpit/src-tauri/icons/source/icon-light-1024.png -o /tmp/mr-icons
// then copy 32x32.png, 128x128.png, 128x128@2x.png, icon.icns and icon.png (the files
// tauri.conf.json lists) into icons/. The dark variant is a source only: a Tauri bundle has
// one icon.
import AppKit
let a = CommandLine.arguments
guard a.count == 6, let img = NSImage(contentsOf: URL(fileURLWithPath: a[1])) else { fatalError("usage / unreadable svg") }
let size = Int(a[3])!, margin = Double(a[5])!
let hex = UInt32(a[4].dropFirst(), radix: 16)!
let bg = NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255, blue: CGFloat(hex & 0xff) / 255, alpha: 1)
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: size, height: size)
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current!.imageInterpolation = .high
bg.setFill(); NSRect(x: 0, y: 0, width: size, height: size).fill()
let box = Double(size) * (1 - 2 * margin)
let s = img.size, k = min(box / s.width, box / s.height)
let w = s.width * k, h = s.height * k
img.draw(in: NSRect(x: (Double(size) - w) / 2, y: (Double(size) - h) / 2, width: w, height: h), from: .zero, operation: .sourceOver, fraction: 1)
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: a[2]))
