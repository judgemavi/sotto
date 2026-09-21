@preconcurrency import AVFoundation
@preconcurrency import AppKit
@preconcurrency import CoreGraphics
@preconcurrency import CoreMedia
@preconcurrency import CoreVideo
@preconcurrency import Darwin
@preconcurrency import Foundation
@preconcurrency import ScreenCaptureKit
@preconcurrency import UniformTypeIdentifiers

private typealias AudioCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<Float>?, Int, UInt64, UInt64
) -> Void
private typealias ErrorCallback = @convention(c) (
    UnsafeMutableRawPointer?, Int32, UnsafePointer<CChar>?
) -> Void
private typealias FrameCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int, UInt32, UInt32, UInt32, UInt32, UInt64, UInt64
) -> Void
private typealias TargetCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UnsafePointer<CChar>?,
    UnsafePointer<CChar>?, UnsafePointer<CChar>?, Int32, Bool
) -> Void

private struct BridgeConfig: @unchecked Sendable {
    let audio: AudioCallback
    let error: ErrorCallback
    let frame: FrameCallback
    let context: UnsafeMutableRawPointer?
    let recordingPath: String?

    func report(_ code: Int32, detail: String? = nil) {
        withOptionalCString(detail) { error(context, code, $0) }
    }
}

private struct PickerRequest: @unchecked Sendable {
    let callback: TargetCallback
    let context: UnsafeMutableRawPointer?
}

private final class PickedTarget: @unchecked Sendable {
    let filter: SCContentFilter
    let bundleID: String?
    let displayName: String
    let windowTitle: String?
    let kind: Int32
    let audioScoped: Bool

    init(
        filter: SCContentFilter, bundleID: String?, displayName: String,
        windowTitle: String?, kind: Int32, audioScoped: Bool
    ) {
        self.filter = filter
        self.bundleID = bundleID
        self.displayName = displayName
        self.windowTitle = windowTitle
        self.kind = kind
        self.audioScoped = audioScoped
    }
}

private final class TargetPicker: NSObject, SCContentSharingPickerObserver {
    private let callback: TargetCallback
    private let context: UnsafeMutableRawPointer?
    private let finishLock = NSLock()
    private var finished = false

    init(callback: @escaping TargetCallback, context: UnsafeMutableRawPointer?) {
        self.callback = callback
        self.context = context
    }

    func present() {
        let picker = SCContentSharingPicker.shared
        var configuration = SCContentSharingPickerConfiguration()
        configuration.allowedPickerModes = [.singleWindow, .singleApplication, .singleDisplay]
        configuration.allowsChangingSelectedContent = false
        picker.configuration = configuration
        picker.add(self)
        picker.isActive = true
        picker.present()
    }

    func contentSharingPicker(
        _ picker: SCContentSharingPicker,
        didUpdateWith filter: SCContentFilter,
        for stream: SCStream?
    ) {
        finish(picker: picker, filter: filter)
    }

    func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor stream: SCStream?) {
        finish(picker: picker, filter: nil)
    }

    func contentSharingPickerStartDidFailWithError(_ error: any Error) {
        finish(picker: SCContentSharingPicker.shared, filter: nil)
    }

    private func finish(picker: SCContentSharingPicker, filter: SCContentFilter?) {
        finishLock.lock()
        guard !finished else {
            finishLock.unlock()
            return
        }
        finished = true
        finishLock.unlock()
        picker.remove(self)
        picker.isActive = false
        guard let filter else {
            callback(context, nil, nil, nil, nil, 0, false)
            Unmanaged.passUnretained(self).release()
            return
        }

        let target = Self.describe(filter)
        let opaque = Unmanaged.passRetained(target).toOpaque()
        withOptionalCString(target.bundleID) { bundleID in
            target.displayName.withCString { displayName in
                withOptionalCString(target.windowTitle) { windowTitle in
                    callback(
                        context, opaque, bundleID, displayName, windowTitle,
                        target.kind, target.audioScoped
                    )
                }
            }
        }
        Unmanaged.passUnretained(self).release()
    }

    /// Dumps what the picker actually handed back, under SOTTO_CAPTURE_DEBUG=1.
    ///
    /// A display pick delivers audio packets whose samples are all exactly zero. If
    /// ScreenCaptureKit mixes audio from the applications named in the filter, a display
    /// filter carrying no applications would explain that precisely.
    private static func dumpFilter(_ filter: SCContentFilter) {
        guard ProcessInfo.processInfo.environment["SOTTO_CAPTURE_DEBUG"] == "1" else { return }
        var line = "filter style=\(filter.style.rawValue) rect=\(filter.contentRect) scale=\(filter.pointPixelScale)"
        if #available(macOS 15.2, *) {
            line += " apps=\(filter.includedApplications.count)"
            line += " windows=\(filter.includedWindows.count)"
            line += " displays=\(filter.includedDisplays.count)"
            let names = filter.includedApplications.prefix(5).map(\.applicationName)
            if !names.isEmpty { line += " [\(names.joined(separator: ", "))]" }
        }
        FileHandle.standardError.write(Data("\(line)\n".utf8))
    }

    private static func describe(_ filter: SCContentFilter) -> PickedTarget {
        dumpFilter(filter)
        switch filter.style {
        case .window:
            let window: SCWindow? = if #available(macOS 15.2, *) {
                filter.includedWindows.first
            } else {
                nil
            }
            let application = window?.owningApplication
            return PickedTarget(
                filter: filter,
                bundleID: application?.bundleIdentifier,
                displayName: application?.applicationName ?? window?.title ?? "Selected window",
                windowTitle: window?.title,
                kind: 2,
                audioScoped: true
            )
        case .application:
            let application: SCRunningApplication? = if #available(macOS 15.2, *) {
                filter.includedApplications.first
            } else {
                nil
            }
            return PickedTarget(
                filter: filter,
                bundleID: application?.bundleIdentifier,
                displayName: application?.applicationName ?? "Selected application",
                windowTitle: nil,
                kind: 1,
                audioScoped: true
            )
        case .display:
            return PickedTarget(
                filter: filter,
                bundleID: nil,
                displayName: "Selected display",
                windowTitle: nil,
                kind: 3,
                audioScoped: false
            )
        case .none:
            return PickedTarget(
                filter: filter,
                bundleID: nil,
                displayName: "Selected target",
                windowTitle: nil,
                kind: 0,
                audioScoped: false
            )
        @unknown default:
            return PickedTarget(
                filter: filter,
                bundleID: nil,
                displayName: "Selected target",
                windowTitle: nil,
                kind: 0,
                audioScoped: false
            )
        }
    }
}

private func withOptionalCString<Result>(
    _ string: String?, _ body: (UnsafePointer<CChar>?) -> Result
) -> Result {
    guard let string else { return body(nil) }
    return string.withCString(body)
}

private struct StrictlyIncreasingPresentationTime {
    private(set) var lastAppended: CMTime?

    mutating func append(
        _ candidate: CMTime, receiver: (CMTime) -> Bool
    ) -> Bool? {
        guard candidate.isNumeric else { return nil }
        let adjusted: CMTime
        if let lastAppended, CMTimeCompare(candidate, lastAppended) <= 0 {
            let timescale = max(candidate.timescale, lastAppended.timescale, 600)
            let previous = CMTimeConvertScale(
                lastAppended, timescale: timescale, method: .default
            )
            adjusted = CMTimeAdd(previous, CMTime(value: 1, timescale: timescale))
        } else {
            adjusted = candidate
        }
        guard receiver(adjusted) else { return false }
        lastAppended = adjusted
        return true
    }
}

private enum RecordingTrack {
    case video
    case audio
}

enum RecordingAudioChannel: Int {
    /// Stereo channel 0 / left is the selected target's meeting audio.
    case meeting = 0
    /// Stereo channel 1 / right is the local microphone.
    case microphone = 1
}

func microphoneOnlyStereo(_ microphone: [Float]) -> [Float] {
    microphone.flatMap { [$0, 0] }
}

struct AudioLevelMeasurement: Equatable {
    let peak: Double
    let rms: Double
    let sampleCount: UInt64
}

struct AudioLevelAccumulator {
    private var peak = 0.0
    private var sumSquares = 0.0
    private var sampleCount = UInt64(0)

    mutating func append(_ samples: [Float]) {
        for sample in samples {
            let value = Double(sample)
            peak = max(peak, abs(value))
            sumSquares += value * value
            sampleCount &+= 1
        }
    }

    mutating func appendInterleavedStereo(_ samples: [Float], channel: RecordingAudioChannel) {
        var index = channel.rawValue
        while index < samples.count {
            let value = Double(samples[index])
            peak = max(peak, abs(value))
            sumSquares += value * value
            sampleCount &+= 1
            index += 2
        }
    }

    var measurement: AudioLevelMeasurement {
        AudioLevelMeasurement(
            peak: peak,
            rms: sampleCount == 0 ? 0 : sqrt(sumSquares / Double(sampleCount)),
            sampleCount: sampleCount
        )
    }
}

