//! The dictionary, applied after transcription instead of only before it.
//!
//! On a hosted Whisper the dictionary is a `prompt` (see
//! [`crate::transcribe`]'s `dictionary_prompt`): the model is told the spellings
//! and gets them right itself. Qwen3-ASR ignores prompts entirely -- the
//! benchmark in `docs/native-port/local-runner-benchmark.md` has 0.6B writing
//! "intro dot lie" for "entro.ly" with the term sitting in the prompt -- so the
//! local runner needs the same string to drive a deterministic replacement over
//! the finished text.
//!
//! This is a line-for-line port of the iPhone's `DictionaryPostPass`
//! (`apps/ios/Packages/OpenFlowMobileCore/Sources/OpenFlowMobileCore/DictionaryPostPass.swift`),
//! rules and test vectors included, so a dictionary written on one device
//! behaves identically on the other.
//!
//! ## The format
//!
//! Exactly the desktop's: one free-text field, trimmed, capped at 800 `char`s,
//! entries separated by commas, newlines or semicolons. [`capped`] reproduces
//! `dictionary_prompt`, so the same string can be handed to Whisper as a prompt
//! and to this post-pass and neither sees something the other did not.
//!
//! Two entry shapes:
//! - `Term` -- match `Term` case-insensitively, rewrite it with this spelling.
//!   `ENTRO.LY` fixes `entro.ly`, `Entro.Ly` and `ENTRO.ly`.
//! - `heard -> Term` (or `=>`) -- match the left side, write the right side.
//!   This catches a mishearing the model has no way to spell correctly, e.g.
//!   `intro dot lie -> ENTRO.LY`. Whisper's prompt form cannot express this, so
//!   a dictionary using it stays valid as a prompt (the arrow is just more
//!   prompt text) while doing strictly more here.
//!
//! ## The matching rules
//!
//! 1. **Whole-word only.** A match must not be flanked by a letter or a digit on
//!    either side. `Sop` does not fire inside `Sopranos`. Punctuation inside an
//!    entry (`ENTRO.LY`) is part of the entry, not a boundary.
//! 2. **Longest entry first.** Entries are sorted by match length descending, so
//!    `ENTRO.LY affiliate` wins over `ENTRO.LY`, and `ENTRO.LY` over `ENTRO`.
//!    Ties break on the order the user typed them, so the result never depends
//!    on iteration order.
//! 3. **Case-insensitive match, dictionary casing wins.** The entry decides how
//!    the term is spelled; that is the entire point of the feature.
//! 4. **Sentence-initial capitalisation is preserved, but only for an entry that
//!    is entirely lower-case.** An entry carrying a capital of its own is left
//!    exactly as written, because that capital is the reason the entry exists:
//!    upper-casing `iPhone` or `eBay` gives `IPhone` and `EBay`, which is worse
//!    than the mistake the dictionary was added to fix.
//! 5. **One left-to-right pass, no rescanning.** Replaced spans are never
//!    matched again, so `a -> b` and `b -> c` cannot chain inside one pass. The
//!    output is a pure function of (text, dictionary).
//!
//! ## Idempotence
//!
//! [`apply`] is run on every backend, including the ones whose prompt already
//! carried the dictionary, so it has to be safe to run over text it has already
//! corrected. It is, for every entry the feature is actually for:
//!
//! - A plain `Term` entry is idempotent by construction. Its output matches its
//!   own pattern case-insensitively and rewrites to the identical spelling, and
//!   rule 4 makes the same capitalisation decision from the same position.
//! - An arrow entry `heard -> Term` is idempotent whenever `Term` is not itself
//!   the left side of another entry: `Term` does not match `heard`, so a second
//!   pass finds nothing to do.
//!
//! The one shape that is *not* idempotent is a chain the user wrote themselves:
//! with `alpha -> beta, beta -> gamma`, one pass gives `beta` (rule 5 stops the
//! chain) and a second pass gives `gamma`. Rule 5 bounds a single application,
//! not repeated ones. That is proven in
//! [`tests::a_user_written_chain_is_the_one_shape_that_is_not_idempotent`], and
//! it is why the pipeline applies the post-pass exactly once per transcript
//! rather than once per stage.

/// The desktop's `dictionary_prompt`: trim, drop if empty, keep the first 800
/// `char`s. Kept identical to the prompt cap so the two never disagree about
/// what a dictionary contains.
pub const DICTIONARY_LIMIT: usize = 800;

