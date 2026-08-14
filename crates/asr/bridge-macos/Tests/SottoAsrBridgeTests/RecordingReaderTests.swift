import AVFoundation
import CoreMedia
import Foundation
import Testing
import UniformTypeIdentifiers
@testable import SottoAsrBridge

private final class ReceivedAudio: @unchecked Sendable {
    var left = [Float]()
    var right = [Float]()
}

private func receiveStereo(
    _ context: UnsafeMutableRawPointer?, _ left: UnsafePointer<Float>?,
    _ right: UnsafePointer<Float>?, _ count: Int, _ timestampNs: UInt64
) {
    guard let context, let left, let right else { return }
    let received = Unmanaged<ReceivedAudio>.fromOpaque(context).takeUnretainedValue()
    received.left.append(contentsOf: UnsafeBufferPointer(start: left, count: count))
    received.right.append(contentsOf: UnsafeBufferPointer(start: right, count: count))
}

@Test func readsFixedStereoChannelsFromRecordingMediaTime() async throws {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("sotto-asr-reader-\(UUID().uuidString).mp4")
    defer { try? FileManager.default.removeItem(at: url) }
    try await writeStereoFixture(url)

    var durationNs = UInt64(0)
    var error = [CChar](repeating: 0, count: 2_048)
    let durationReadable = url.path.withCString { path in
        error.withUnsafeMutableBufferPointer { buffer in
            sottoAsrRecordingDuration(path, &durationNs, buffer.baseAddress, buffer.count)
        }
    }
    #expect(durationReadable)
    #expect(durationNs >= 90_000_000)

    let received = ReceivedAudio()
    let context = Unmanaged.passUnretained(received).toOpaque()
    let audioReadable = url.path.withCString { path in
        error.withUnsafeMutableBufferPointer { buffer in
            sottoAsrReadStereo(
                path, 0, durationNs, receiveStereo, context,
                buffer.baseAddress, buffer.count
            )
        }
    }
    #expect(audioReadable)
    #expect(received.left.count >= 1_400)
    #expect(received.left.count == received.right.count)
    #expect(mean(received.left) > 0.1)
    #expect(mean(received.right) < -0.1)
}

@Test func readsCommittedFragmentWhileSegmentWriterIsStillOpen() async throws {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("sotto-asr-growing-\(UUID().uuidString).mp4")
    defer { try? FileManager.default.removeItem(at: url) }
    let fixture = try GrowingSegmentFixture(url: url)
    try fixture.append(seconds: 2.2)
    #expect(fixture.waitForSegments(2, timeout: 5))

    var durationNs = UInt64(0)
    var error = [CChar](repeating: 0, count: 2_048)
    let durationReadable = url.path.withCString { path in
        error.withUnsafeMutableBufferPointer { buffer in
            sottoAsrRecordingDuration(path, &durationNs, buffer.baseAddress, buffer.count)
        }
    }
    #expect(durationReadable)
    #expect(durationNs >= 900_000_000)

    let received = ReceivedAudio()
    let context = Unmanaged.passUnretained(received).toOpaque()
    let audioReadable = url.path.withCString { path in
        error.withUnsafeMutableBufferPointer { buffer in
            sottoAsrReadStereo(
                path, 0, durationNs, receiveStereo, context,
                buffer.baseAddress, buffer.count
            )
        }
    }
    #expect(audioReadable)
    #expect(received.left.count >= 14_000)
    #expect(mean(received.left) > 0.1)
    #expect(mean(received.right) < -0.1)

    await fixture.finish()
}

private func writeStereoFixture(_ url: URL) async throws {
    let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
    let playbackInput = AVAssetWriterInput(
        mediaType: .audio,
        outputSettings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: 2,
            AVEncoderBitRateKey: 192_000,
        ],
        sourceFormatHint: try stereoFormat()
    )
    let isolatedInput = AVAssetWriterInput(
        mediaType: .audio,
        outputSettings: [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: 2,
            AVEncoderBitRateKey: 192_000,
        ],
        sourceFormatHint: try stereoFormat()
    )
    #expect(writer.canAdd(playbackInput))
    #expect(writer.canAdd(isolatedInput))
    writer.add(playbackInput)
    writer.add(isolatedInput)
    #expect(writer.startWriting())
    writer.startSession(atSourceTime: .zero)
    #expect(playbackInput.append(try stereoSample(left: 0.75, right: 0.75)))
    #expect(isolatedInput.append(try stereoSample()))
    playbackInput.markAsFinished()
    isolatedInput.markAsFinished()
    await withCheckedContinuation { continuation in
        writer.finishWriting { continuation.resume() }
    }
    #expect(writer.status == .completed)
}