/// Combines the independently arriving canonical mono streams into one stereo timeline.
///
/// AVAssetWriter's live HLS segment profile permits video plus one audio track, not the two
/// mono tracks used by the finalized-file writer. A short reorder window absorbs callback skew;
/// any channel that genuinely has no packet for a committed interval is represented by silence.
struct StereoRecordingMixer {
    private static let sampleRate: UInt64 = 48_000
    private static let chunkFrames: UInt64 = 4_800
    private static let reorderFrames: UInt64 = 24_000

    private var chunks: [UInt64: [Float]] = [:]
    private var nextChunk = UInt64(0)
    private var maximumObservedEnd = [
        RecordingAudioChannel.meeting: UInt64(0),
        RecordingAudioChannel.microphone: UInt64(0),
    ]

    mutating func append(
        _ samples: [Float], channel: RecordingAudioChannel, startTimeNs: UInt64
    ) -> [(startFrame: UInt64, frameCount: Int, samples: [Float])] {
        let startFrame = Self.frames(forNanoseconds: startTimeNs)
        var sourceOffset = 0
        while sourceOffset < samples.count {
            let frame = startFrame + UInt64(sourceOffset)
            let chunkIndex = frame / Self.chunkFrames
            let chunkStart = chunkIndex * Self.chunkFrames
            if chunkIndex < nextChunk {
                sourceOffset += min(
                    samples.count - sourceOffset,
                    Int(max(chunkStart + Self.chunkFrames, frame) - frame)
                )
                continue
            }
            let offset = Int(frame - chunkStart)
            let writable = min(samples.count - sourceOffset, Int(Self.chunkFrames) - offset)
            var chunk = chunks.removeValue(forKey: chunkIndex)
                ?? [Float](repeating: 0, count: Int(Self.chunkFrames) * 2)
            for index in 0..<writable {
                chunk[(offset + index) * 2 + channel.rawValue] = samples[sourceOffset + index]
            }
            chunks[chunkIndex] = chunk
            sourceOffset += writable
        }
        maximumObservedEnd[channel] = max(
            maximumObservedEnd[channel] ?? 0,
            startFrame + UInt64(samples.count)
        )
        // CPAL is started synchronously before ScreenCaptureKit finishes its asynchronous start.
        // Until both callbacks have arrived, silence in the unseen channel is startup skew, not a
        // genuinely missing packet. Committing it would make all later zero-based packets from the
        // second source too old to enter the retained recording.
        let meetingEnd = maximumObservedEnd[.meeting] ?? 0
        let microphoneEnd = maximumObservedEnd[.microphone] ?? 0
        let sharedEnd = min(meetingEnd, microphoneEnd)
        let committedThrough = sharedEnd > Self.reorderFrames
            ? sharedEnd - Self.reorderFrames : 0
        return drain(through: committedThrough)
    }

    mutating func finish() -> [(startFrame: UInt64, frameCount: Int, samples: [Float])] {
        drain(through: maximumObservedEnd.values.max() ?? 0, includePartial: true)
    }

    private mutating func drain(
        through frame: UInt64, includePartial: Bool = false
    ) -> [(startFrame: UInt64, frameCount: Int, samples: [Float])] {
        var output = [(startFrame: UInt64, frameCount: Int, samples: [Float])]()
        while nextChunk * Self.chunkFrames < frame {
            let chunkStart = nextChunk * Self.chunkFrames
            let chunkEnd = chunkStart + Self.chunkFrames
            guard chunkEnd <= frame || includePartial else { break }
            let frameCount = Int(min(Self.chunkFrames, frame - chunkStart))
            guard frameCount > 0 else { break }
            var samples = chunks.removeValue(forKey: nextChunk)
                ?? [Float](repeating: 0, count: Int(Self.chunkFrames) * 2)
            samples.removeSubrange((frameCount * 2)..<samples.count)
            output.append((chunkStart, frameCount, samples))
            nextChunk += 1
        }
        return output
    }

    static func frames(forNanoseconds nanoseconds: UInt64) -> UInt64 {
        let seconds = nanoseconds / 1_000_000_000
        let remainder = nanoseconds % 1_000_000_000
        return seconds * sampleRate + remainder * sampleRate / 1_000_000_000
    }
}

/// Owns the single retained fMP4 file while AVAssetWriter emits complete segment bytes.
/// Each delegate delivery is appended and synchronized before it is considered committed, so an
/// abrupt process loss can discard only the segment AVAssetWriter has not delivered yet.
private final class RecordingSegmentSink: NSObject, AVAssetWriterDelegate, @unchecked Sendable {
    private let handle: FileHandle
    private let lock = NSLock()
    var onFailure: (@Sendable (String) -> Void)?

    init(path: String) throws {
        let url = URL(fileURLWithPath: path)
        try? FileManager.default.removeItem(at: url)
        guard FileManager.default.createFile(atPath: path, contents: nil) else {
            throw RecordingError.cannotOpenOutput
        }
        do {
            handle = try FileHandle(forWritingTo: url)
        } catch {
            try? FileManager.default.removeItem(at: url)
            throw RecordingError.cannotOpenOutput
        }
        super.init()
    }

    func assetWriter(
        _ writer: AVAssetWriter, didOutputSegmentData segmentData: Data,
        segmentType: AVAssetSegmentType
    ) {
        lock.lock()
        defer { lock.unlock() }
        do {
            try handle.write(contentsOf: segmentData)
            try handle.synchronize()
        } catch {
            onFailure?(
                "segment file append failed for \(segmentType.rawValue): \(error.localizedDescription)"
            )
        }
    }

    func finish() throws {
        lock.lock()
        defer { lock.unlock() }
        try handle.synchronize()
        try handle.close()
    }
}

enum RecordingPlaybackFinalizationError: LocalizedError {
    case missingAudioTrack
    case cannotConfigureReader
    case cannotConfigureWriter
    case readFailed(String)
    case writeFailed(String)
    case cannotReplaceOriginal(Int32)

    var errorDescription: String? {
        switch self {
        case .missingAudioTrack:
            "fragmented recording has no audio track"
        case .cannotConfigureReader:
            "could not configure the fragmented recording reader"
        case .cannotConfigureWriter:
            "could not configure the playback-compatible MP4 writer"
        case .readFailed(let detail):
            "fragmented recording read failed: \(detail)"
        case .writeFailed(let detail):
            "playback-compatible MP4 write failed: \(detail)"
        case .cannotReplaceOriginal(let code):
            "could not atomically replace the fragmented recording (errno \(code))"
        }
    }
}

