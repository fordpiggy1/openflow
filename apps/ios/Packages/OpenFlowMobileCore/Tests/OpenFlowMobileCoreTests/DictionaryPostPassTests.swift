import Foundation
import Testing
@testable import OpenFlowMobileCore

@Suite struct DictionaryPostPassTests {

    /// transcribe.rs `dictionary_prompt_is_trimmed_and_bounded`, ported. The
    /// phone must accept exactly the strings the desktop accepts.
    @Test func testCapMatchesTheDesktopPrompt() {
        #expect(nil == DictionaryPostPass.capped(nil))
        #expect(nil == DictionaryPostPass.capped("   "))
        #expect(DictionaryPostPass.capped(" ENTRO.LY, FastPay ") == "ENTRO.LY, FastPay")
        #expect(DictionaryPostPass.capped(String(repeating: "x", count: 2_000))?.count == 800)
    }

    /// Rust counts `char`s, which are Unicode scalars, not grapheme clusters.
    /// `String.prefix` would count clusters and cut in a different place.
    @Test func testCapCountsUnicodeScalarsLikeRustChars() {
        let flags = String(repeating: "e\u{0301}", count: 1_000)   // e + combining acute
        let capped = DictionaryPostPass.capped(flags)
        #expect(capped?.unicodeScalars.count == 800)
    }

    @Test func testPlainEntryFixesSpellingAndCase() {
        let out = DictionaryPostPass.apply(
            "we sent it to entro.ly and then to Entro.LY again",
            dictionary: "ENTRO.LY"
        )
        #expect(out == "we sent it to ENTRO.LY and then to ENTRO.LY again")
    }

    /// Rule 1: whole-word only. This is the rule that stops a dictionary from
    /// quietly corrupting ordinary prose.
    @Test func testEntriesDoNotFireInsideLongerWords() {
        let out = DictionaryPostPass.apply(
            "the Sopranos ran on a sop of a budget",
            dictionary: "Sop"
        )
        #expect(out == "the Sopranos ran on a Sop of a budget")
    }

    /// Rule 2: longest first, so a prefix entry cannot eat a longer one.
    @Test func testLongestEntryWins() {
        let out = DictionaryPostPass.apply(
            "ask the entro.ly affiliate team",
            dictionary: "ENTRO, ENTRO.LY, ENTRO.LY affiliate"
        )
        #expect(out == "ask the ENTRO.LY affiliate team")
    }

    /// The arrow form: the thing Qwen actually gets wrong. PLAN.md section 3
    /// records 0.6B writing "intro dot lie" for "entro.ly".
    @Test func testArrowEntryRewritesAMishearing() {
        let out = DictionaryPostPass.apply(
            "send it to intro dot lie today",
            dictionary: "intro dot lie -> ENTRO.LY"
        )
        #expect(out == "send it to ENTRO.LY today")
        let fatArrow = DictionaryPostPass.apply(
            "send it to intro dot lie today",
            dictionary: "intro dot lie => ENTRO.LY"
        )
        #expect(fatArrow == "send it to ENTRO.LY today")
    }

    /// Rule 4: a lower-case dictionary entry is still capitalised where a
    /// sentence starts, so the post-pass does not undo the model's punctuation.
    @Test func testSentenceInitialCapitalisationIsPreserved() {
        let dictionary = "openflow"
        #expect(DictionaryPostPass.apply("openflow is local. openflow stays local", dictionary: dictionary) == "Openflow is local. Openflow stays local")
        #expect(DictionaryPostPass.apply("I like openflow", dictionary: dictionary) == "I like openflow")
        #expect(DictionaryPostPass.apply("first line\nopenflow second", dictionary: dictionary) == "first line\nOpenflow second")
        // An entry that is already capitalised or upper-case is untouched.
        #expect(DictionaryPostPass.apply("entro.ly ships", dictionary: "ENTRO.LY") == "ENTRO.LY ships")
    }

    /// Rule 5: no rescanning, so rules cannot chain into something nobody wrote.
    @Test func testReplacementsDoNotChain() {
        let out = DictionaryPostPass.apply(
            "say alpha now",
            dictionary: "alpha -> beta, beta -> gamma"
        )
        #expect(out == "say beta now")
        // At the start of a sentence rule 4 still applies to the replacement,
        // and rule 5 still stops the chain.
        #expect(
            DictionaryPostPass.apply("alpha now", dictionary: "alpha -> beta, beta -> gamma")
                == "Beta now"
        )
    }

    @Test func testParsingIsDeterministicAndDeduplicated() {
        let entries = DictionaryPostPass.entries(from: "b, aaa, cc, b, , aaa")
        #expect(entries.map(\.matchLowercased) == ["aaa", "cc", "b"])

        // Newlines and semicolons separate too, so a pasted list works.
        let multiline = DictionaryPostPass.entries(from: "FastPay\nENTRO.LY; Lark")
        #expect(multiline.map(\.replacement).sorted() == ["ENTRO.LY", "FastPay", "Lark"])
    }

    @Test func testEmptyDictionaryAndEmptyTextAreNoOps() {
        #expect(DictionaryPostPass.apply("hello", dictionary: nil) == "hello")
        #expect(DictionaryPostPass.apply("hello", dictionary: "   ") == "hello")
        #expect(DictionaryPostPass.apply("", dictionary: "ENTRO.LY") == "")
    }
}
