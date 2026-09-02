import Foundation

#if canImport(AVFoundation)
import AVFoundation
#endif

// MARK: - The maths (pure, and the part the tests exercise)

/// Sample-rate conversion, ported from `downsample` / `design_lowpass` in
/// `src-tauri/src/audio.rs`.
///
/// Interpolation alone does not prevent aliasing: at 48k -> 16k every component
/// above the new 8 kHz Nyquist folds back into the speech band regardless of how
/// the output points are interpolated -- a 15 kHz whine lands on 1 kHz, right on
/// top of the voice. Measured on the desktop, plain decimation and linear
/// interpolation both leave the alias at -0.0 dB; filtering first drops it to
/// -60 dB while the passband below 6 kHz stays within 0.1 dB.
public enum AudioResampler {
    /// Anti-alias filter length. 63 taps buys ~60 dB of stopband rejection at
    /// 48k -> 16k, far more than speech needs. (audio.rs `FIR_TAPS`)
    public static let firTaps = 63

    /// Windowed-sinc low-pass, Hamming window, normalised to unity DC gain.
    public static func designLowpass(cutoffHz: Float, sampleRate: Float, numTaps: Int) -> [Float] {
        let fc = cutoffHz / sampleRate
        let m = Float(numTaps - 1)
        var taps = [Float]()
        taps.reserveCapacity(numTaps)
        for i in 0..<numTaps {
            let n = Float(i) - m / 2
            let sinc: Float
            if abs(n) < 1e-6 {
                sinc = 2 * fc
            } else {
                sinc = sin(2 * .pi * fc * n) / (.pi * n)
            }
            let window = 0.54 - 0.46 * cos(2 * .pi * Float(i) / m)
            taps.append(sinc * window)
        }
        let sum = taps.reduce(0, +)
        if abs(sum) < 1e-9 { return taps }
        return taps.map { $0 / sum }
    }

    /// The filter used for a decimation to `toRate`.
    ///
    /// 0.45 * toRate leaves a transition band below the new Nyquist while keeping
    /// everything speech uses (< 7.2 kHz at a 16 kHz output).
    public static func decimationTaps(fromRate: Double, toRate: Double) -> [Float] {
        designLowpass(
            cutoffHz: 0.45 * Float(toRate),
            sampleRate: Float(fromRate),
            numTaps: firTaps
        )
    }

    /// Resample to `toRate`, low-passing first when decimating.
    public static func downsample(_ samples: [Float], from fromRate: Double, to toRate: Double) -> [Float] {
        if fromRate == toRate { return samples }
        if samples.isEmpty || fromRate <= 0 || toRate <= 0 { return [] }

        let ratio = fromRate / toRate
        let outputLength = Int(Double(samples.count) / ratio)
        if outputLength == 0 { return [] }

        // Upsampling needs interpolation, not decimation; no anti-alias filter
        // applies. Keep the linear interpolation for that direction.
        if fromRate < toRate {
            var output = [Float]()
            output.reserveCapacity(outputLength)
            for i in 0..<outputLength {
                let position = Double(i) * ratio
                let left = Int(position.rounded(.down))
                if left >= samples.count { break }
                let right = min(left + 1, samples.count - 1)
                let fraction = Float(position - Double(left))
                output.append(samples[left] + (samples[right] - samples[left]) * fraction)
            }
            return output
        }

        let taps = decimationTaps(fromRate: fromRate, toRate: toRate)
        let half = taps.count / 2
        var output = [Float]()
        output.reserveCapacity(outputLength)
        for i in 0..<outputLength {
            let center = Int(Double(i) * ratio)
            var acc: Float = 0
            for (k, tap) in taps.enumerated() {
                let index = center + k - half
                if index >= 0 && index < samples.count {
                    acc += samples[index] * tap
                }
            }
            output.append(acc)
        }
        return output
    }
}

/// The same decimation, fed a block at a time from the microphone tap.
///
/// The desktop can accumulate a whole take at the native rate and convert once,
/// because it has the memory to spare. The phone's ring is 16 kHz (PLAN.md
/// section 5), so the conversion happens in the tap. This keeps the filter's
/// input history across block boundaries so the result is identical to
/// converting the whole take at once -- `flush()` closes out the tail with the
/// same zero padding the batch version uses.
public struct StreamingDownsampler: Sendable {
    private let ratio: Double
    private let taps: [Float]
    private let half: Int
    private let passthrough: Bool

    /// Input samples still needed by the filter, oldest first.
    private var pending: [Float] = []
    /// Global index of `pending[0]` in the input stream.
    private var pendingBase = 0
    /// Total input samples seen.
    private var inputCount = 0
    /// Output samples emitted so far.
    private var outputCount = 0