/// Converts the crash-readable fragmented recording into the conventional MP4 layout expected by
/// desktop players. The source remains the canonical recording until a complete, synchronized
/// replacement is ready, so a crash during clean-stop finalization cannot destroy committed media.
func finalizeRecordingForPlayback(at sourceURL: URL) async throws {
    let temporaryURL = sourceURL.deletingLastPathComponent().appendingPathComponent(
        ".\(sourceURL.lastPathComponent).sotto-finalizing.mp4"
    )
    try? FileManager.default.removeItem(at: temporaryURL)
    defer { try? FileManager.default.removeItem(at: temporaryURL) }

    let asset = AVURLAsset(url: sourceURL)
    guard let audioTrack = asset.tracks(withMediaType: .audio).first else {
        throw RecordingPlaybackFinalizationError.missingAudioTrack
    }
    let reader = try AVAssetReader(asset: asset)
    let audioOutput = AVAssetReaderTrackOutput(
        track: audioTrack,
        outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: 2,
            AVLinearPCMBitDepthKey: 32,
            AVLinearPCMIsFloatKey: true,
            AVLinearPCMIsNonInterleaved: false,
        ]
    )
    guard reader.canAdd(audioOutput) else {
        throw RecordingPlaybackFinalizationError.cannotConfigureReader
    }
    reader.add(audioOutput)

    let outputWriter = try AVAssetWriter(outputURL: temporaryURL, fileType: .mp4)
    outputWriter.shouldOptimizeForNetworkUse = true
    var stereoLayout = AudioChannelLayout()
    stereoLayout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo
    let stereoLayoutData = Data(
        bytes: &stereoLayout, count: MemoryLayout<AudioChannelLayout>.size
    )
    let audioSettings: [String: Any] = [
        AVFormatIDKey: kAudioFormatMPEG4AAC,
        AVSampleRateKey: 48_000,
        AVNumberOfChannelsKey: 2,
        AVEncoderBitRateKey: 192_000,
        AVChannelLayoutKey: stereoLayoutData,
    ]
    let playbackAudioInput = AVAssetWriterInput(
        mediaType: .audio,
        outputSettings: audioSettings
    )
    let isolatedAudioInput = AVAssetWriterInput(
        mediaType: .audio,
        outputSettings: audioSettings
    )
    guard outputWriter.canAdd(playbackAudioInput), outputWriter.canAdd(isolatedAudioInput) else {
        throw RecordingPlaybackFinalizationError.cannotConfigureWriter
    }
    // Players select the first audio track by default. It is deliberately a centered playback mix;
    // the second track retains left=meeting/right=microphone for deterministic retranscription.
    outputWriter.add(playbackAudioInput)
    outputWriter.add(isolatedAudioInput)

    var videoOutput: AVAssetReaderTrackOutput?
    var videoInput: AVAssetWriterInput?
    if let videoTrack = asset.tracks(withMediaType: .video).first,
       let rawFormat = videoTrack.formatDescriptions.first {
        let format = rawFormat as! CMFormatDescription
        let candidateOutput = AVAssetReaderTrackOutput(track: videoTrack, outputSettings: nil)
        let candidateInput = AVAssetWriterInput(
            mediaType: .video, outputSettings: nil, sourceFormatHint: format
        )
        guard reader.canAdd(candidateOutput), outputWriter.canAdd(candidateInput) else {
            throw RecordingPlaybackFinalizationError.cannotConfigureWriter
        }
        reader.add(candidateOutput)
        outputWriter.add(candidateInput)
        videoOutput = candidateOutput
        videoInput = candidateInput
    }

    guard outputWriter.startWriting(), reader.startReading() else {
        throw RecordingPlaybackFinalizationError.cannotConfigureWriter
    }
    outputWriter.startSession(atSourceTime: .zero)
    var audioPending = true
    var videoPending = videoOutput != nil
    while audioPending || videoPending {
        var progressed = false
        if audioPending, playbackAudioInput.isReadyForMoreMediaData,
           isolatedAudioInput.isReadyForMoreMediaData {
            if let sample = audioOutput.copyNextSampleBuffer() {
                guard let playbackSample = centeredPlaybackSample(from: sample),
                      playbackAudioInput.append(playbackSample),
                      isolatedAudioInput.append(sample) else {
                    throw RecordingPlaybackFinalizationError.writeFailed(
                        outputWriter.error?.localizedDescription ?? "audio append failed"
                    )
                }
            } else {
                playbackAudioInput.markAsFinished()
                isolatedAudioInput.markAsFinished()
                audioPending = false
            }
            progressed = true
        }
        if videoPending, let videoOutput, let videoInput,
           videoInput.isReadyForMoreMediaData {
            if let sample = videoOutput.copyNextSampleBuffer() {
                guard videoInput.append(sample) else {
                    throw RecordingPlaybackFinalizationError.writeFailed(
                        outputWriter.error?.localizedDescription ?? "video append failed"
                    )
                }
            } else {
                videoInput.markAsFinished()
                videoPending = false
            }
            progressed = true
        }
        if !progressed { try await Task.sleep(nanoseconds: 500_000) }
        if reader.status == .failed {
            throw RecordingPlaybackFinalizationError.readFailed(
                reader.error?.localizedDescription ?? "reader status failed"
            )
        }
        if outputWriter.status == .failed {
            throw RecordingPlaybackFinalizationError.writeFailed(
                outputWriter.error?.localizedDescription ?? "writer status failed"
            )
        }
    }
    await withCheckedContinuation { continuation in
        outputWriter.finishWriting { continuation.resume() }
    }
    guard outputWriter.status == .completed else {
        throw RecordingPlaybackFinalizationError.writeFailed(
            outputWriter.error?.localizedDescription
                ?? "writer status \(outputWriter.status.rawValue)"
        )
    }

    let handle = try FileHandle(forWritingTo: temporaryURL)
    try handle.synchronize()
    try handle.close()
    let renameStatus = temporaryURL.path.withCString { temporaryPath in
        sourceURL.path.withCString { sourcePath in
            Darwin.rename(temporaryPath, sourcePath)
        }
    }
    guard renameStatus == 0 else {
        throw RecordingPlaybackFinalizationError.cannotReplaceOriginal(errno)
    }
}

private func centeredPlaybackSample(from source: CMSampleBuffer) -> CMSampleBuffer? {
    guard let sourceBlock = CMSampleBufferGetDataBuffer(source),
          let format = source.formatDescription else { return nil }
    let length = CMBlockBufferGetDataLength(sourceBlock)
    guard length > 0, length % (MemoryLayout<Float>.size * 2) == 0 else { return nil }
    var bytes = [UInt8](repeating: 0, count: length)
    guard CMBlockBufferCopyDataBytes(
        sourceBlock, atOffset: 0, dataLength: length, destination: &bytes
    ) == noErr else { return nil }
    bytes.withUnsafeMutableBytes { raw in
        let samples = raw.bindMemory(to: Float.self)
        var frame = 0
        while frame * 2 + 1 < samples.count {
            let mixed = max(-1, min(1, samples[frame * 2] + samples[frame * 2 + 1]))
            samples[frame * 2] = mixed
            samples[frame * 2 + 1] = mixed
            frame += 1
        }
    }
    var block: CMBlockBuffer?
    guard CMBlockBufferCreateWithMemoryBlock(
        allocator: kCFAllocatorDefault,
        memoryBlock: nil,
        blockLength: length,
        blockAllocator: kCFAllocatorDefault,
        customBlockSource: nil,
        offsetToData: 0,
        dataLength: length,
        flags: 0,
        blockBufferOut: &block
    ) == noErr, let block else { return nil }
    let copied = bytes.withUnsafeBytes { raw in
        CMBlockBufferReplaceDataBytes(
            with: raw.baseAddress!, blockBuffer: block,
            offsetIntoDestination: 0, dataLength: length
        )
    }
    guard copied == noErr else { return nil }
    var timingCount = 0
    guard CMSampleBufferGetSampleTimingInfoArray(
        source, entryCount: 0, arrayToFill: nil, entriesNeededOut: &timingCount
    ) == noErr else { return nil }
    var timing = [CMSampleTimingInfo](
        repeating: CMSampleTimingInfo(
            duration: .invalid, presentationTimeStamp: .invalid, decodeTimeStamp: .invalid
        ),
        count: timingCount
    )
    guard timing.withUnsafeMutableBufferPointer({ buffer in
        CMSampleBufferGetSampleTimingInfoArray(
            source, entryCount: timingCount, arrayToFill: buffer.baseAddress,
            entriesNeededOut: &timingCount
        )
    }) == noErr else { return nil }
    var sampleSize = MemoryLayout<Float>.size * 2
    var result: CMSampleBuffer?
    let status = timing.withUnsafeBufferPointer { timingBuffer in
        CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: block,
            formatDescription: format,
            sampleCount: CMSampleBufferGetNumSamples(source),
            sampleTimingEntryCount: timingCount,
            sampleTimingArray: timingBuffer.baseAddress,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &result
        )
    }
    return status == noErr ? result : nil
}

private final class RecordingWriter: @unchecked Sendable {
    private let writer: AVAssetWriter
    private let segmentSink: RecordingSegmentSink
    private let videoInput: AVAssetWriterInput?
    private let audioInput: AVAssetWriterInput
    private let queue = DispatchQueue(label: "dev.sotto.capture.recording", qos: .userInteractive)
    private let path: String
    private let maxBytes: UInt64?
    private let measuresAudioLevels: Bool
    private var videoOrigin: CMTime?
    private var systemAudioOriginNs: UInt64?
    private var microphoneOriginNs: UInt64?
    private var videoPresentationTime = StrictlyIncreasingPresentationTime()
    private var audioPresentationTime = StrictlyIncreasingPresentationTime()
    private var microphoneNormalizer = MonoAudioNormalizer()
    private var audioMixer = StereoRecordingMixer()
    private var callbackMeetingLevels = AudioLevelAccumulator()
    private var callbackMicrophoneLevels = AudioLevelAccumulator()
    private var mixedMeetingLevels = AudioLevelAccumulator()
    private var mixedMicrophoneLevels = AudioLevelAccumulator()
    private var failed = false
    var onFailure: (@Sendable (String) -> Void)?

    init(path: String, width: Int, height: Int, includesVideo: Bool = true) throws {
        self.path = path
        maxBytes = ProcessInfo.processInfo.environment["SOTTO_RECORDING_MAX_BYTES"]
            .flatMap(UInt64.init)
        measuresAudioLevels = ProcessInfo.processInfo.environment["SOTTO_RECORDING_AUDIO_LEVELS"]
            == "1"
        guard let contentType = UTType(AVFileType.mp4.rawValue) else {
            throw RecordingError.invalidContentType
        }
        writer = AVAssetWriter(contentType: contentType)
        segmentSink = try RecordingSegmentSink(path: path)
        writer.outputFileTypeProfile = .mpeg4AppleHLS
        // Two seconds, not five: this interval is the floor on how far behind live the transcript
        // can run, because a reader only sees whole committed segments. It is also the abrupt-loss
        // window, so shortening it tightens durability rather than trading it away. Kept in step
        // with `SEGMENT_COMMIT_INTERVAL` in crates/asr/src/recording.rs, which cannot read it.
        writer.preferredOutputSegmentInterval = CMTime(seconds: 2, preferredTimescale: 1)
        writer.initialSegmentStartTime = .zero
        videoInput = includesVideo ? AVAssetWriterInput(
                mediaType: .video,
                outputSettings: [
                    AVVideoCodecKey: AVVideoCodecType.h264,
                    AVVideoWidthKey: width,
                    AVVideoHeightKey: height,
                    AVVideoCompressionPropertiesKey: [
                        AVVideoAverageBitRateKey: 2_000_000,
                        AVVideoMaxKeyFrameIntervalKey: 15,
                        AVVideoMaxKeyFrameIntervalDurationKey: 0.5,
                    ],
                ]
            ) : nil
        let audioSettings: [String: Any] = [
            AVFormatIDKey: kAudioFormatMPEG4AAC,
            AVSampleRateKey: 48_000,
            AVNumberOfChannelsKey: 2,
            AVEncoderBitRateKey: 192_000,
            AVChannelLayoutKey: Self.stereoChannelLayoutData(),
        ]
        let canonicalAudio = try Self.canonicalAudioFormat(channels: 2)
        audioInput = AVAssetWriterInput(
            mediaType: .audio, outputSettings: audioSettings, sourceFormatHint: canonicalAudio
        )
        for input in [videoInput, audioInput].compactMap({ $0 }) {
            input.expectsMediaDataInRealTime = true
            guard writer.canAdd(input) else { throw RecordingError.cannotAddTrack }
            writer.add(input)
        }
        segmentSink.onFailure = { [weak self] cause in
            self?.queue.async { [weak self] in self?.reportFailure(cause) }
        }
        writer.delegate = segmentSink
        guard writer.startWriting() else {
            throw writer.error ?? RecordingError.cannotStart
        }
        writer.startSession(atSourceTime: .zero)
    }