private func stereoFormat() throws -> CMAudioFormatDescription {
    var description = AudioStreamBasicDescription(
        mSampleRate: 48_000,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: 8,
        mFramesPerPacket: 1,
        mBytesPerFrame: 8,
        mChannelsPerFrame: 2,
        mBitsPerChannel: 32,
        mReserved: 0
    )
    var format: CMAudioFormatDescription?
    let status = CMAudioFormatDescriptionCreate(
        allocator: kCFAllocatorDefault,
        asbd: &description,
        layoutSize: 0,
        layout: nil,
        magicCookieSize: 0,
        magicCookie: nil,
        extensions: nil,
        formatDescriptionOut: &format
    )
    guard status == noErr, let format else { throw FixtureError.format }
    return format
}

private func stereoSample(left: Float = 0.25, right: Float = -0.5) throws -> CMSampleBuffer {
    let frames = 4_800
    var samples = [Float]()
    samples.reserveCapacity(frames * 2)
    for _ in 0..<frames {
        samples.append(left)
        samples.append(right)
    }
    let data = samples.withUnsafeBytes { Data($0) }
    var block: CMBlockBuffer?
    guard CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault,
        memoryBlock: nil,
        blockLength: data.count,
        blockAllocator: kCFAllocatorDefault,
        customBlockSource: nil,
        offsetToData: 0,
        dataLength: data.count,
        flags: 0,
        blockBufferOut: &block
    ) == noErr, let block else { throw FixtureError.buffer }
    let copied = data.withUnsafeBytes { bytes in
        CMBlockBufferReplaceDataBytes(
            with: bytes.baseAddress!, blockBuffer: block,
            offsetIntoDestination: 0, dataLength: data.count
        )
    }
    guard copied == noErr else { throw FixtureError.buffer }
    var timing = CMSampleTimingInfo(
        duration: CMTime(value: 1, timescale: 48_000),
        presentationTimeStamp: .zero,
        decodeTimeStamp: .invalid
    )
    var sampleSize = 8
    var sample: CMSampleBuffer?
    let created = CMSampleBufferCreateReady(
        allocator: kCFAllocatorDefault,
        dataBuffer: block,
        formatDescription: try stereoFormat(),
        sampleCount: frames,
        sampleTimingEntryCount: 1,
        sampleTimingArray: &timing,
        sampleSizeEntryCount: 1,
        sampleSizeArray: &sampleSize,
        sampleBufferOut: &sample
    )
    guard created == noErr, let sample else { throw FixtureError.buffer }
    return sample
}

private final class GrowingSegmentFixture: NSObject, AVAssetWriterDelegate, @unchecked Sendable {
    private let writer: AVAssetWriter
    private let audio: AVAssetWriterInput
    private let video: AVAssetWriterInput
    private let handle: FileHandle
    private let condition = NSCondition()
    private var segmentCount = 0
    private var packet = 0

    init(url: URL) throws {
        guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
            throw FixtureError.buffer
        }
        handle = try FileHandle(forWritingTo: url)
        writer = AVAssetWriter(contentType: UTType.mpeg4Movie)
        audio = AVAssetWriterInput(
            mediaType: .audio,
            outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 192_000,
            ],
            sourceFormatHint: try stereoFormat()
        )
        video = AVAssetWriterInput(
            mediaType: .video,
            outputSettings: [
                AVVideoCodecKey: AVVideoCodecType.h264,
                AVVideoWidthKey: 16,
                AVVideoHeightKey: 16,
                AVVideoCompressionPropertiesKey: [
                    AVVideoMaxKeyFrameIntervalKey: 1,
                ],
            ]
        )
        super.init()
        writer.outputFileTypeProfile = .mpeg4AppleHLS
        writer.preferredOutputSegmentInterval = CMTime(seconds: 1, preferredTimescale: 10)
        writer.initialSegmentStartTime = .zero
        writer.delegate = self
        for input in [audio, video] {
            #expect(writer.canAdd(input))
            writer.add(input)
        }
        #expect(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
    }

    func append(seconds: Double) throws {
        let packets = Int((seconds * 10).rounded(.up))
        while packet < packets {
            while !audio.isReadyForMoreMediaData || !video.isReadyForMoreMediaData {
                Thread.sleep(forTimeInterval: 0.001)
            }
            #expect(audio.append(try stereoSample(packet: packet)))
            #expect(video.append(try videoSample(packet: packet)))
            packet += 1
        }
    }

    func waitForSegments(_ count: Int, timeout: TimeInterval) -> Bool {
        condition.lock()
        defer { condition.unlock() }
        let deadline = Date(timeIntervalSinceNow: timeout)
        while segmentCount < count {
            guard condition.wait(until: deadline) else { return false }
        }
        return true
    }

    func finish() async {
        audio.markAsFinished()
        video.markAsFinished()
        await withCheckedContinuation { continuation in
            writer.finishWriting { continuation.resume() }
        }
        try? handle.synchronize()
        try? handle.close()
    }

    func assetWriter(
        _ writer: AVAssetWriter, didOutputSegmentData segmentData: Data,
        segmentType: AVAssetSegmentType
    ) {
        condition.lock()
        defer { condition.unlock() }
        try? handle.write(contentsOf: segmentData)
        try? handle.synchronize()
        segmentCount += 1
        condition.broadcast()
    }
}