/// Trim, drop if empty, cap at `limit` `char`s.
pub fn capped(dictionary: Option<&str>, limit: usize) -> Option<String> {
    let text = dictionary?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(limit).collect())
}

/// One parsed rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// What to look for, lower-cased for matching.
    pub match_lowercased: String,
    /// What to write instead.
    pub replacement: String,
}

/// Parse the dictionary field into rules, in the order they will be applied.
pub fn entries(dictionary: Option<&str>) -> Vec<Entry> {
    let Some(text) = capped(dictionary, DICTIONARY_LIMIT) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut parsed: Vec<Entry> = Vec::new();
    for piece in text.split([',', '\n', ';']) {
        let raw = piece.trim();
        if raw.is_empty() {
            continue;
        }
        let mut needle = raw;
        let mut replacement = raw;
        // The first arrow the entry actually contains decides, and only that
        // one: `->` is checked before `=>`, and neither contains the other.
        for arrow in ["->", "=>"] {
            if !raw.contains(arrow) {
                continue;
            }
            let halves: Vec<&str> = raw.split(arrow).collect();
            if halves.len() == 2 {
                let left = halves[0].trim();
                let right = halves[1].trim();
                if !left.is_empty() && !right.is_empty() {
                    needle = left;
                    replacement = right;
                }
            }
            break;
        }
        let key = needle.to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key.clone());
        parsed.push(Entry {
            match_lowercased: key,
            replacement: replacement.to_string(),
        });
    }
    // Rule 2: longest first, stable within a length so ties keep typed order.
    parsed.sort_by(|a, b| {
        b.match_lowercased
            .chars()
            .count()
            .cmp(&a.match_lowercased.chars().count())
    });
    parsed
}

/// Apply the dictionary to a finished transcript.
pub fn apply(text: &str, dictionary: Option<&str>) -> String {
    apply_entries(text, &entries(dictionary))
}

/// Apply already-parsed rules. Separated so a caller that transcribes in a loop
/// parses the dictionary once.
pub fn apply_entries(text: &str, entries: &[Entry]) -> String {
    if entries.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let source: Vec<char> = text.chars().collect();
    // Matching is by index, so a lower-casing that changes the number of
    // characters (the Turkish dotted capital I, say) would make every index
    // past it point at the wrong character. Leave the text alone instead of
    // corrupting it, which is what the Swift length guard does.
    let mut lowered: Vec<char> = Vec::with_capacity(source.len());
    for character in &source {
        let mut folded = character.to_lowercase();
        let (Some(first), None) = (folded.next(), folded.next()) else {
            return text.to_string();
        };
        lowered.push(first);
    }

    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < source.len() {
        let mut matched = false;
        for entry in entries {
            let needle: Vec<char> = entry.match_lowercased.chars().collect();
            if needle.is_empty() || index + needle.len() > lowered.len() {
                continue;
            }
            if lowered[index..index + needle.len()] != needle[..] {
                continue;
            }
            // Rule 1: whole-word only.
            let before = index.checked_sub(1).map(|i| source[i]);
            let after = source.get(index + needle.len()).copied();
            if is_word_character(before) || is_word_character(after) {
                continue;
            }
            // Rule 4: sentence-initial capitalisation, for entries that
            // expressed no capitalisation of their own.
            if starts_sentence(&output) {
                output.push_str(&sentence_cased(&entry.replacement));
            } else {
                output.push_str(&entry.replacement);
            }
            index += needle.len();
            matched = true;
            break;
        }
        if !matched {
            output.push(source[index]);
            index += 1;
        }
    }
    output
}

fn is_word_character(character: Option<char>) -> bool {
    character
        .map(|c| c.is_alphabetic() || c.is_numeric())
        .unwrap_or(false)
}

/// True when nothing but sentence-ending punctuation and whitespace precedes
/// the cursor.
fn starts_sentence(emitted: &str) -> bool {
    let mut saw_whitespace = false;
    for character in emitted.chars().rev() {
        if is_newline(character) {
            return true;
        }
        if character.is_whitespace() {
            saw_whitespace = true;
            continue;
        }
        if matches!(character, '.' | '!' | '?' | ':') {
            return saw_whitespace;
        }
        return false;
    }
    true
}