    func appendVideo(_ sampleBuffer: CMSampleBuffer) {
        queue.sync {
            guard !failed, let videoInput else { return }
            let origin = videoOrigin ?? sampleBuffer.presentationTimeStamp
            videoOrigin = origin
            append(
                retimed(sampleBuffer, origin: origin), to: videoInput,
                track: .video, label: "video"
            )
        }
    }

    func appendSystemAudio(
        _ samples: UnsafePointer<Float>?, count: Int, streamTimeNs: UInt64
    ) {
        guard let samples, count > 0 else { return }
        let canonical = Array(UnsafeBufferPointer(start: samples, count: count))
        queue.async { [weak self] in
            guard let self, !self.failed else { return }
            if self.measuresAudioLevels {
                self.callbackMeetingLevels.append(canonical)
            }
            let origin = self.systemAudioOriginNs ?? streamTimeNs
            self.systemAudioOriginNs = origin
            self.appendMixedAudio(
                self.audioMixer.append(
                    canonical, channel: .meeting,
                    startTimeNs: streamTimeNs >= origin ? streamTimeNs - origin : 0
                ),
                label: "stereo audio (meeting channel)"
            )
        }
    }

    func appendMicrophone(
        _ samples: UnsafePointer<Float>, count: Int, sampleRate: UInt32,
        channels: UInt32, streamTimeNs: UInt64
    ) {
        guard count > 0, sampleRate > 0, channels > 0 else { return }
        let input = Array(UnsafeBufferPointer(start: samples, count: count))
        queue.async { [weak self] in
            guard let self, !self.failed else { return }
            let normalized = self.microphoneNormalizer.process(
                input, sampleRate: sampleRate, channels: channels
            )
            guard !normalized.isEmpty else { return }
            if self.measuresAudioLevels {
                self.callbackMicrophoneLevels.append(normalized)
            }
            let origin = self.microphoneOriginNs ?? streamTimeNs
            self.microphoneOriginNs = origin
            self.appendMixedAudio(
                self.audioMixer.append(
                    normalized, channel: .microphone,
                    startTimeNs: streamTimeNs >= origin ? streamTimeNs - origin : 0
                ),
                label: "stereo audio (microphone channel from \(sampleRate)Hz/\(channels)ch)"
            )
        }
    }

    /// A microphone-only recording carries its sole source on channel 0. Channel 1 is explicit
    /// silence; the microphone-only ASR layout reads channel 0 once and labels it as the mic.
    func appendMicrophoneOnly(
        _ samples: UnsafePointer<Float>, count: Int, sampleRate: UInt32,
        channels: UInt32, streamTimeNs: UInt64
    ) {
        guard count > 0, sampleRate > 0, channels > 0 else { return }
        let input = Array(UnsafeBufferPointer(start: samples, count: count))
        queue.async { [weak self] in
            guard let self, !self.failed else { return }
            let normalized = self.microphoneNormalizer.process(
                input, sampleRate: sampleRate, channels: channels
            )
            guard !normalized.isEmpty else { return }
            if self.measuresAudioLevels {
                self.callbackMicrophoneLevels.append(normalized)
            }
            let origin = self.microphoneOriginNs ?? streamTimeNs
            self.microphoneOriginNs = origin
            let relativeTime = streamTimeNs >= origin ? streamTimeNs - origin : 0
            let stereo = microphoneOnlyStereo(normalized)
            self.appendMixedAudio(
                [(StereoRecordingMixer.frames(forNanoseconds: relativeTime), normalized.count, stereo)],
                label: "stereo audio (microphone-only on channel 0)"
            )
        }
    }

    func finish() async {
        let callbackLevels: (AudioLevelMeasurement, AudioLevelMeasurement)? = queue.sync {
            guard measuresAudioLevels else { return nil }
            return (callbackMeetingLevels.measurement, callbackMicrophoneLevels.measurement)
        }
        queue.sync {
            if !failed {
                appendMixedAudio(audioMixer.finish(), label: "stereo audio finalization")
            }
            videoInput?.markAsFinished()
            audioInput.markAsFinished()
        }
        await withCheckedContinuation { continuation in
            writer.finishWriting { continuation.resume() }
        }
        if writer.status != .completed {
            reportFailure("finishWriting ended with status \(writer.status.rawValue), expected .completed")
        }
        do {
            try segmentSink.finish()
        } catch {
            reportFailure("segment file finalization failed: \(error.localizedDescription)")
        }
        if writer.status == .completed, !queue.sync(execute: { failed }) {
            do {
                try await finalizeRecordingForPlayback(at: URL(fileURLWithPath: path))
            } catch {
                reportFailure("playback finalization failed: \(error.localizedDescription)")
            }
        }
        if let callbackLevels {
            let mixedLevels = queue.sync {
                (mixedMeetingLevels.measurement, mixedMicrophoneLevels.measurement)
            }
            let fileLevels = Self.measureFinalizedAudio(path: path)
            Self.reportAudioLevels(
                callbackMeeting: callbackLevels.0,
                callbackMicrophone: callbackLevels.1,
                mixedMeeting: mixedLevels.0,
                mixedMicrophone: mixedLevels.1,
                fileMeeting: fileLevels?.0,
                fileMicrophone: fileLevels?.1
            )
        }
    }

    private func append(
        _ sampleBuffer: CMSampleBuffer?, to input: AVAssetWriterInput,
        track: RecordingTrack, label: String
    ) {
        guard let sampleBuffer else {
            reportFailure("\(label): sample buffer was nil before append")
            return
        }
        let deadline = Date(timeIntervalSinceNow: 5)
        while writer.status == .writing && !input.isReadyForMoreMediaData && Date() < deadline {
            Thread.sleep(forTimeInterval: 0.0005)
        }
        guard writer.status == .writing else {
            reportFailure("\(label): writer status is \(writer.status.rawValue), expected .writing")
            return
        }
        guard input.isReadyForMoreMediaData else {
            reportFailure("\(label): input not ready for more media data after 5s")
            return
        }
        let candidate = sampleBuffer.presentationTimeStamp
        var presentedTimestamp = candidate
        let receiver: (CMTime) -> Bool = { adjusted in
            presentedTimestamp = adjusted
            let prepared = CMTimeCompare(candidate, adjusted) == 0
                ? sampleBuffer
                : self.shifted(sampleBuffer, by: CMTimeSubtract(adjusted, candidate))
            return prepared.map(input.append) ?? false
        }
        let appended: Bool?
        switch track {
        case .video:
            appended = videoPresentationTime.append(candidate, receiver: receiver)
        case .audio:
            appended = audioPresentationTime.append(candidate, receiver: receiver)
        }
        guard let appended else {
            reportFailure("\(label): sample buffer had no valid presentation timestamp")
            return
        }
        guard appended else {
            reportFailure("\(label): input.append returned false, pts=\(presentedTimestamp.seconds)s")
            return
        }
        if let maxBytes,
           let attributes = try? FileManager.default.attributesOfItem(atPath: path),
           let size = attributes[.size] as? NSNumber,
           size.uint64Value >= maxBytes {
            reportFailure("\(label): SOTTO_RECORDING_MAX_BYTES cap of \(maxBytes) reached")
        }
    }

