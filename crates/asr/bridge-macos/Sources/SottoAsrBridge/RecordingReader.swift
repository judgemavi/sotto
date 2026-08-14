@preconcurrency import AVFoundation
@preconcurrency import CoreMedia
@preconcurrency import Foundation

public typealias StereoCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<Float>?, UnsafePointer<Float>?, Int, UInt64
) -> Void

private let outputSampleRate = 16_000.0

@_cdecl("sotto_asr_recording_duration")
public func sottoAsrRecordingDuration(
    _ rawPath: UnsafePointer<CChar>?, _ durationNs: UnsafeMutablePointer<UInt64>?,
    _ errorBuffer: UnsafeMutablePointer<CChar>?, _ errorCapacity: Int
) -> Bool {
    guard let rawPath, let durationNs else {
        writeError("recording duration received invalid pointers", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let asset = AVURLAsset(url: URL(fileURLWithPath: String(cString: rawPath)))
    let duration = asset.duration
    guard duration.isNumeric else {
        writeError("recording has no readable committed duration", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let seconds = CMTimeGetSeconds(duration)
    guard seconds.isFinite, seconds >= 0 else {
        writeError("recording committed duration is invalid", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    durationNs.pointee = UInt64(seconds * 1_000_000_000)
    return true
}

@_cdecl("sotto_asr_read_stereo")
public func sottoAsrReadStereo(
    _ rawPath: UnsafePointer<CChar>?, _ startNs: UInt64, _ endNs: UInt64,
    _ callback: StereoCallback?, _ context: UnsafeMutableRawPointer?,
    _ errorBuffer: UnsafeMutablePointer<CChar>?, _ errorCapacity: Int
) -> Bool {
    guard let rawPath, let callback, endNs > startNs else {
        writeError("recording read received an invalid range or callback", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let asset = AVURLAsset(url: URL(fileURLWithPath: String(cString: rawPath)))
    let audioTracks = asset.tracks(withMediaType: .audio)
    guard let track = audioTracks.count > 1 ? audioTracks.last : audioTracks.first else {
        writeError("recording has no committed audio track", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    guard let rawFormat = track.formatDescriptions.first else {
        writeError("recording audio has no format description", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let sourceFormat = rawFormat as! CMFormatDescription
    guard isStereo(sourceFormat) else {
        writeError(
            "recording audio must be stereo: left=meeting audio, right=microphone",
            to: errorBuffer,
            capacity: errorCapacity
        )
        return false
    }
    let reader: AVAssetReader
    do {
        reader = try AVAssetReader(asset: asset)
    } catch {
        writeError("could not open committed recording audio: \(error)", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    let start = CMTime(value: CMTimeValue(startNs), timescale: 1_000_000_000)
    let end = CMTime(value: CMTimeValue(endNs), timescale: 1_000_000_000)
    reader.timeRange = CMTimeRange(start: start, duration: CMTimeSubtract(end, start))
    let output = AVAssetReaderTrackOutput(
        track: track,
        outputSettings: [
            AVFormatIDKey: kAudioFormatLinearPCM,
            AVSampleRateKey: outputSampleRate,
            AVNumberOfChannelsKey: 2,
            AVLinearPCMBitDepthKey: 32,
            AVLinearPCMIsFloatKey: true,
            AVLinearPCMIsBigEndianKey: false,
            AVLinearPCMIsNonInterleaved: false,
        ]
    )
    guard reader.canAdd(output) else {
        writeError("recording reader rejected stereo PCM output", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    reader.add(output)
    guard reader.startReading() else {
        writeError(reader.error?.localizedDescription ?? "recording reader could not start", to: errorBuffer, capacity: errorCapacity)
        return false
    }

    while let sample = output.copyNextSampleBuffer() {
        guard deliver(sample, callback: callback, context: context) else {
            reader.cancelReading()
            writeError("recording reader could not decode stereo PCM", to: errorBuffer, capacity: errorCapacity)
            return false
        }
    }
    guard reader.status == .completed else {
        writeError(reader.error?.localizedDescription ?? "recording reader stopped before the committed end", to: errorBuffer, capacity: errorCapacity)
        return false
    }
    return true
}

private func isStereo(_ description: CMFormatDescription) -> Bool {
    guard let stream = CMAudioFormatDescriptionGetStreamBasicDescription(description) else {
        return false
    }
    return stream.pointee.mChannelsPerFrame == 2
}

private func deliver(
    _ sampleBuffer: CMSampleBuffer, callback: StereoCallback,
    context: UnsafeMutableRawPointer?
) -> Bool {
    guard sampleBuffer.isValid,
          let description = sampleBuffer.formatDescription else {
        return false
    }
    let format = AVAudioFormat(cmAudioFormatDescription: description)
    var delivered = false
    do {
        try sampleBuffer.withAudioBufferList { list, _ in
            guard let buffer = AVAudioPCMBuffer(
                pcmFormat: format, bufferListNoCopy: list.unsafePointer
            ), let channels = buffer.floatChannelData else { return }
            let frameCount = Int(buffer.frameLength)
            guard frameCount > 0, format.channelCount == 2 else { return }
            let timestamp = sampleBuffer.presentationTimeStamp
            guard timestamp.isNumeric else { return }
            let timestampNs = UInt64(max(0, CMTimeGetSeconds(timestamp) * 1_000_000_000))
            if format.isInterleaved {
                var left = [Float](repeating: 0, count: frameCount)
                var right = [Float](repeating: 0, count: frameCount)
                for frame in 0..<frameCount {
                    left[frame] = channels[0][frame * 2]
                    right[frame] = channels[0][frame * 2 + 1]
                }
                left.withUnsafeBufferPointer { leftBuffer in
                    right.withUnsafeBufferPointer { rightBuffer in
                        callback(
                            context, leftBuffer.baseAddress, rightBuffer.baseAddress,
                            frameCount, timestampNs
                        )
                    }
                }
            } else {
                callback(context, channels[0], channels[1], frameCount, timestampNs)
            }
            delivered = true
        }
    } catch {
        return false
    }
    return delivered
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
