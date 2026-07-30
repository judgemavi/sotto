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
private typealias TargetCallback = @convention(c) (
    UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, UnsafePointer<CChar>?,
    UnsafePointer<CChar>?, UnsafePointer<CChar>?, Int32, Bool
) -> Void

private struct BridgeConfig: @unchecked Sendable {
    let audio: AudioCallback
    let error: ErrorCallback
    let frame: FrameCallback
    let context: UnsafeMutableRawPointer?
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

private final class CaptureSession: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    private let config: BridgeConfig
    private let target: PickedTarget
    private let queue = DispatchQueue(label: "dev.sotto.capture.system-audio", qos: .userInteractive)
    private let videoQueue = DispatchQueue(label: "dev.sotto.capture.frames", qos: .utility)
    private var stream: SCStream?
    private var sequence: UInt64 = 0
    private var lastFrameTimeNs: UInt64 = 0
    private var monoScratch = [Float]()
    private let terminalLock = NSLock()
    private var reportedTerminal = false

    init(config: BridgeConfig, target: PickedTarget) {
        self.config = config
        self.target = target
    }

    func start() async throws {
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
        streamConfig.width = frameWidth
        streamConfig.height = frameHeight
        streamConfig.minimumFrameInterval = CMTime(seconds: 10, preferredTimescale: 600)
        streamConfig.queueDepth = 2
        streamConfig.pixelFormat = kCVPixelFormatType_32BGRA

        let stream = SCStream(filter: target.filter, configuration: streamConfig, delegate: self)
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
        guard sampleBuffer.isValid else { return }
        if outputType == .screen {
            deliverFrame(sampleBuffer)
            return
        }
        guard outputType == .audio,
              let description = sampleBuffer.formatDescription?.audioStreamBasicDescription,
              let format = AVAudioFormat(
                standardFormatWithSampleRate: description.mSampleRate,
                channels: description.mChannelsPerFrame
              ) else { return }

        try? sampleBuffer.withAudioBufferList { audioBufferList, _ in
            guard let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                bufferListNoCopy: audioBufferList.unsafePointer
            ), let channelData = buffer.floatChannelData else { return }

            let frameCount = Int(buffer.frameLength)
            let channelCount = Int(format.channelCount)
            guard frameCount > 0, channelCount > 0 else { return }
            let timestamp = sampleBuffer.presentationTimeStamp
            let timestampNs = timestamp.isValid
                ? UInt64(max(0, CMTimeGetSeconds(timestamp) * 1_000_000_000)) : 0

            if channelCount == 1 {
                config.audio(config.context, channelData[0], frameCount, sequence, timestampNs)
            } else {
                if monoScratch.count != frameCount {
                    monoScratch = [Float](repeating: 0, count: frameCount)
                }
                for frame in 0..<frameCount {
                    var sum: Float = 0
                    for channel in 0..<channelCount {
                        sum += channelData[channel][frame]
                    }
                    monoScratch[frame] = sum / Float(channelCount)
                }
                monoScratch.withUnsafeBufferPointer { samples in
                    config.audio(
                        config.context, samples.baseAddress, samples.count, sequence, timestampNs
                    )
                }
            }
            sequence &+= 1
        }
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

    private func reportTerminal(_ code: Int32) {
        terminalLock.lock()
        defer { terminalLock.unlock() }
        guard !reportedTerminal else { return }
        reportedTerminal = true
        config.error(config.context, code)
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
    let target = Unmanaged<PickedTarget>.fromOpaque(rawTarget).takeUnretainedValue()
    let session = CaptureSession(config: config, target: target)
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