    /// Records why the recording stopped.
    ///
    /// Every cause here uses terminal status -8. The callback carries this complete diagnostic
    /// line across FFI so Rust and the app can preserve the actual writer cause without guessing
    /// that every failure means disk-full.
    private func reportFailure(_ cause: String) {
        guard !failed else { return }
        failed = true
        var line = "sotto: recording failed: \(cause)"
        if let error = writer.error {
            let ns = error as NSError
            line += " | writer.error: \(error.localizedDescription)"
            line += " | \(ns.domain) code=\(ns.code)"
            // -11800 is AVErrorUnknown, a wrapper. The actionable OSStatus is underneath it.
            if let underlying = ns.userInfo[NSUnderlyingErrorKey] as? NSError {
                line += " | underlying: \(underlying.domain) code=\(underlying.code) \(underlying.localizedDescription)"
                if let deeper = underlying.userInfo[NSUnderlyingErrorKey] as? NSError {
                    line += " | deeper: \(deeper.domain) code=\(deeper.code)"
                }
            }
            if let reason = ns.localizedFailureReason { line += " | reason: \(reason)" }
        }
        FileHandle.standardError.write(Data("\(line)\n".utf8))
        onFailure?(line)
    }

    private func appendMixedAudio(
        _ chunks: [(startFrame: UInt64, frameCount: Int, samples: [Float])], label: String
    ) {
        for chunk in chunks {
            if measuresAudioLevels {
                mixedMeetingLevels.appendInterleavedStereo(chunk.samples, channel: .meeting)
                mixedMicrophoneLevels.appendInterleavedStereo(chunk.samples, channel: .microphone)
            }
            let bytes = chunk.samples.withUnsafeBytes { Data($0) }
            guard let sampleBuffer = Self.makeAudioSample(
                bytes: bytes, sampleCount: chunk.frameCount * 2, sampleRate: 48_000,
                channels: 2,
                presentationTime: CMTime(value: CMTimeValue(chunk.startFrame), timescale: 48_000)
            ) else {
                reportFailure("\(label): could not construct canonical 48000Hz/2ch sample")
                return
            }
            append(sampleBuffer, to: audioInput, track: .audio, label: label)
        }
    }

    private static func measureFinalizedAudio(
        path: String
    ) -> (AudioLevelMeasurement, AudioLevelMeasurement)? {
        let asset = AVURLAsset(url: URL(fileURLWithPath: path))
        let tracks = asset.tracks(withMediaType: .audio)
        guard let track = tracks.count > 1 ? tracks.last : tracks.first,
              let reader = try? AVAssetReader(asset: asset) else { return nil }
        let output = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [
                AVFormatIDKey: kAudioFormatLinearPCM,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVLinearPCMBitDepthKey: 32,
                AVLinearPCMIsFloatKey: true,
                AVLinearPCMIsNonInterleaved: false,
            ]
        )
        guard reader.canAdd(output) else { return nil }
        reader.add(output)
        guard reader.startReading() else { return nil }
        var meeting = AudioLevelAccumulator()
        var microphone = AudioLevelAccumulator()
        while let sampleBuffer = output.copyNextSampleBuffer() {
            guard let block = CMSampleBufferGetDataBuffer(sampleBuffer) else { return nil }
            let length = CMBlockBufferGetDataLength(block)
            guard length > 0, length % MemoryLayout<Float>.size == 0 else { return nil }
            var contiguous = [UInt8](repeating: 0, count: length)
            guard CMBlockBufferCopyDataBytes(
                block, atOffset: 0, dataLength: length, destination: &contiguous
            ) == noErr else { return nil }
            contiguous.withUnsafeBytes { raw in
                let floats = raw.bindMemory(to: Float.self)
                let samples = Array(floats)
                meeting.appendInterleavedStereo(samples, channel: .meeting)
                microphone.appendInterleavedStereo(samples, channel: .microphone)
            }
        }
        guard reader.status == .completed else { return nil }
        return (meeting.measurement, microphone.measurement)
    }

    private static func reportAudioLevels(
        callbackMeeting: AudioLevelMeasurement,
        callbackMicrophone: AudioLevelMeasurement,
        mixedMeeting: AudioLevelMeasurement,
        mixedMicrophone: AudioLevelMeasurement,
        fileMeeting: AudioLevelMeasurement?,
        fileMicrophone: AudioLevelMeasurement?
    ) {
        func fields(_ measurement: AudioLevelMeasurement?) -> String {
            guard let measurement else { return "unavailable" }
            return String(
                format: "peak=%.9f rms=%.9f samples=%llu",
                measurement.peak, measurement.rms, measurement.sampleCount
            )
        }
        func ratio(_ numerator: Double, _ denominator: Double) -> String {
            guard denominator > 0 else { return "undefined" }
            return String(format: "%.9f", numerator / denominator)
        }
        let line = """
        sotto: recording audio levels | callback meeting \(fields(callbackMeeting)) | callback microphone \(fields(callbackMicrophone)) | mixed meeting \(fields(mixedMeeting)) | mixed microphone \(fields(mixedMicrophone)) | file meeting \(fields(fileMeeting)) | file microphone \(fields(fileMicrophone)) | meeting mixed/callback rms=\(ratio(mixedMeeting.rms, callbackMeeting.rms)) | microphone mixed/callback rms=\(ratio(mixedMicrophone.rms, callbackMicrophone.rms)) | meeting file/callback rms=\(ratio(fileMeeting?.rms ?? 0, callbackMeeting.rms)) | microphone file/callback rms=\(ratio(fileMicrophone?.rms ?? 0, callbackMicrophone.rms))
        """
        FileHandle.standardError.write(Data("\(line)\n".utf8))
    }

    private static func canonicalAudioFormat(channels: UInt32) throws -> CMAudioFormatDescription {
        var description = AudioStreamBasicDescription(
            mSampleRate: 48_000,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: channels * UInt32(MemoryLayout<Float>.size),
            mFramesPerPacket: 1,
            mBytesPerFrame: channels * UInt32(MemoryLayout<Float>.size),
            mChannelsPerFrame: channels,
            mBitsPerChannel: UInt32(MemoryLayout<Float>.size * 8),
            mReserved: 0
        )
        var channelLayout = AudioChannelLayout()
        channelLayout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo
        var format: CMAudioFormatDescription?
        let status = withUnsafePointer(to: &channelLayout) { layout in
            CMAudioFormatDescriptionCreate(
                allocator: kCFAllocatorDefault,
                asbd: &description,
                layoutSize: MemoryLayout<AudioChannelLayout>.size,
                layout: layout,
                magicCookieSize: 0,
                magicCookie: nil,
                extensions: nil,
                formatDescriptionOut: &format
            )
        }
        guard status == noErr, let format else { throw RecordingError.cannotCreateAudioFormat }
        return format
    }

    private static func stereoChannelLayoutData() -> Data {
        var layout = AudioChannelLayout()
        layout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo
        return Data(bytes: &layout, count: MemoryLayout<AudioChannelLayout>.size)
    }

    private func retimed(_ sampleBuffer: CMSampleBuffer, origin: CMTime) -> CMSampleBuffer? {
        shifted(sampleBuffer, by: CMTimeMultiplyByFloat64(origin, multiplier: -1))
    }

    private func shifted(_ sampleBuffer: CMSampleBuffer, by offset: CMTime) -> CMSampleBuffer? {
        var count = 0
        guard CMSampleBufferGetSampleTimingInfoArray(
            sampleBuffer, entryCount: 0, arrayToFill: nil, entriesNeededOut: &count
        ) == noErr else { return nil }
        var timing = [CMSampleTimingInfo](
            repeating: CMSampleTimingInfo(
                duration: .invalid, presentationTimeStamp: .invalid, decodeTimeStamp: .invalid
            ),
            count: count
        )
        let status = timing.withUnsafeMutableBufferPointer { buffer in
            CMSampleBufferGetSampleTimingInfoArray(
                sampleBuffer, entryCount: count, arrayToFill: buffer.baseAddress,
                entriesNeededOut: &count
            )
        }
        guard status == noErr else { return nil }
        for index in timing.indices {
            if timing[index].presentationTimeStamp.isValid {
                timing[index].presentationTimeStamp = CMTimeAdd(
                    timing[index].presentationTimeStamp, offset
                )
            }
            if timing[index].decodeTimeStamp.isValid {
                timing[index].decodeTimeStamp = CMTimeAdd(timing[index].decodeTimeStamp, offset)
            }
        }
        var copy: CMSampleBuffer?
        let copyStatus = timing.withUnsafeBufferPointer { buffer in
            CMSampleBufferCreateCopyWithNewTiming(
                allocator: kCFAllocatorDefault,
                sampleBuffer: sampleBuffer,
                sampleTimingEntryCount: count,
                sampleTimingArray: buffer.baseAddress,
                sampleBufferOut: &copy
            )
        }
        return copyStatus == noErr ? copy : nil
    }

    private static func makeAudioSample(
        bytes: Data, sampleCount: Int, sampleRate: UInt32, channels: UInt32,
        presentationTime: CMTime
    ) -> CMSampleBuffer? {
        guard let frameCount = Int(exactly: sampleCount / Int(channels)), frameCount > 0 else {
            return nil
        }
        var description = AudioStreamBasicDescription(
            mSampleRate: Double(sampleRate),
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: channels * UInt32(MemoryLayout<Float>.size),
            mFramesPerPacket: 1,
            mBytesPerFrame: channels * UInt32(MemoryLayout<Float>.size),
            mChannelsPerFrame: channels,
            mBitsPerChannel: UInt32(MemoryLayout<Float>.size * 8),
            mReserved: 0
        )
        var format: CMAudioFormatDescription?
        guard CMAudioFormatDescriptionCreate(
            allocator: kCFAllocatorDefault,
            asbd: &description,
            layoutSize: 0,
            layout: nil,
            magicCookieSize: 0,
            magicCookie: nil,
            extensions: nil,
            formatDescriptionOut: &format
        ) == noErr, let format else { return nil }
        var block: CMBlockBuffer?
        guard CMBlockBufferCreateWithMemoryBlock(
            allocator: kCFAllocatorDefault,
            memoryBlock: nil,
            blockLength: bytes.count,
            blockAllocator: kCFAllocatorDefault,
            customBlockSource: nil,
            offsetToData: 0,
            dataLength: bytes.count,
            flags: 0,
            blockBufferOut: &block
        ) == noErr, let block else { return nil }
        let copied = bytes.withUnsafeBytes { raw in
            CMBlockBufferReplaceDataBytes(
                with: raw.baseAddress!, blockBuffer: block, offsetIntoDestination: 0,
                dataLength: bytes.count
            )
        }
        guard copied == noErr else { return nil }
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: CMTimeScale(sampleRate)),
            presentationTimeStamp: presentationTime,
            decodeTimeStamp: .invalid
        )
        var sampleSize = Int(channels) * MemoryLayout<Float>.size
        var sampleBuffer: CMSampleBuffer?
        let created = CMSampleBufferCreateReady(
            allocator: kCFAllocatorDefault,
            dataBuffer: block,
            formatDescription: format,
            sampleCount: frameCount,
            sampleTimingEntryCount: 1,
            sampleTimingArray: &timing,
            sampleSizeEntryCount: 1,
            sampleSizeArray: &sampleSize,
            sampleBufferOut: &sampleBuffer
        )
        return created == noErr ? sampleBuffer : nil
    }
}

