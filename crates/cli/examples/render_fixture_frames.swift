import AppKit

func render(path: String, background: NSColor, title: String, subtitle: String) throws {
    let size = NSSize(width: 1280, height: 720)
    let image = NSImage(size: size)
    image.lockFocus()
    background.setFill()
    NSRect(origin: .zero, size: size).fill()

    let paragraph = NSMutableParagraphStyle()
    paragraph.alignment = .center
    let titleAttributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 76, weight: .bold),
        .foregroundColor: NSColor.black,
        .paragraphStyle: paragraph,
    ]
    let subtitleAttributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 42, weight: .medium),
        .foregroundColor: NSColor.darkGray,
        .paragraphStyle: paragraph,
    ]
    title.draw(in: NSRect(x: 80, y: 390, width: 1120, height: 110), withAttributes: titleAttributes)
    subtitle.draw(in: NSRect(x: 80, y: 270, width: 1120, height: 80), withAttributes: subtitleAttributes)
    image.unlockFocus()

    guard let data = image.tiffRepresentation,
          let bitmap = NSBitmapImageRep(data: data),
          let png = bitmap.representation(using: .png, properties: [:]) else {
        throw NSError(domain: "SottoFixtureRenderer", code: 1)
    }
    try png.write(to: URL(fileURLWithPath: path))
}

guard CommandLine.arguments.count == 2 else {
    fatalError("usage: render_fixture_frames.swift <output-directory>")
}
let directory = CommandLine.arguments[1]
try render(
    path: directory + "/00000-overview.png",
    background: NSColor(calibratedRed: 0.88, green: 0.94, blue: 1.0, alpha: 1),
    title: "Sotto Product Overview",
    subtitle: "Local-first sales call copilot"
)
try render(
    path: directory + "/15000-pricing.png",
    background: NSColor(calibratedRed: 0.96, green: 0.91, blue: 0.72, alpha: 1),
    title: "Enterprise Pricing",
    subtitle: "Migration and support included"
)