    public init(from fromRate: Double, to toRate: Double) {
        self.passthrough = (fromRate == toRate) || fromRate <= 0 || toRate <= 0
        self.ratio = passthrough ? 1 : fromRate / toRate
        if passthrough || fromRate < toRate {
            self.taps = []
            self.half = 0
        } else {
            let designed = AudioResampler.decimationTaps(fromRate: fromRate, toRate: toRate)
            self.taps = designed
            self.half = designed.count / 2
        }
    }

    /// Convert one block. Returns only the output samples the filter can produce
    /// without seeing the future.
    public mutating func process(_ block: [Float]) -> [Float] {
        guard !block.isEmpty else { return [] }
        if passthrough || taps.isEmpty {
            inputCount += block.count
            outputCount += block.count
            return block
        }
        pending.append(contentsOf: block)
        inputCount += block.count
        return emit(upTo: inputCount, zeroPadTail: false)
    }

    /// Close out the take: emit every remaining output sample, padding past the
    /// end of the input with zeros exactly as the batch converter does.
    public mutating func flush() -> [Float] {
        guard !passthrough, !taps.isEmpty else { return [] }
        let tail = emit(upTo: inputCount, zeroPadTail: true)
        pending.removeAll(keepingCapacity: true)
        return tail
    }

    private mutating func emit(upTo availableInputs: Int, zeroPadTail: Bool) -> [Float] {
        let totalOutputs = Int(Double(availableInputs) / ratio)
        var output = [Float]()
        while outputCount < totalOutputs {
            let center = Int(Double(outputCount) * ratio)
            let lastNeeded = center + taps.count - 1 - half
            if !zeroPadTail && lastNeeded >= availableInputs { break }
            var acc: Float = 0
            for (k, tap) in taps.enumerated() {
                let index = center + k - half
                if index >= 0 && index < availableInputs {
                    let local = index - pendingBase
                    if local >= 0 && local < pending.count {
                        acc += pending[local] * tap
                    }
                }
            }
            output.append(acc)
            outputCount += 1
            // Drop history the next output can no longer reach.
            let nextCenter = Int(Double(outputCount) * ratio)
            let keepFrom = max(0, nextCenter - half)
            if keepFrom > pendingBase {
                pending.removeFirst(min(keepFrom - pendingBase, pending.count))
                pendingBase = keepFrom
            }
        }
        return output
    }
}

/// Preallocated 16 kHz Float32 ring. PLAN.md section 5: the capture pipeline
/// allocates once per take, and ten minutes is the ceiling.
///
/// Ten minutes at 16 kHz is 9.6 M floats, 38.4 MB. When a take runs past that the
/// ring keeps the most recent ten minutes and raises `didOverflow`, which is the
/// watchdog's cue to stop the capture -- the desktop's `MAX_CAPTURE_SAMPLES`
/// rule, translated to a bounded ring instead of a bounded vector.
public struct CaptureRingBuffer: Sendable {
    public static let sampleRate: Double = 16_000
    public static let maxSeconds: Double = 600

    public let capacity: Int
    private var storage: [Float]
    private var writeIndex = 0
    public private(set) var totalWritten = 0

    public init(capacity: Int = Int(sampleRate * maxSeconds)) {
        self.capacity = max(1, capacity)
        self.storage = [Float](repeating: 0, count: max(1, capacity))
    }

    public var didOverflow: Bool { totalWritten > capacity }
    public var count: Int { min(totalWritten, capacity) }
    public var seconds: Double { Double(count) / Self.sampleRate }

    public mutating func append(_ samples: [Float]) {
        for sample in samples {
            storage[writeIndex] = sample
            writeIndex = (writeIndex + 1) % capacity
            totalWritten += 1
        }
    }

    /// The take, oldest sample first.
    public func snapshot() -> [Float] {
        let available = count
        guard available > 0 else { return [] }
        if totalWritten <= capacity {
            return Array(storage[0..<available])
        }
        return Array(storage[writeIndex..<capacity]) + Array(storage[0..<writeIndex])
    }

    public mutating func reset() {
        writeIndex = 0
        totalWritten = 0
    }
}

/// What a finished take produced.
public struct CaptureResult: Sendable {
    /// 16 kHz mono Float32, auto-gained, ready for `SpeechEngine.transcribe`.
    public var samples16k: [Float]
    public var seconds: Double
    /// True when the whole-take gate says nothing reached the microphone.
    public var isSilent: Bool
    /// True when the ten-minute watchdog cut the take short.
    public var hitWatchdog: Bool
}

public enum AudioCaptureError: Error, Equatable, Sendable {
    case permissionDenied
    case engineUnavailable(String)
    case notRecording
    case tooShort
}