private enum RecordingError: Error {
    case cannotAddTrack
    case cannotStart
    case cannotCreateAudioFormat
    case cannotOpenOutput
    case invalidContentType
}

/// Stateful channel normalization and linear resampling for the retained microphone track.
/// Packet boundaries preserve both fractional phase and the previous sample, matching the
/// drift-safe normalization used by the Rust transcript path.
private struct MonoAudioNormalizer {
    private var index: Int = 0
    private var fraction = 0.0
    private var previous: Float?
    private var inputRate: UInt32?

    mutating func process(
        _ interleaved: [Float], sampleRate: UInt32, channels: UInt32
    ) -> [Float] {
        guard sampleRate > 0, channels > 0 else { return [] }
        let channelCount = Int(channels)
        let frameCount = interleaved.count / channelCount
        var mono = [Float]()
        mono.reserveCapacity(frameCount)
        for frame in 0..<frameCount {
            let start = frame * channelCount
            var sum: Float = 0
            for channel in 0..<channelCount {
                sum += interleaved[start + channel]
            }
            mono.append(sum / Float(channelCount))
        }
        guard !mono.isEmpty else { return [] }
        if inputRate != sampleRate {
            index = 0
            fraction = 0
            previous = nil
            inputRate = sampleRate
        }
        let step = Double(sampleRate) / 48_000
        var output = [Float]()
        while index < mono.count {
            let lower = sample(mono, at: index)
            guard let upper = upperSample(mono, at: index) else { break }
            output.append(lower + (upper - lower) * Float(fraction))
            fraction += step
            let advance = Int(fraction.rounded(.down))
            fraction -= Double(advance)
            index += advance
        }
        previous = mono.last
        index -= mono.count
        return output
    }

    private func sample(_ input: [Float], at index: Int) -> Float {
        guard index >= 0 else { return previous ?? input[0] }
        return input[index]
    }

    private func upperSample(_ input: [Float], at index: Int) -> Float? {
        if index == -1 { return input.first }
        let next = index + 1
        return next < input.count ? input[next] : nil
    }
}

private final class CaptureSession: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let config: BridgeConfig
    private let target: PickedTarget?
    private let queue = DispatchQueue(label: "dev.sotto.capture.system-audio", qos: .userInteractive)
    private let videoQueue = DispatchQueue(label: "dev.sotto.capture.frames", qos: .utility)
    private var stream: SCStream?
    private var sequence: UInt64 = 0
    private var lastFrameTimeNs: UInt64 = 0
    private var monoScratch = [Float]()
    private var recording: RecordingWriter?
    private let terminalLock = NSLock()
    private var reportedTerminal = false

    init(config: BridgeConfig, target: PickedTarget?) {
        self.config = config
        self.target = target
    }

    var usesScreenCapture: Bool { target != nil }

    func start() async throws {
        guard let target else {
            if let path = config.recordingPath {
                let recording = try RecordingWriter(
                    path: path, width: 2, height: 2, includesVideo: false
                )
                recording.onFailure = { [weak self] detail in
                    self?.reportTerminal(-8, detail: detail)
                }
                self.recording = recording
            }
            return
        }
        let streamConfig = SCStreamConfiguration()
        streamConfig.capturesAudio = true
        // Display capture is intentionally system-wide. Excluding this process is
        // useful for application/window filters, but on display-style picker filters
        // it can result in silent audio buffers on current macOS releases.
        streamConfig.excludesCurrentProcessAudio = target.audioScoped
        streamConfig.sampleRate = 48_000
        streamConfig.channelCount = 1
        // Capture frames at the display's real backing resolution, capped so a frame
        // always fits the preallocated pool buffers on the Rust side (MAX_FRAME_BYTES).
        // These were previously hardcoded to 2x2: ScreenCaptureKit requires a video
        // output even for audio-only capture, and 2x2 was the cheapest way to satisfy
        // that. Screen context is now a real timeline producer, so the placeholder has
        // to go — at 2x2 every captured frame was 16 bytes of nothing.
        let scale = CGFloat(target.filter.pointPixelScale)
        var frameWidth = max(1, Int(target.filter.contentRect.width * scale))
        var frameHeight = max(1, Int(target.filter.contentRect.height * scale))
        let maxPixels = (40 * 1024 * 1024) / 4
        if frameWidth * frameHeight > maxPixels {
            let shrink = (Double(maxPixels) / Double(frameWidth * frameHeight)).squareRoot()
            frameWidth = max(1, Int(Double(frameWidth) * shrink))
            frameHeight = max(1, Int(Double(frameHeight) * shrink))
        }
        frameWidth = max(2, frameWidth - (frameWidth % 2))
        frameHeight = max(2, frameHeight - (frameHeight % 2))
        streamConfig.width = frameWidth
        streamConfig.height = frameHeight
        streamConfig.minimumFrameInterval = CMTime(value: 1, timescale: 30)
        streamConfig.queueDepth = 8
        streamConfig.pixelFormat = kCVPixelFormatType_32BGRA

        if let path = config.recordingPath {
            let recording = try RecordingWriter(path: path, width: frameWidth, height: frameHeight)
            recording.onFailure = { [weak self] detail in
                self?.reportTerminal(-8, detail: detail)
            }
            self.recording = recording
        }

        let stream = SCStream(filter: target.filter, configuration: streamConfig, delegate: self)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: videoQueue)
        self.stream = stream
        try await stream.startCapture()
    }

    func stop() async {
        if let stream {
            try? await stream.stopCapture()
            self.stream = nil
        }
        if let recording {
            await recording.finish()
            self.recording = nil
        }
    }

    func appendMicrophone(
        _ samples: UnsafePointer<Float>, count: Int, sampleRate: UInt32,
        channels: UInt32, streamTimeNs: UInt64
    ) {
        if target == nil {
            recording?.appendMicrophoneOnly(
                samples, count: count, sampleRate: sampleRate,
                channels: channels, streamTimeNs: streamTimeNs
            )
        } else {
            recording?.appendMicrophone(
                samples, count: count, sampleRate: sampleRate,
                channels: channels, streamTimeNs: streamTimeNs
            )
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: any Error) {
        let nsError = error as NSError
        guard nsError.domain == SCStreamErrorDomain,
              let code = SCStreamError.Code(rawValue: nsError.code) else {
            reportTerminal(-3)
            return
        }
        switch code {
        case .userStopped:
            reportTerminal(-6)
        case .noCaptureSource, .systemStoppedStream:
            reportTerminal(-5)
        default:
            reportTerminal(-3)
        }
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        if outputType == .screen {
            // ScreenCaptureKit also emits idle, blank, suspended, started, and stopped
            // buffers. Only `.complete` denotes newly generated content; forwarding the
            // others can repeat a PTS and AVAssetWriterInput rejects non-monotonic samples.
            guard sampleBuffer.isValid, Self.isCompleteScreenFrame(sampleBuffer) else { return }
            recording?.appendVideo(sampleBuffer)
            deliverFrame(sampleBuffer)
            return
        }
        guard outputType == .audio else { return }
        guard sampleBuffer.isValid,
              let formatDescription = sampleBuffer.formatDescription else {
            reportTerminal(-7)
            return
        }
        let format = AVAudioFormat(cmAudioFormatDescription: formatDescription)
        guard format.commonFormat == .pcmFormatFloat32,
              format.channelCount > 0,
              format.sampleRate == 48_000 else {
            // The compact C ABI intentionally carries mono samples, not format metadata.
            // Rust therefore accepts only the rate requested from SCStream; treating any
            // other actual rate as 48 kHz would corrupt resampling and drift evidence.
            reportTerminal(-7)
            return
        }

        do {
            try sampleBuffer.withAudioBufferList { audioBufferList, _ in
            guard let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                bufferListNoCopy: audioBufferList.unsafePointer
            ), let channelData = buffer.floatChannelData else {
                throw AudioExtractionError.unreadableBuffer
            }

            let frameCount = Int(buffer.frameLength)
            let channelCount = Int(format.channelCount)
            guard frameCount > 0, channelCount > 0 else { return }
            let timestamp = sampleBuffer.presentationTimeStamp
            let timestampNs = timestamp.isValid
                ? UInt64(max(0, CMTimeGetSeconds(timestamp) * 1_000_000_000)) : 0

            if channelCount == 1 {
                recording?.appendSystemAudio(
                    channelData[0], count: frameCount, streamTimeNs: timestampNs
                )
                config.audio(config.context, channelData[0], frameCount, sequence, timestampNs)
            } else {
                if monoScratch.count != frameCount {
                    monoScratch = [Float](repeating: 0, count: frameCount)
                }
                for frame in 0..<frameCount {
                    var sum: Float = 0
                    for channel in 0..<channelCount {
                        if format.isInterleaved {
                            sum += channelData[0][frame * channelCount + channel]
                        } else {
                            sum += channelData[channel][frame]
                        }
                    }
                    monoScratch[frame] = sum / Float(channelCount)
                }
                monoScratch.withUnsafeBufferPointer { samples in
                    recording?.appendSystemAudio(
                        samples.baseAddress, count: samples.count, streamTimeNs: timestampNs
                    )
                    config.audio(
                        config.context, samples.baseAddress, samples.count, sequence, timestampNs
                    )
                }
            }
            sequence &+= 1
            }
        } catch {
            // A healthy packet cadence with unreadable buffers previously looked like
            // valid silence. Fail closed so Rust clears Running and surfaces the typed
            // stream failure instead of writing a misleading empty customer track.
            reportTerminal(-7)
        }
    }

    private static func isCompleteScreenFrame(_ sampleBuffer: CMSampleBuffer) -> Bool {
        guard
            let attachmentArray = CMSampleBufferGetSampleAttachmentsArray(
                sampleBuffer, createIfNecessary: false
            ) as? [[SCStreamFrameInfo: Any]],
            let attachments = attachmentArray.first,
            let rawStatus = attachments[.status] as? Int,
            let status = SCFrameStatus(rawValue: rawStatus)
        else { return false }
        return status == .complete
    }

    private func deliverFrame(_ sampleBuffer: CMSampleBuffer) {
        let hostNs = Self.hostTimeNanoseconds()
        guard hostNs &- lastFrameTimeNs >= 10_000_000_000,
              let image = sampleBuffer.imageBuffer else { return }
        lastFrameTimeNs = hostNs
        CVPixelBufferLockBaseAddress(image, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(image, .readOnly) }
        guard let base = CVPixelBufferGetBaseAddress(image) else { return }
        let width = CVPixelBufferGetWidth(image)
        let height = CVPixelBufferGetHeight(image)
        let stride = CVPixelBufferGetBytesPerRow(image)
        let length = CVPixelBufferGetDataSize(image)
        guard let width32 = UInt32(exactly: width),
              let height32 = UInt32(exactly: height),
              let stride32 = UInt32(exactly: stride) else { return }
        let timestamp = sampleBuffer.presentationTimeStamp
        let streamNs = timestamp.isValid ? UInt64(max(0, CMTimeGetSeconds(timestamp) * 1_000_000_000)) : 0
        config.frame(
            config.context,
            base.assumingMemoryBound(to: UInt8.self),
            length,
            width32,
            height32,
            stride32,
            UInt32(kCVPixelFormatType_32BGRA),
            streamNs,
            hostNs
        )
    }

    func reportTerminal(_ code: Int32, detail: String? = nil) {
        terminalLock.lock()
        defer { terminalLock.unlock() }
        guard !reportedTerminal else { return }
        reportedTerminal = true
        config.report(code, detail: detail)
    }

    private static func hostTimeNanoseconds() -> UInt64 {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        let ticks = mach_continuous_time()
        let denominator = UInt64(info.denom)
        let numerator = UInt64(info.numer)
        return (ticks / denominator) * numerator + ((ticks % denominator) * numerator) / denominator
    }
}

