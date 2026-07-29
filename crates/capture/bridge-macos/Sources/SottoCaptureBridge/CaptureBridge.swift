@preconcurrency import AVFoundation
@preconcurrency import AppKit
@preconcurrency import CoreGraphics
@preconcurrency import CoreMedia
@preconcurrency import CoreVideo
@preconcurrency import Darwin
@preconcurrency import Foundation
@preconcurrency import ScreenCaptureKit

private typealias AudioCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<Float>?, Int, UInt64, UInt64
) -> Void
private typealias ErrorCallback = @convention(c) (UnsafeMutableRawPointer?, Int32) -> Void
private typealias FrameCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafePointer<UInt8>?, Int, UInt32, UInt32, UInt32, UInt32, UInt64, UInt64
) -> Void

private struct BridgeConfig: @unchecked Sendable {
    let audio: AudioCallback
    let error: ErrorCallback
    let frame: FrameCallback
    let context: UnsafeMutableRawPointer?
}

private final class CaptureSession: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let config: BridgeConfig
    private let queue = DispatchQueue(label: "dev.sotto.capture.system-audio", qos: .userInteractive)
    private let videoQueue = DispatchQueue(label: "dev.sotto.capture.frames", qos: .utility)
    private var stream: SCStream?
    private var sequence: UInt64 = 0
    private var lastFrameTimeNs: UInt64 = 0

    init(config: BridgeConfig) {
        self.config = config
    }

    func start() async throws {
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
        guard let display = content.displays.first else {
            throw CaptureBridgeError.noDisplay
        }

        let filter = SCContentFilter(display: display, excludingApplications: [], exceptingWindows: [])
        let streamConfig = SCStreamConfiguration()
        streamConfig.capturesAudio = true
        streamConfig.excludesCurrentProcessAudio = true
        streamConfig.sampleRate = 48_000
        streamConfig.channelCount = 1
        streamConfig.width = 2
        streamConfig.height = 2
        streamConfig.minimumFrameInterval = CMTime(seconds: 10, preferredTimescale: 600)
        streamConfig.queueDepth = 2
        streamConfig.pixelFormat = kCVPixelFormatType_32BGRA

        let stream = SCStream(filter: filter, configuration: streamConfig, delegate: self)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: videoQueue)
        self.stream = stream
        try await stream.startCapture()
    }

    func stop() async {
        guard let stream else { return }
        try? await stream.stopCapture()
        self.stream = nil
    }

    func stream(_ stream: SCStream, didStopWithError error: any Error) {
        config.error(config.context, -3)
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard sampleBuffer.isValid else { return }
        if outputType == .screen {
            deliverFrame(sampleBuffer)
            return
        }
        guard outputType == .audio,
              let block = sampleBuffer.dataBuffer else { return }

        var length = 0
        var pointer: UnsafeMutablePointer<Int8>?
        guard CMBlockBufferGetDataPointer(
            block,
            atOffset: 0,
            lengthAtOffsetOut: nil,
            totalLengthOut: &length,
            dataPointerOut: &pointer
        ) == kCMBlockBufferNoErr, let pointer else { return }

        let count = length / MemoryLayout<Float>.stride
        let timestamp = sampleBuffer.presentationTimeStamp
        let timestampNs = timestamp.isValid ? UInt64(max(0, CMTimeGetSeconds(timestamp) * 1_000_000_000)) : 0
        pointer.withMemoryRebound(to: Float.self, capacity: count) { samples in
            config.audio(config.context, samples, count, sequence, timestampNs)
        }
        sequence &+= 1
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

    private static func hostTimeNanoseconds() -> UInt64 {
        var info = mach_timebase_info_data_t()
        mach_timebase_info(&info)
        let ticks = mach_continuous_time()
        let denominator = UInt64(info.denom)
        let numerator = UInt64(info.numer)
        return (ticks / denominator) * numerator + ((ticks % denominator) * numerator) / denominator
    }
}

private enum CaptureBridgeError: Error {
    case invalidConfig
    case noDisplay
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
                config.error(config.context, 2)
            } else {
                phase = .running
                config.error(config.context, 1)
            }
        } catch {
            config.error(config.context, CGPreflightScreenCaptureAccess() ? -2 : -4)
            if stopRequested {
                phase = .stopped
                config.error(config.context, 2)
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
            config.error(config.context, 2)
        case .failed:
            phase = .stopped
            config.error(config.context, 2)
        case .stopping, .stopped:
            break
        }
    }
}

private final class HandleBox: @unchecked Sendable {
    let lifecycle: CaptureLifecycle
    init(_ lifecycle: CaptureLifecycle) { self.lifecycle = lifecycle }
}

// Config memory layout mirrors include/SottoCaptureBridge.h.
@_cdecl("sotto_capture_start")
public func sottoCaptureStart(_ rawConfig: UnsafeRawPointer?) -> UnsafeMutableRawPointer? {
    guard let rawConfig else { return nil }
    let words = rawConfig.bindMemory(to: UnsafeRawPointer?.self, capacity: 4)
    guard let audioRaw = words[0], let errorRaw = words[1], let frameRaw = words[2] else { return nil }
    let audio = unsafeBitCast(audioRaw, to: AudioCallback.self)
    let error = unsafeBitCast(errorRaw, to: ErrorCallback.self)
    let frame = unsafeBitCast(frameRaw, to: FrameCallback.self)
    let config = BridgeConfig(
        audio: audio,
        error: error,
        frame: frame,
        context: UnsafeMutableRawPointer(mutating: words[3])
    )
    let session = CaptureSession(config: config)
    let box = HandleBox(CaptureLifecycle(session: session, config: config))
    let retained = Unmanaged.passRetained(box)
    Task { await box.lifecycle.start() }
    return retained.toOpaque()
}

@_cdecl("sotto_capture_stop")
public func sottoCaptureStop(_ handle: UnsafeMutableRawPointer?) {
    guard let handle else { return }
    let box = Unmanaged<HandleBox>.fromOpaque(handle).takeRetainedValue()
    Task { await box.lifecycle.stop() }
}

@_cdecl("sotto_capture_permission_status")
public func sottoCapturePermissionStatus() -> Int32 {
    CGPreflightScreenCaptureAccess() ? 1 : 2
}

@_cdecl("sotto_capture_request_permission")
public func sottoCaptureRequestPermission() -> Bool {
    CGRequestScreenCaptureAccess()
}

@_cdecl("sotto_capture_open_permission_settings")
public func sottoCaptureOpenPermissionSettings() -> Bool {
    guard let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture") else {
        return false
    }
    return NSWorkspace.shared.open(url)
}