private func stereoSample(packet: Int) throws -> CMSampleBuffer {
    let frames = 4_800
    var samples = [Float]()
    samples.reserveCapacity(frames * 2)
    for _ in 0..<frames {
        samples.append(0.25)
        samples.append(-0.5)
    }
    let data = samples.withUnsafeBytes { Data($0) }
    var block: CMBlockBuffer?
    guard CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault,
        memoryBlock: nil,
        blockLength: data.count,
        blockAllocator: kCFAllocatorDefault,
        customBlockSource: nil,
        offsetToData: 0,
        dataLength: data.count,
        flags: 0,
        blockBufferOut: &block
    ) == noErr, let block else { throw FixtureError.buffer }
    let copied = data.withUnsafeBytes { bytes in
        CMBlockBufferReplaceDataBytes(
            with: bytes.baseAddress!, blockBuffer: block,
            offsetIntoDestination: 0, dataLength: data.count
        )
    }
    guard copied == noErr else { throw FixtureError.buffer }
    var timing = CMSampleTimingInfo(
        duration: CMTime(value: 1, timescale: 48_000),
        presentationTimeStamp: CMTime(value: CMTimeValue(packet * frames), timescale: 48_000),
        decodeTimeStamp: .invalid
    )
    var sampleSize = 8
    var sample: CMSampleBuffer?
    let created = CMSampleBufferCreateReady(
        allocator: kCFAllocatorDefault,
        dataBuffer: block,
        formatDescription: try stereoFormat(),
        sampleCount: frames,
        sampleTimingEntryCount: 1,
        sampleTimingArray: &timing,
        sampleSizeEntryCount: 1,
        sampleSizeArray: &sampleSize,
        sampleBufferOut: &sample
    )
    guard created == noErr, let sample else { throw FixtureError.buffer }
    return sample
}

private func videoSample(packet: Int) throws -> CMSampleBuffer {
    var pixelBuffer: CVPixelBuffer?
    guard CVPixelBufferCreate(
        kCFAllocatorDefault, 16, 16, kCVPixelFormatType_32BGRA,
        nil, &pixelBuffer
    ) == kCVReturnSuccess, let pixelBuffer else { throw FixtureError.buffer }
    CVPixelBufferLockBaseAddress(pixelBuffer, [])
    if let base = CVPixelBufferGetBaseAddress(pixelBuffer) {
        memset(base, 0, CVPixelBufferGetDataSize(pixelBuffer))
    }
    CVPixelBufferUnlockBaseAddress(pixelBuffer, [])
    var format: CMVideoFormatDescription?
    guard CMVideoFormatDescriptionCreateForImageBuffer(
        allocator: kCFAllocatorDefault,
        imageBuffer: pixelBuffer,
        formatDescriptionOut: &format
    ) == noErr, let format else { throw FixtureError.format }
    var timing = CMSampleTimingInfo(
        duration: CMTime(value: 1, timescale: 10),
        presentationTimeStamp: CMTime(value: CMTimeValue(packet), timescale: 10),
        decodeTimeStamp: .invalid
    )
    var sample: CMSampleBuffer?
    guard CMSampleBufferCreateReadyWithImageBuffer(
        allocator: kCFAllocatorDefault,
        imageBuffer: pixelBuffer,
        formatDescription: format,
        sampleTiming: &timing,
        sampleBufferOut: &sample
    ) == noErr, let sample else { throw FixtureError.buffer }
    return sample
}

private func mean(_ samples: [Float]) -> Float {
    samples.reduce(0, +) / Float(samples.count)
}

private enum FixtureError: Error {
    case buffer
    case format
}
