import Foundation
import AppKit
import Vision

guard CommandLine.arguments.count == 3 else {
    fatalError("usage: verify_fixture_vision.swift <png> <expected-text>")
}
let url = URL(fileURLWithPath: CommandLine.arguments[1])
let expected = CommandLine.arguments[2].lowercased()
guard let image = NSImage(contentsOf: url),
      let cgImage = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    throw NSError(domain: "SottoFixtureVision", code: 2)
}
let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true
let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
try handler.perform([request])
let text = (request.results ?? [])
    .compactMap { $0.topCandidates(1).first?.string }
    .joined(separator: "\n")
print(text)
guard text.lowercased().contains(expected) else {
    throw NSError(
        domain: "SottoFixtureVision",
        code: 1,
        userInfo: [NSLocalizedDescriptionKey: "expected \(expected.debugDescription), got \(text.debugDescription)"]
    )
}