fn is_newline(character: char) -> bool {
    matches!(
        character,
        '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// Upper-cases the first character, but only for an entry that is entirely
/// lower-case. `openflow` becomes `Openflow`; `iPhone`, `eBay` and `ENTRO.LY`
/// are returned untouched, because their capitals are the point.
fn sentence_cased(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return value.to_string();
    };
    if !first.is_lowercase() || value.chars().any(char::is_uppercase) {
        return value.to_string();
    }
    first.to_uppercase().collect::<String>() + characters.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `transcribe.rs`'s `dictionary_prompt_is_trimmed_and_bounded`, and the
    /// phone's `testCapMatchesTheDesktopPrompt`. All three have to accept
    /// exactly the same strings.
    #[test]
    fn the_cap_matches_the_whisper_prompt() {
        assert_eq!(capped(None, DICTIONARY_LIMIT), None);
        assert_eq!(capped(Some("   "), DICTIONARY_LIMIT), None);
        assert_eq!(
            capped(Some(" ENTRO.LY, FastPay "), DICTIONARY_LIMIT).as_deref(),
            Some("ENTRO.LY, FastPay")
        );
        assert_eq!(
            capped(Some(&"x".repeat(2_000)), DICTIONARY_LIMIT).map(|text| text.chars().count()),
            Some(800)
        );
    }

    #[test]
    fn a_plain_entry_fixes_spelling_and_case() {
        assert_eq!(
            apply(
                "we sent it to entro.ly and then to Entro.LY again",
                Some("ENTRO.LY")
            ),
            "we sent it to ENTRO.LY and then to ENTRO.LY again"
        );
    }

    /// Rule 1: whole-word only. This is the rule that stops a dictionary from
    /// quietly corrupting ordinary prose.
    ///
    /// The phone's vector is the first assertion, and on its own it does not
    /// actually test the rule: `Sop` rewrites `Sop` inside `Sopranos` to the
    /// same three characters, so deleting the boundary check leaves the output
    /// unchanged and the test still green. A mutation run caught that. The rest
    /// of the assertions use a replacement that differs from what it matched,
    /// so a missing boundary check shows up in the text.
    #[test]
    fn entries_do_not_fire_inside_longer_words() {
        assert_eq!(
            apply("the Sopranos ran on a sop of a budget", Some("Sop")),
            "the Sopranos ran on a Sop of a budget"
        );
        // A letter after the match: `Sopranos` must survive intact.
        assert_eq!(
            apply("the Sopranos ran on a sop of a budget", Some("sop -> SOP")),
            "the Sopranos ran on a SOP of a budget"
        );
        // A letter before it.
        assert_eq!(apply("asop and sop", Some("sop -> SOP")), "asop and SOP");
        // A digit on either side counts as a word character too.
        assert_eq!(apply("sop9 9sop sop", Some("sop -> SOP")), "sop9 9sop SOP");
        // Punctuation is a boundary, so a term at the end of a sentence fires.
        assert_eq!(apply("(sop), sop.", Some("sop -> SOP")), "(SOP), SOP.");
    }

    /// Rule 2: longest first, so a prefix entry cannot eat a longer one.
    #[test]
    fn the_longest_entry_wins() {
        assert_eq!(
            apply(
                "ask the entro.ly affiliate team",
                Some("ENTRO, ENTRO.LY, ENTRO.LY affiliate")
            ),
            "ask the ENTRO.LY affiliate team"
        );
    }

    /// The arrow form: the thing Qwen actually gets wrong. The benchmark
    /// records 0.6B writing "intro dot lie" for "entro.ly".
    #[test]
    fn an_arrow_entry_rewrites_a_mishearing() {
        assert_eq!(
            apply(
                "send it to intro dot lie today",
                Some("intro dot lie -> ENTRO.LY")
            ),
            "send it to ENTRO.LY today"
        );
        assert_eq!(
            apply(
                "send it to intro dot lie today",
                Some("intro dot lie => ENTRO.LY")
            ),
            "send it to ENTRO.LY today"
        );
    }

    /// Rule 4: a lower-case dictionary entry is still capitalised where a
    /// sentence starts, so the post-pass does not undo the model's punctuation.
    #[test]
    fn sentence_initial_capitalisation_is_preserved() {
        let dictionary = Some("openflow");
        assert_eq!(
            apply("openflow is local. openflow stays local", dictionary),
            "Openflow is local. Openflow stays local"
        );
        assert_eq!(apply("I like openflow", dictionary), "I like openflow");
        assert_eq!(
            apply("first line\nopenflow second", dictionary),
            "first line\nOpenflow second"
        );
        assert_eq!(apply("entro.ly ships", Some("ENTRO.LY")), "ENTRO.LY ships");
    }

    /// Rule 4's exception: an entry that carries its own capital keeps it, even
    /// at the start of a sentence.
    #[test]
    fn mixed_case_entries_keep_their_own_casing_at_a_sentence_start() {
        assert_eq!(
            apply("iphone. iphone", Some("iphone -> iPhone")),
            "iPhone. iPhone"
        );
        assert_eq!(
            apply("ebay sells it. ebay does", Some("ebay -> eBay")),
            "eBay sells it. eBay does"
        );
        // An all-lower-case entry is still sentence-cased: the exception is
        // narrow, not a removal of rule 4.
        assert_eq!(
            apply("openflow. openflow", Some("openflow")),
            "Openflow. Openflow"
        );
        // ...and an upper-case entry is untouched either way.
        assert_eq!(
            apply("entro.ly. entro.ly", Some("ENTRO.LY")),
            "ENTRO.LY. ENTRO.LY"
        );
    }

    /// Rule 5: no rescanning, so rules cannot chain into something nobody wrote.
    #[test]
    fn replacements_do_not_chain_inside_one_pass() {
        assert_eq!(
            apply("say alpha now", Some("alpha -> beta, beta -> gamma")),
            "say beta now"
        );
        assert_eq!(
            apply("alpha now", Some("alpha -> beta, beta -> gamma")),
            "Beta now"
        );
    }

    #[test]
    fn parsing_is_deterministic_and_deduplicated() {
        let parsed = entries(Some("b, aaa, cc, b, , aaa"));
        let keys: Vec<&str> = parsed
            .iter()
            .map(|entry| entry.match_lowercased.as_str())
            .collect();
        assert_eq!(keys, ["aaa", "cc", "b"]);

        // Newlines and semicolons separate too, so a pasted list works.
        let mut multiline: Vec<String> = entries(Some("FastPay\nENTRO.LY; Lark"))
            .into_iter()
            .map(|entry| entry.replacement)
            .collect();
        multiline.sort();
        assert_eq!(multiline, ["ENTRO.LY", "FastPay", "Lark"]);
    }

    #[test]
    fn an_empty_dictionary_or_empty_text_is_a_no_op() {
        assert_eq!(apply("hello", None), "hello");
        assert_eq!(apply("hello", Some("   ")), "hello");
        assert_eq!(apply("", Some("ENTRO.LY")), "");
    }

    /// The property the pipeline relies on: running the post-pass over text it
    /// has already corrected changes nothing, for both entry shapes and at both
    /// sentence positions.
    #[test]
    fn applying_twice_changes_nothing_for_the_shapes_the_feature_is_for() {
        for (text, dictionary) in [
            ("we sent it to entro.ly again", "ENTRO.LY"),
            ("entro.ly. entro.ly", "ENTRO.LY"),
            ("openflow. openflow is local", "openflow"),
            (
                "send it to intro dot lie today",
                "intro dot lie -> ENTRO.LY",
            ),
            ("iphone. iphone", "iphone -> iPhone"),
            (
                "ask the entro.ly affiliate team",
                "ENTRO, ENTRO.LY, ENTRO.LY affiliate",
            ),
            ("the Sopranos ran on a sop", "Sop"),
        ] {
            let once = apply(text, Some(dictionary));
            let twice = apply(&once, Some(dictionary));
            assert_eq!(once, twice, "{dictionary} over {text:?} must be idempotent");
        }
    }

    /// ...and the one shape that is not, so the boundary is written down rather
    /// than assumed. Rule 5 bounds a single application, not repeated ones, and
    /// this is why the pipeline applies the post-pass exactly once.
    #[test]
    fn a_user_written_chain_is_the_one_shape_that_is_not_idempotent() {
        let dictionary = Some("alpha -> beta, beta -> gamma");
        let once = apply("say alpha now", dictionary);
        assert_eq!(once, "say beta now");
        assert_eq!(apply(&once, dictionary), "say gamma now");
    }

    /// A lower-casing that changes the character count would make every index
    /// past it point at the wrong character, so the pass declines instead.
    #[test]
    fn text_that_does_not_lower_case_one_for_one_is_left_alone() {
        // U+0130 LATIN CAPITAL LETTER I WITH DOT ABOVE lower-cases to two
        // characters.
        let text = "\u{0130}stanbul openflow";
        assert_eq!(apply(text, Some("openflow")), text);
        // The same text without it is corrected normally, so the guard is what
        // made the difference and not a missing entry.
        assert_eq!(
            apply("Istanbul openflow", Some("openflow")),
            "Istanbul openflow"
        );
        assert_eq!(apply("openflow", Some("openflow")), "Openflow");
    }
}