private enum AudioExtractionError: Error {
    case unreadableBuffer
}

private enum CaptureBridgeError: Error {
    case invalidConfig
}

private actor CaptureLifecycle {
    private enum Phase { case starting, running, stopping, failed, stopped }

    private let session: CaptureSession
    private let config: BridgeConfig
    private var phase = Phase.starting
    private var stopRequested = false

    init(session: CaptureSession, config: BridgeConfig) {
        self.session = session
        self.config = config
    }

    func start() async {
        do {
            try await session.start()
            if stopRequested {
                phase = .stopping
                await session.stop()
                phase = .stopped
                config.report(2)
            } else {
                phase = .running
                config.report(1)
            }
        } catch {
            await session.stop()
            let ns = error as NSError
            var detail = "\(String(reflecting: error)) | \(ns.domain) code=\(ns.code)"
            if let underlying = ns.userInfo[NSUnderlyingErrorKey] as? NSError {
                detail += " | underlying: \(underlying.domain) code=\(underlying.code)"
            }
            let deniedScreenPermission = session.usesScreenCapture
                && !CGPreflightScreenCaptureAccess()
            session.reportTerminal(deniedScreenPermission ? -4 : -2, detail: detail)
            if stopRequested {
                phase = .stopped
                config.report(2)
            } else {
                phase = .failed
            }
        }
    }

    func stop() async {
        switch phase {
        case .starting:
            stopRequested = true
        case .running:
            phase = .stopping
            await session.stop()
            phase = .stopped
            config.report(2)
        case .failed:
            phase = .stopped
            config.report(2)
        case .stopping, .stopped:
            break
        }
    }
}

private final class HandleBox: @unchecked Sendable {
    let lifecycle: CaptureLifecycle
    let session: CaptureSession
    init(session: CaptureSession, lifecycle: CaptureLifecycle) {
        self.session = session
        self.lifecycle = lifecycle
    }
}

// Config memory layout mirrors include/SottoCaptureBridge.h.
@_cdecl("sotto_capture_pick_target")
public func sottoCapturePickTarget(
    _ rawCallback: UnsafeRawPointer?, _ context: UnsafeMutableRawPointer?
) {
    guard let rawCallback else { return }
    let request = PickerRequest(
        callback: unsafeBitCast(rawCallback, to: TargetCallback.self),
        context: context
    )
    Task { @MainActor in
        let picker = TargetPicker(callback: request.callback, context: request.context)
        _ = Unmanaged.passRetained(picker)
        picker.present()
    }
}

@_cdecl("sotto_capture_release_target")
public func sottoCaptureReleaseTarget(_ target: UnsafeMutableRawPointer?) {
    guard let target else { return }
    Unmanaged<PickedTarget>.fromOpaque(target).release()
}

@_cdecl("sotto_capture_start_with_target")
public func sottoCaptureStartWithTarget(
    _ rawConfig: UnsafeRawPointer?, _ rawTarget: UnsafeMutableRawPointer?
) -> UnsafeMutableRawPointer? {
    guard let rawConfig, let rawTarget else { return nil }
    let words = rawConfig.bindMemory(to: UnsafeRawPointer?.self, capacity: 5)
    guard let audioRaw = words[0], let errorRaw = words[1], let frameRaw = words[2] else { return nil }
    let audio = unsafeBitCast(audioRaw, to: AudioCallback.self)
    let error = unsafeBitCast(errorRaw, to: ErrorCallback.self)
    let frame = unsafeBitCast(frameRaw, to: FrameCallback.self)
    let config = BridgeConfig(
        audio: audio,
        error: error,
        frame: frame,
        context: UnsafeMutableRawPointer(mutating: words[3]),
        recordingPath: words[4].map { String(cString: $0.assumingMemoryBound(to: CChar.self)) }
    )
    let target = Unmanaged<PickedTarget>.fromOpaque(rawTarget).takeUnretainedValue()
    let session = CaptureSession(config: config, target: target)
    let box = HandleBox(
        session: session,
        lifecycle: CaptureLifecycle(session: session, config: config)
    )
    let retained = Unmanaged.passRetained(box)
    Task { await box.lifecycle.start() }
    return retained.toOpaque()
}

