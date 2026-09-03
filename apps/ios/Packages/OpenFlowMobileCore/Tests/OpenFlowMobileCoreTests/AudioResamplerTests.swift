import Foundation
import Testing
@testable import OpenFlowMobileCore

@Suite struct AudioResamplerTests {

    /// audio.rs `downsample_interpolates_and_preserves_duration`
    @Test func testDownsampleInterpolatesAndPreservesDuration() {
        let input: [Float] = (0..<48_000).map { Float($0) / 48_000 }
        let output = AudioResampler.downsample(input, from: 48_000, to: 16_000)
        #expect(output.count == 16_000)
        #expect(abs((output[8_000]) - (0.5)) < 0.001)
    }

    /// audio.rs `downsample_rejects_aliasing`. 15 kHz folds onto 1 kHz at a
    /// 16 kHz output rate; linear interpolation leaves it at full strength.
    @Test func testDownsampleRejectsAliasing() {
        let out = AudioResampler.downsample(tone(15_000, 48_000, 0.5), from: 48_000, to: 16_000)
        let ghost = energyAt(out, 16_000, 1_000)
        #expect(ghost < 0.05, "15 kHz aliased into the speech band: \(ghost)")
    }

    /// The anti-alias test above only means something if the same measurement
    /// screams when the filter is removed. Plain decimation of the same tone
    /// must leave the ghost at full strength -- otherwise the assertion above
    /// would pass with no filter at all.
    @Test func testTheAliasTestWouldFailWithoutTheFilter() {
        let input = tone(15_000, 48_000, 0.5)
        let decimated = stride(from: 0, to: input.count, by: 3).map { input[$0] }
        let ghost = energyAt(decimated, 16_000, 1_000)
        #expect(ghost > 0.5, "plain decimation must alias, or the filter test proves nothing: \(ghost)")
    }

    /// audio.rs `downsample_preserves_speech_band`
    @Test func testDownsamplePreservesSpeechBand() {
        let out = AudioResampler.downsample(tone(1_000, 48_000, 0.5), from: 48_000, to: 16_000)
        #expect(energyAt(out, 16_000, 1_000) > 0.8, "1 kHz speech tone must survive decimation")
    }

    @Test func testDownsampleEdgeCases() {
        #expect(AudioResampler.downsample([], from: 48_000, to: 16_000) == [])
        let passthrough: [Float] = [0.1, 0.2, 0.3]
        #expect(AudioResampler.downsample(passthrough, from: 16_000, to: 16_000) == passthrough)
        // Upsampling takes the interpolation path, not the filter path.
        let up = AudioResampler.downsample([0, 1], from: 8_000, to: 16_000)
        #expect(up.count == 4)
    }

    /// The microphone tap converts a block at a time. If that is not identical to
    /// converting the whole take at once, the ring holds something the desktop
    /// would never have produced.
    @Test func testStreamingConversionMatchesTheBatchConversion() {
        let input = tone(440, 48_000, 0.35)
        let batch = AudioResampler.downsample(input, from: 48_000, to: 16_000)

        var streaming = StreamingDownsampler(from: 48_000, to: 16_000)
        var produced: [Float] = []
        for block in stride(from: 0, to: input.count, by: 1_024) {
            let end = min(block + 1_024, input.count)
            produced.append(contentsOf: streaming.process(Array(input[block..<end])))
        }
        produced.append(contentsOf: streaming.flush())

        #expect(produced.count == batch.count)
        for index in 0..<min(produced.count, batch.count) {
            #expect(abs((produced[index]) - (batch[index])) < 1e-6, "sample \(index)")
        }
    }

    @Test func testRingBufferKeepsTheMostRecentAudioAndFlagsTheWatchdog() {
        var ring = CaptureRingBuffer(capacity: 5)
        ring.append([1, 2, 3])
        #expect(ring.snapshot() == [1, 2, 3])
        #expect(!ring.didOverflow)

        ring.append([4, 5, 6, 7])
        #expect(ring.didOverflow, "past capacity the watchdog must say so")
        #expect(ring.snapshot() == [3, 4, 5, 6, 7], "the ring keeps the newest audio")

        ring.reset()
        #expect(ring.snapshot() == [])
        #expect(!ring.didOverflow)
    }

    /// PLAN.md section 5 budgets ten minutes at 16 kHz, allocated once.
    @Test func testDefaultRingIsTenMinutesAtSixteenKilohertz() {
        let ring = CaptureRingBuffer()
        #expect(ring.capacity == 9_600_000)
        #expect(CaptureRingBuffer.maxSeconds == 600)
        #expect(CaptureRingBuffer.sampleRate == 16_000)
    }
}
