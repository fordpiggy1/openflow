//! What this build calls itself, in one place.
//!
//! Three surfaces have to agree: `--version` on the command line, the About
//! panel in the app menu, and `CFBundleShortVersionString` plus `OpenFlowCommit`
//! in the bundle's `Info.plist`. They agree because there is exactly one source
//! for each half -- the version from `Cargo.toml`, the commit from `build.rs` --
//! and the bundle script does not compute either of them itself: it asks the
//! binary it just built (`openflow --version`) and copies the answer into the
//! plist. A bundle can therefore never claim a commit its executable was not
//! built from, however stale the tree it was assembled in.
//!
//! The module is deliberately not `cfg(target_os = "macos")`. It is pure string
//! handling with nothing platform-specific in it, and the non-macOS `main`
//! answers `--version` too, so the formatting is compiled and tested on Linux
//! CI as well as here.

/// The crate version, read from `crates/openflow-native/Cargo.toml` at compile
/// time. The bundle script reads the same line with `sed`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The short commit the binary was built from, or `unknown` outside a git
/// checkout. Set by `build.rs`; see there for the order it tries.
pub const COMMIT: &str = env!("OPENFLOW_BUILD_COMMIT");

/// `OpenFlow 0.1.0 (a1b2c3d)`. One line, and the same line everywhere it is
/// shown, so a version pasted into an issue can be grepped for verbatim.
pub fn describe(version: &str, commit: &str) -> String {
    format!("OpenFlow {} ({})", version, commit)
}

/// [`describe`] for this build.
pub fn long() -> String {
    describe(VERSION, COMMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_is_name_version_and_commit_in_parentheses() {
        assert_eq!(describe("0.1.0", "a1b2c3d"), "OpenFlow 0.1.0 (a1b2c3d)");
        assert_eq!(describe("1.2.3", "unknown"), "OpenFlow 1.2.3 (unknown)");
    }

    /// The bundle script parses the commit back out of this line with a
    /// `sed` that takes what is between the last parentheses, so a format
    /// with no parentheses, or a commit containing one, would silently put
    /// the wrong string into `Info.plist`.
    #[test]
    fn the_commit_is_recoverable_from_the_line() {
        let line = describe("0.1.0", "a1b2c3d");
        let open = line.rfind('(').expect("an opening parenthesis");
        let close = line.rfind(')').expect("a closing parenthesis");
        assert!(open < close, "the parentheses are the right way round");
        assert_eq!(&line[open + 1..close], "a1b2c3d");
    }

    /// `build.rs` promises a short hash or the literal `unknown`, and the
    /// promise is what the plist and the About panel are built on. A commit
    /// that arrived with a trailing newline, or an `OPENFLOW_COMMIT` that
    /// expanded to an empty workflow expression, would break every consumer
    /// downstream of it, so it is asserted against the real baked value
    /// rather than a fixture.
    #[test]
    fn the_baked_commit_is_a_short_hash_or_unknown() {
        assert!(!COMMIT.is_empty(), "build.rs always bakes something");
        assert_eq!(COMMIT.trim(), COMMIT, "no stray whitespace from git");
        if COMMIT != "unknown" {
            assert!(
                COMMIT.len() >= 4 && COMMIT.len() <= 40,
                "a git short hash, got {:?}",
                COMMIT
            );
            assert!(
                COMMIT
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "lowercase hex only, got {:?}",
                COMMIT
            );
        }
    }

    /// `VERSION` is what the bundle script's `sed` pulls out of `Cargo.toml`
    /// and what goes into `CFBundleShortVersionString`, which macOS requires
    /// to be dot-separated digits.
    #[test]
    fn the_version_is_a_plist_safe_number() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert!(
            (2..=3).contains(&parts.len()),
            "two or three components, got {:?}",
            VERSION
        );
        assert!(
            parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
            "digits only, got {:?}",
            VERSION
        );
    }
}