/// Starts the CPAL-fed recording side without constructing ScreenCaptureKit state.
@_cdecl("sotto_capture_start_microphone_only")
public func sottoCaptureStartMicrophoneOnly(
    _ rawConfig: UnsafeRawPointer?
) -> UnsafeMutableRawPointer? {
    guard let rawConfig else { return nil }
    let words = rawConfig.bindMemory(to: UnsafeRawPointer?.self, capacity: 5)
    guard let audioRaw = words[0], let errorRaw = words[1], let frameRaw = words[2] else { return nil }
    let config = BridgeConfig(
        audio: unsafeBitCast(audioRaw, to: AudioCallback.self),
        error: unsafeBitCast(errorRaw, to: ErrorCallback.self),
        frame: unsafeBitCast(frameRaw, to: FrameCallback.self),
        context: UnsafeMutableRawPointer(mutating: words[3]),
        recordingPath: words[4].map { String(cString: $0.assumingMemoryBound(to: CChar.self)) }
    )
    let session = CaptureSession(config: config, target: nil)
    let box = HandleBox(
        session: session,
        lifecycle: CaptureLifecycle(session: session, config: config)
    )
    let retained = Unmanaged.passRetained(box)
    Task { await box.lifecycle.start() }
    return retained.toOpaque()
}

@_cdecl("sotto_capture_append_microphone")
public func sottoCaptureAppendMicrophone(
    _ handle: UnsafeMutableRawPointer?, _ samples: UnsafePointer<Float>?, _ count: Int,
    _ sampleRate: UInt32, _ channels: UInt32, _ streamTimeNs: UInt64
) {
    guard let handle, let samples else { return }
    let box = Unmanaged<HandleBox>.fromOpaque(handle).takeUnretainedValue()
    box.session.appendMicrophone(
        samples, count: count, sampleRate: sampleRate,
        channels: channels, streamTimeNs: streamTimeNs
    )
}

@_cdecl("sotto_capture_stop")
public func sottoCaptureStop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    let box = Unmanaged<HandleBox>.fromOpaque(handle).takeRetainedValue()
    Task { await box.lifecycle.stop() }
}

@_cdecl("sotto_recording_probe")
public func sottoRecordingProbe(
    _ rawPath: UnsafePointer<CChar>?, _ durationNs: UnsafeMutablePointer<UInt64>?,
    _ byteSize: UnsafeMutablePointer<UInt64>?, _ firstVideoNs: UnsafeMutablePointer<UInt64>?,
    _ seekVideoNs: UnsafeMutablePointer<UInt64>?
) -> Bool {
    guard let rawPath, let durationNs, let byteSize, let firstVideoNs, let seekVideoNs else {
        return false
    }
    let path = String(cString: rawPath)
    let url = URL(fileURLWithPath: path)
    let asset = AVURLAsset(url: url)
    let duration = asset.duration
    guard duration.isNumeric,
          let attributes = try? FileManager.default.attributesOfItem(atPath: path),
          let size = attributes[.size] as? NSNumber,
          !asset.tracks(withMediaType: .audio).isEmpty else { return false }
    let seconds = CMTimeGetSeconds(duration)
    guard seconds.isFinite, seconds >= 0 else { return false }
    durationNs.pointee = UInt64(seconds * 1_000_000_000)
    byteSize.pointee = size.uint64Value
    guard let track = asset.tracks(withMediaType: .video).first else {
        firstVideoNs.pointee = UInt64.max
        seekVideoNs.pointee = UInt64.max
        return true
    }
    guard let firstReader = try? AVAssetReader(asset: asset) else { return false }
    let firstOutput = AVAssetReaderTrackOutput(
        track: track,
        outputSettings: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
    )
    guard firstReader.canAdd(firstOutput) else { return false }
    firstReader.add(firstOutput)
    guard firstReader.startReading(), let firstFrame = firstOutput.copyNextSampleBuffer() else {
        return false
    }
    let frameSeconds = CMTimeGetSeconds(firstFrame.presentationTimeStamp)
    guard frameSeconds.isFinite, frameSeconds >= 0 else { return false }
    let requestedSeekSeconds = max(0, seconds - 1)
    let seekStart = CMTime(seconds: requestedSeekSeconds, preferredTimescale: 600)
    guard let seekReader = try? AVAssetReader(asset: asset) else { return false }
    seekReader.timeRange = CMTimeRange(
        start: seekStart,
        duration: CMTime(seconds: max(0.001, seconds - requestedSeekSeconds), preferredTimescale: 600)
    )
    let seekOutput = AVAssetReaderTrackOutput(
        track: track,
        outputSettings: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA
        ]
    )
    guard seekReader.canAdd(seekOutput) else { return false }
    seekReader.add(seekOutput)
    guard seekReader.startReading(), let seekFrame = seekOutput.copyNextSampleBuffer() else {
        return false
    }
    let seekSeconds = CMTimeGetSeconds(seekFrame.presentationTimeStamp)
    // AVAssetReader may begin at the preceding sync sample even when its timeRange starts later.
    // Sparse screen recordings can therefore return a valid frame whose PTS predates seekStart.
    guard isReadableRecordingTimestamp(seekSeconds) else { return false }
    firstVideoNs.pointee = UInt64(frameSeconds * 1_000_000_000)
    seekVideoNs.pointee = UInt64(seekSeconds * 1_000_000_000)
    return true
}

@_cdecl("sotto_recording_committed_duration")
public func sottoRecordingCommittedDuration(
    _ rawPath: UnsafePointer<CChar>?, _ durationNs: UnsafeMutablePointer<UInt64>?,
    _ byteSize: UnsafeMutablePointer<UInt64>?
) -> Bool {
    guard let rawPath, let durationNs, let byteSize else { return false }
    let path = String(cString: rawPath)
    let duration = AVURLAsset(url: URL(fileURLWithPath: path)).duration
    guard duration.isNumeric,
          let attributes = try? FileManager.default.attributesOfItem(atPath: path),
          let size = attributes[.size] as? NSNumber else { return false }
    let seconds = CMTimeGetSeconds(duration)
    guard isReadableRecordingTimestamp(seconds) else { return false }
    durationNs.pointee = UInt64(seconds * 1_000_000_000)
    byteSize.pointee = size.uint64Value
    return true
}

func isReadableRecordingTimestamp(_ seconds: Double) -> Bool {
    seconds.isFinite && seconds >= 0
}

/// Exercises the exact timestamp gate used immediately before every writer input append.
/// Values are nanoseconds; outputs contain the timestamp that the writer would receive.
@_cdecl("sotto_recording_append_pts_probe")
public func sottoRecordingAppendPTSProbe(
    _ input: UnsafePointer<Int64>?, _ output: UnsafeMutablePointer<Int64>?, _ count: Int
) -> Bool {
    guard let input, let output, count >= 0 else { return false }
    var presentationTime = StrictlyIncreasingPresentationTime()
    for index in 0..<count {
        let candidate = CMTime(value: input[index], timescale: 1_000_000_000)
        let appended = presentationTime.append(candidate) { adjusted in
            let nanoseconds = CMTimeConvertScale(
                adjusted, timescale: 1_000_000_000, method: .default
            )
            output[index] = nanoseconds.value
            return true
        }
        guard appended == true else { return false }
    }
    return true
}

/// Persists whether this app identity has ever asked the user for Screen & System Audio
/// Recording access. `CGPreflightScreenCaptureAccess()` alone cannot distinguish "never asked"
/// from "asked and refused" — both read as `false` — and there is no direct macOS query for
/// TCC's not-determined state for this permission. The asymmetry we can observe is behavioral:
/// macOS asks once. Requesting again after a real denial is a silent no-op (no UI, status
/// unchanged); requesting for the first time puts the system prompt on screen. So the bridge
/// remembers, across relaunches (a grant only takes effect for a newly launched process, so a
/// relaunch is already required either way), whether `sotto_capture_request_permission` has ever
/// been called. Preflight false plus "never asked" is not-determined; preflight false plus
/// "asked before" is a stated denial.
private let screenCaptureRequestIssuedKey = "com.sotto.screenCapture.requestIssued"

@_cdecl("sotto_capture_permission_status")
public func sottoCapturePermissionStatus() -> Int32 {
    if CGPreflightScreenCaptureAccess() {
        return 1
    }
    let everRequested = UserDefaults.standard.bool(forKey: screenCaptureRequestIssuedKey)
    return everRequested ? 2 : 3
}

@_cdecl("sotto_capture_request_permission")
public func sottoCaptureRequestPermission() -> Bool {
    UserDefaults.standard.set(true, forKey: screenCaptureRequestIssuedKey)
    return CGRequestScreenCaptureAccess()
}

@_cdecl("sotto_capture_pump_main_loop")
public func sottoCapturePumpMainLoop(_ seconds: Double) {
    // The picker is presented by the system on behalf of this process, which needs a
    // window-server connection; touching NSApplication.shared establishes one. Running
    // the main run loop then drains the main queue, where the picker task was scheduled.
    // The GPUI app already runs an AppKit event loop and never calls this.
    let application = NSApplication.shared
    if application.activationPolicy() == .prohibited {
        application.setActivationPolicy(.accessory)
    }
    RunLoop.main.run(until: Date(timeIntervalSinceNow: seconds))
}

@_cdecl("sotto_capture_open_permission_settings")
public func sottoCaptureOpenPermissionSettings() -> Bool {
    guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") else {
        return false
    }
    return NSWorkspace.shared.open(url)
}
