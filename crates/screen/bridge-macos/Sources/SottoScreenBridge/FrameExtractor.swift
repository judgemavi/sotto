@preconcurrency import AppKit
@preconcurrency import AVFoundation
@preconcurrency import CoreMedia
@preconcurrency import Foundation

@_cdecl("sotto_screen_decode_frame_png")
public func sottoScreenDecodeFramePng(
    _ rawPath: UnsafePointer<CChar>?, _ requestedNs: UInt64,
    _ outputBuffer: UnsafeMutablePointer<UInt8>?, _ outputCapacity: Int,
    _ outputLength: UnsafeMutablePointer<Int>?, _ actualNs: UnsafeMutablePointer<UInt64>?,
    _ errorBuffer: UnsafeMutablePointer<CChar>?, _ errorCapacity: Int
) -> Bool {
    guard let rawPath, let outputLength, let actualNs else {
        writeError("frame extraction received invalid pointers", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let url = URL(fileURLWithPath: String(cString: rawPath))
    guard FileManager.default.fileExists(atPath: url.path) else {
        writeError("recording file is missing", to: errorBuffer, capacity: errorCapacity)
        return false
    }

    let asset = AVURLAsset(url: url)
    let generator = AVAssetImageGenerator(asset: asset)
    generator.appliesPreferredTrackTransform = true
    generator.requestedTimeToleranceBefore = .zero
    generator.requestedTimeToleranceAfter = .zero
    let requested = CMTime(value: CMTimeValue(requestedNs), timescale: 1_000_000_000)
    var actual = CMTime.invalid
    let image: CGImage
    do {
        image = try generator.copyCGImage(at: requested, actualTime: &actual)
    } catch {
        writeError("could not decode recording frame: \(error)", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    guard actual.isNumeric else {
        writeError("decoded frame has no numeric media timestamp", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let bitmap = NSBitmapImageRep(cgImage: image)
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        writeError("could not encode decoded frame as PNG", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    outputLength.pointee = png.count
    actualNs.pointee = UInt64(max(0, CMTimeGetSeconds(actual) * 1_000_000_000))
    guard let outputBuffer, outputCapacity >= png.count else {
        return true
    }
    png.copyBytes(to: outputBuffer, count: png.count)
    return true
}

private func writeError(
    _ message: String, to buffer: UnsafeMutablePointer<CChar>?, capacity: Int
) {
    guard let buffer, capacity > 0 else { return }
    let bytes = Array(message.utf8.prefix(capacity - 1))
    for (index, byte) in bytes.enumerated() {
        buffer[index] = CChar(bitPattern: byte)
    }
    buffer[bytes.count] = 0
}