/// Shared between the real-time tap and the actor. The tap runs on an audio
/// thread with no actor of its own, so the ring lives behind a lock rather than
/// inside the actor: `@unchecked Sendable` because the lock is the proof.
final class CaptureBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var ring: CaptureRingBuffer
    private var downsampler: StreamingDownsampler
    private var lastLevel: Float = 0

    init(inputRate: Double) {
        self.ring = CaptureRingBuffer()
        self.downsampler = StreamingDownsampler(from: inputRate, to: CaptureRingBuffer.sampleRate)
    }

    func write(mono: [Float]) {
        let converted = downsampler.process(mono)
        guard !converted.isEmpty else { return }
        let level = SilenceGate.speechLevel(converted)
        lock.lock()
        ring.append(converted)
        lastLevel = level
        lock.unlock()
    }

    /// The most recent block's 95th-percentile level, for stop-on-silence.
    var level: Float {
        lock.lock()
        defer { lock.unlock() }
        return lastLevel
    }

    var didOverflow: Bool {
        lock.lock()
        defer { lock.unlock() }
        return ring.didOverflow
    }

    func finish() -> (samples: [Float], overflowed: Bool) {
        lock.lock()
        let tail = downsampler.flush()
        if !tail.isEmpty { ring.append(tail) }
        let samples = ring.snapshot()
        let overflowed = ring.didOverflow
        lock.unlock()
        return (samples, overflowed)
    }
}

#if canImport(AVFoundation)

/// The microphone tap. AVAudioEngine gives us the input node's native format; we
/// mix to mono and decimate to 16 kHz in the tap so the ring stays at the size
/// PLAN.md section 5 budgets for.
public actor AudioCapture {
    private var engine: AVAudioEngine?
    private var buffer: CaptureBuffer?
    private var startedAt: Date?

    public init() {}

    public var isRecording: Bool { engine != nil }

    /// The most recent block level, so the sheet can run stop-on-silence without
    /// reaching into the audio thread itself.
    public var currentLevel: Float { buffer?.level ?? 0 }

    /// True once the ten-minute watchdog has tripped.
    public var watchdogTripped: Bool { buffer?.didOverflow ?? false }

    public func start() throws {
        guard engine == nil else { return }
        #if os(iOS)
        let session = AVAudioSession.sharedInstance()
        do {
            // `.record` and not `.playAndRecord`: OpenFlow never plays anything,
            // and the narrower category is one less thing to explain at review.
            try session.setCategory(.record, mode: .measurement)
            try session.setActive(true, options: [])
        } catch {
            throw AudioCaptureError.engineUnavailable(error.localizedDescription)
        }
        #endif

        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.inputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw AudioCaptureError.engineUnavailable("The input node reported no usable format")
        }
        let capture = CaptureBuffer(inputRate: format.sampleRate)
        input.installTap(onBus: 0, bufferSize: 4_096, format: format) { pcm, _ in
            guard let channels = pcm.floatChannelData else { return }
            let frames = Int(pcm.frameLength)
            guard frames > 0 else { return }
            let channelCount = Int(pcm.format.channelCount)
            var mono = [Float](repeating: 0, count: frames)
            // Same rule as the desktop's `mix_frame_to_mono`: average every
            // channel, never pick channel 0 and hope.
            for frame in 0..<frames {
                var sum: Float = 0
                for channel in 0..<channelCount {
                    sum += channels[channel][frame]
                }
                mono[frame] = sum / Float(channelCount)
            }
            capture.write(mono: mono)
        }
        do {
            engine.prepare()
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            throw AudioCaptureError.engineUnavailable(error.localizedDescription)
        }
        self.engine = engine
        self.buffer = capture
        self.startedAt = Date()
    }

    /// Stop and hand back the take, auto-gained and gate-checked.
    public func stop() throws -> CaptureResult {
        guard let engine, let buffer else { throw AudioCaptureError.notRecording }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        self.engine = nil
        self.buffer = nil
        self.startedAt = nil
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        #endif

        let (samples, overflowed) = buffer.finish()
        // The desktop refuses anything under 800 samples (50 ms) as a mis-tap.
        guard samples.count >= 800 else { throw AudioCaptureError.tooShort }
        let silent = SilenceGate.isSilent(samples)
        return CaptureResult(
            samples16k: SilenceGate.autoGain(samples),
            seconds: Double(samples.count) / CaptureRingBuffer.sampleRate,
            isSilent: silent,
            hitWatchdog: overflowed
        )
    }

    /// Abandon a take without producing a transcript.
    public func cancel() {
        guard let engine else { return }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        self.engine = nil
        self.buffer = nil
        self.startedAt = nil
        #if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(false, options: [.notifyOthersOnDeactivation])
        #endif
    }
}

#endif
