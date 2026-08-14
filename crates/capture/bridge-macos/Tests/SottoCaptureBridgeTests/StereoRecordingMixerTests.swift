import AVFoundation
import CoreMedia
import Foundation
import Testing
import UniformTypeIdentifiers
@testable import SottoCaptureBridge

@Test func microphoneOnlyRecordingUsesChannelZeroAndExplicitSilentChannelOne() {
    let stereo = microphoneOnlyStereo([0.25, -0.5])

    #expect(stereo == [0.25, 0, -0.5, 0])
}

@Test func recordingProbeAcceptsPrecedingSyncSampleTimestamp() {
    let requestedSeekSeconds = 9.0
    let precedingSyncSampleSeconds = 2.0
    #expect(precedingSyncSampleSeconds < requestedSeekSeconds)
    #expect(isReadableRecordingTimestamp(precedingSyncSampleSeconds))
    #expect(isReadableRecordingTimestamp(0.0))
    #expect(!isReadableRecordingTimestamp(-0.001))
    #expect(!isReadableRecordingTimestamp(.infinity))
    #expect(!isReadableRecordingTimestamp(.nan))
}

@Test func stereoMixerPreservesMeetingAndMicrophoneChannelsAcrossPackets() {
    var mixer = StereoRecordingMixer()
    var output = [(startFrame: UInt64, frameCount: Int, samples: [Float])]()

    for packet in 0..<6 {
        let timestamp = UInt64(packet) * 100_000_000
        output += mixer.append(
            [Float](repeating: 0.25, count: 4_800),
            channel: .meeting,
            startTimeNs: timestamp
        )
        output += mixer.append(
            [Float](repeating: -0.5, count: 4_800),
            channel: .microphone,
            startTimeNs: timestamp
        )
    }

    #expect(output.count == 1)
    #expect(output[0].startFrame == 0)
    #expect(output[0].frameCount == 4_800)
    #expect(output[0].samples.count == 9_600)
    #expect(stride(from: 0, to: output[0].samples.count, by: 2).allSatisfy {
        output[0].samples[$0] == 0.25
    })
    #expect(stride(from: 1, to: output[0].samples.count, by: 2).allSatisfy {
        output[0].samples[$0] == -0.5
    })
}

@Test func stereoMixerFlushesPartialTailAndFillsMissingChannelWithSilence() {
    var mixer = StereoRecordingMixer()
    let samples = [Float](repeating: 0.75, count: 1_200)

    #expect(mixer.append(samples, channel: .meeting, startTimeNs: 0).isEmpty)
    let output = mixer.finish()

    #expect(output.count == 1)
    #expect(output[0].frameCount == 1_200)
    #expect(stride(from: 0, to: output[0].samples.count, by: 2).allSatisfy {
        output[0].samples[$0] == 0.75
    })
    #expect(stride(from: 1, to: output[0].samples.count, by: 2).allSatisfy {
        output[0].samples[$0] == 0
    })
}

@Test func microphoneStartupLeadPreservesAmplitudeOnBothChannels() {
    var mixer = StereoRecordingMixer()
    var output = [(startFrame: UInt64, frameCount: Int, samples: [Float])]()
    let microphone = [Float](repeating: 0.5, count: 4_800)
    let meeting = [Float](repeating: 0.25, count: 4_800)

    // CPAL starts before the asynchronous ScreenCaptureKit stream. Each source currently gets an
    // independent zero origin, so once the microphone exceeds the reorder window it permanently
    // commits the same logical chunks before the later meeting callbacks can fill them.
    for packet in 0..<6 {
        output += mixer.append(
            microphone,
            channel: .microphone,
            startTimeNs: UInt64(packet) * 100_000_000
        )
    }
    for packet in 0..<20 {
        output += mixer.append(
            microphone,
            channel: .microphone,
            startTimeNs: UInt64(packet + 6) * 100_000_000
        )
        output += mixer.append(
            meeting,
            channel: .meeting,
            startTimeNs: UInt64(packet) * 100_000_000
        )
    }
    output += mixer.finish()

    var mixedMeeting = AudioLevelAccumulator()
    var mixedMicrophone = AudioLevelAccumulator()
    for chunk in output {
        mixedMeeting.appendInterleavedStereo(chunk.samples, channel: .meeting)
        mixedMicrophone.appendInterleavedStereo(chunk.samples, channel: .microphone)
    }

    #expect(mixedMeeting.measurement.peak >= 0.25)
    #expect(mixedMeeting.measurement.rms >= 0.20)
    #expect(mixedMicrophone.measurement.peak >= 0.5)
    #expect(mixedMicrophone.measurement.rms >= 0.40)
}

