import Foundation
import Testing
@testable import OpenFlowMobileCore

/// These are the desktop's tests, ported sample for sample from
/// `src-tauri/src/audio.rs`. If one of them fails here and passes there, the two
/// implementations have drifted and the phone is no longer doing what the Mac
/// does. That is the whole point of copying the vectors rather than inventing
/// new ones.
@Suite struct SilenceGateTests {

    /// The constants are the contract with audio.rs. Pinned literally so a
    /// well-meaning tweak has to argue with a test.
    @Test func testConstantsMatchTheDesktop() {
        #expect(SilenceGate.targetPeak == 0.21)
        #expect(SilenceGate.maxGain == 20.0)
        #expect(SilenceGate.silenceLevel == 1e-3)
        #expect(SilenceGate.gainFloor == 1e-4)
        #expect(AudioResampler.firTaps == 63)
    }

    /// audio.rs `silence_gate_rejects_dead_input_but_keeps_quiet_speech`
    @Test func testSilenceGateRejectsDeadInputButKeepsQuietSpeech() {
        #expect(SilenceGate.isSilent([Float](repeating: 0, count: 16_000)))

        let hiss: [Float] = (0..<16_000).map { $0 % 2 == 0 ? 2e-4 : -2e-4 }
        #expect(SilenceGate.isSilent(hiss), "a virtual device's noise floor is not speech")

        let quiet = tone(300, 16_000, 1.0).map { $0 * 0.01 }
        #expect(!SilenceGate.isSilent(quiet), "a quiet real take must pass the gate")

        let mostlySilent = [Float](repeating: 0, count: 32_000) + quiet
        #expect(
            !SilenceGate.isSilent(mostlySilent),
            "two seconds of leading silence must not fail a real take"
        )
    }

    /// audio.rs `auto_gain_survives_one_loud_transient`
    @Test func testAutoGainSurvivesOneLoudTransient() {
        let quiet = tone(300, 16_000, 1.0).map { $0 * 0.03 }
        let cleanLevel = rms(SilenceGate.autoGain(quiet))

        var bumped = quiet
        for index in 0..<200 { bumped[index] = 0.95 }
        let bumpedLevel = rms(SilenceGate.autoGain(bumped))

        #expect(cleanLevel > rms(quiet) * 2, "quiet take must be boosted")
        #expect(bumpedLevel > cleanLevel * 0.5, "one transient must not cancel the boost: clean=\(cleanLevel) bumped=\(bumpedLevel)")
    }

    /// audio.rs `auto_gain_leaves_silence_alone_and_never_clips`
    @Test func testAutoGainLeavesSilenceAloneAndNeverClips() {
        let silence = [Float](repeating: 0, count: 1_000)
        #expect(SilenceGate.autoGain(silence) == silence)
        #expect(SilenceGate.autoGain([]).isEmpty)
        #expect(SilenceGate.autoGain(tone(300, 16_000, 0.2)).allSatisfy { abs($0) <= 1.0 })
    }

    /// The percentile itself, not just its consequences: a run of 100 samples
    /// where only the top 5 are loud must report the quiet level, and the
    /// truncating index rule must match Rust's `as usize` cast.
    @Test func testSpeechLevelIsTheNinetyFifthPercentileNotThePeak() {
        var samples = [Float](repeating: 0.1, count: 95)
        samples.append(contentsOf: [Float](repeating: 0.9, count: 5))
        #expect(abs((SilenceGate.speechLevel(samples)) - (0.9)) < 1e-6)

        var quieter = [Float](repeating: 0.1, count: 96)
        quieter.append(contentsOf: [Float](repeating: 0.9, count: 4))
        #expect(abs((SilenceGate.speechLevel(quieter)) - (0.1)) < 1e-6)

        #expect(SilenceGate.speechLevel([]) == 0)
        #expect(SilenceGate.speechLevel([0.5]) == 0.5)
    }

    /// The gate has to be capable of failing, or it is decoration. A take one
    /// notch either side of the line must land on opposite sides of it.
    @Test func testTheGateActuallyFiresAtItsThreshold() {
        let justUnder = [Float](repeating: SilenceGate.silenceLevel * 0.9, count: 1_000)
        let justOver = [Float](repeating: SilenceGate.silenceLevel * 1.1, count: 1_000)
        #expect(SilenceGate.isSilent(justUnder))
        #expect(!SilenceGate.isSilent(justOver))
    }
}