@Test func cleanStopProducesConventionalMp4WithAudibleStereoChannels() async throws {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("sotto-playback-finalization-\(UUID().uuidString).mp4")
    defer { try? FileManager.default.removeItem(at: url) }

    let fixture = try SegmentedStereoFixture(url: url)
    try fixture.append(packetCount: 22)
    await fixture.finish()
    #expect(topLevelAtomTypes(at: url).contains("moof"))

    try await finalizeRecordingForPlayback(at: url)

    let atoms = topLevelAtomTypes(at: url)
    #expect(atoms.contains("moov"))
    #expect(atoms.contains("mdat"))
    #expect(!atoms.contains("moof"))
    let playback = try decodedStereoLevels(at: url, trackIndex: 0)
    #expect(playback.meeting.rms > 0.10)
    #expect(abs(playback.meeting.rms - playback.microphone.rms) < 0.01)
    let isolated = try decodedStereoLevels(at: url, trackIndex: 1)
    #expect(isolated.meeting.peak > 0.15)
    #expect(isolated.meeting.rms > 0.10)
    #expect(isolated.microphone.peak > 0.30)
    #expect(isolated.microphone.rms > 0.20)

    var durationNs = UInt64(0)
    var byteSize = UInt64(0)
    var firstVideoNs = UInt64(0)
    var seekVideoNs = UInt64(0)
    let probeSucceeded = url.path.withCString { path in
        sottoRecordingProbe(
            path, &durationNs, &byteSize, &firstVideoNs, &seekVideoNs
        )
    }
    #expect(probeSucceeded)
    #expect(durationNs > 0)
    #expect(byteSize > 0)
    #expect(firstVideoNs == UInt64.max)
    #expect(seekVideoNs == UInt64.max)
}

private final class SegmentedStereoFixture: NSObject, AVAssetWriterDelegate, @unchecked Sendable {
    private let writer: AVAssetWriter
    private let input: AVAssetWriterInput
    private let handle: FileHandle

    init(url: URL) throws {
        guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
            throw PlaybackFixtureError.cannotCreateFile
        }
        handle = try FileHandle(forWritingTo: url)
        writer = AVAssetWriter(contentType: UTType.mpeg4Movie)
        input = AVAssetWriterInput(
            mediaType: .audio,
            outputSettings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: 48_000,
                AVNumberOfChannelsKey: 2,
                AVEncoderBitRateKey: 192_000,
                AVChannelLayoutKey: fixtureStereoLayoutData(),
            ],
            sourceFormatHint: try fixtureStereoFormat()
        )
        super.init()
        writer.outputFileTypeProfile = .mpeg4AppleHLS
        writer.preferredOutputSegmentInterval = CMTime(seconds: 1, preferredTimescale: 10)
        writer.initialSegmentStartTime = .zero
        writer.delegate = self
        guard writer.canAdd(input) else { throw PlaybackFixtureError.cannotAddInput }
        writer.add(input)
        guard writer.startWriting() else { throw writer.error ?? PlaybackFixtureError.cannotStart }
        writer.startSession(atSourceTime: .zero)
    }

    func append(packetCount: Int) throws {
        for packet in 0..<packetCount {
            let deadline = Date(timeIntervalSinceNow: 5)
            while !input.isReadyForMoreMediaData, Date() < deadline {
                Thread.sleep(forTimeInterval: 0.001)
            }
            guard input.isReadyForMoreMediaData,
                  input.append(try fixtureStereoSample(packet: packet)) else {
                throw writer.error ?? PlaybackFixtureError.cannotAppend
            }
        }
    }

    func finish() async {
        input.markAsFinished()
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
        try? handle.write(contentsOf: segmentData)
        try? handle.synchronize()
    }
}

private func fixtureStereoFormat() throws -> CMAudioFormatDescription {
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
    guard status == noErr, let format else { throw PlaybackFixtureError.cannotCreateFormat }
    return format
}

private func fixtureStereoLayoutData() -> Data {
    var layout = AudioChannelLayout()
    layout.mChannelLayoutTag = kAudioChannelLayoutTag_Stereo
    return Data(bytes: &layout, count: MemoryLayout<AudioChannelLayout>.size)
}

private func fixtureStereoSample(packet: Int) throws -> CMSampleBuffer {
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
    ) == noErr, let block else { throw PlaybackFixtureError.cannotCreateBuffer }
    let copied = data.withUnsafeBytes { bytes in
        CMBlockBufferReplaceDataBytes(
            with: bytes.baseAddress!, blockBuffer: block,
            offsetIntoDestination: 0, dataLength: data.count
        )
    }
    guard copied == noErr else { throw PlaybackFixtureError.cannotCreateBuffer }
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
        formatDescription: try fixtureStereoFormat(),
        sampleCount: frames,
        sampleTimingEntryCount: 1,
        sampleTimingArray: &timing,
        sampleSizeEntryCount: 1,
        sampleSizeArray: &sampleSize,
        sampleBufferOut: &sample
    )
    guard created == noErr, let sample else { throw PlaybackFixtureError.cannotCreateBuffer }
    return sample
}

private func decodedStereoLevels(
    at url: URL, trackIndex: Int
) throws -> (meeting: AudioLevelMeasurement, microphone: AudioLevelMeasurement) {
    let asset = AVURLAsset(url: url)
    let tracks = asset.tracks(withMediaType: .audio)
    guard tracks.indices.contains(trackIndex) else {
        throw PlaybackFixtureError.missingAudioTrack
    }
    let track = tracks[trackIndex]
    let reader = try AVAssetReader(asset: asset)
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
    guard reader.canAdd(output) else { throw PlaybackFixtureError.cannotReadAudio }
    reader.add(output)
    guard reader.startReading() else { throw reader.error ?? PlaybackFixtureError.cannotReadAudio }
    var meeting = AudioLevelAccumulator()
    var microphone = AudioLevelAccumulator()
    while let sample = output.copyNextSampleBuffer() {
        guard let block = CMSampleBufferGetDataBuffer(sample) else {
            throw PlaybackFixtureError.cannotReadAudio
        }
        let length = CMBlockBufferGetDataLength(block)
        var bytes = [UInt8](repeating: 0, count: length)
        guard CMBlockBufferCopyDataBytes(
            block, atOffset: 0, dataLength: length, destination: &bytes
        ) == noErr else { throw PlaybackFixtureError.cannotReadAudio }
        bytes.withUnsafeBytes { raw in
            let samples = Array(raw.bindMemory(to: Float.self))
            meeting.appendInterleavedStereo(samples, channel: .meeting)
            microphone.appendInterleavedStereo(samples, channel: .microphone)
        }
    }
    guard reader.status == .completed else {
        throw reader.error ?? PlaybackFixtureError.cannotReadAudio
    }
    return (meeting.measurement, microphone.measurement)
}

private func topLevelAtomTypes(at url: URL) -> [String] {
    guard let data = try? Data(contentsOf: url) else { return [] }
    var types = [String]()
    var offset = 0
    while offset + 8 <= data.count {
        let size32 = data[offset..<(offset + 4)].reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        let typeData = data[(offset + 4)..<(offset + 8)]
        if let type = String(data: typeData, encoding: .ascii) { types.append(type) }
        let size: UInt64
        if size32 == 1, offset + 16 <= data.count {
            size = data[(offset + 8)..<(offset + 16)].reduce(UInt64(0)) {
                ($0 << 8) | UInt64($1)
            }
        } else if size32 == 0 {
            size = UInt64(data.count - offset)
        } else {
            size = UInt64(size32)
        }
        guard size >= 8, let next = Int(exactly: size), offset + next <= data.count else { break }
        offset += next
    }
    return types
}

private enum PlaybackFixtureError: Error {
    case cannotCreateFile
    case cannotAddInput
    case cannotStart
    case cannotAppend
    case cannotCreateFormat
    case cannotCreateBuffer
    case missingAudioTrack
    case cannotReadAudio
}
