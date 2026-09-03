//! What `scripts/bundle-native.sh` will call the disk image, asserted against
//! the script itself rather than against a copy of its rules.
//!
//! The name matters beyond tidiness: the release workflow uploads an asset by
//! that name and anyone verifying a download compares it, so a version that
//! silently became empty, or an extraction that started matching a
//! dependency's `version = "..."` line instead of the crate's, would publish a
//! file called `OpenFlow__aarch64.dmg` and nothing would fail. `--print-
//! artifacts` exists so the rule has exactly one implementation and both the
//! workflow and this test read it from there.
//!
//! `cfg(unix)`: the script is bash and the crate is macOS-only anyway, but the
//! workspace's clippy and test runs also happen on a Windows runner, where
//! there is no bash to ask.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the crate sits two levels under the repo root")
}

/// Run `bundle-native.sh --print-artifacts` and return its `key=value` lines.
/// Nothing is built and nothing is written: the flag returns before the first
/// `cargo build`.
fn print_artifacts(manifest: Option<&Path>, arch: Option<&str>) -> Vec<(String, String)> {
    let root = repo_root();
    let mut command = Command::new("bash");
    command
        .arg(root.join("scripts/bundle-native.sh"))
        .arg("--print-artifacts")
        .current_dir(&root);
    if let Some(manifest) = manifest {
        command.env("OPENFLOW_CARGO_TOML", manifest);
    }
    if let Some(arch) = arch {
        command.env("OPENFLOW_DMG_ARCH", arch);
    }
    let output = command.output().expect("bash is on the path");
    assert!(
        output.status.success(),
        "--print-artifacts failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn value(pairs: &[(String, String)], key: &str) -> String {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("no {} in {:?}", key, pairs))
        .1
        .clone()
}

/// The version the script reports for the real tree is the one the crate was
/// compiled with, which is the whole claim `Info.plist` and `--version` rest
/// on: two readers of `Cargo.toml`, one answer.
#[test]
fn the_script_reads_the_same_version_the_crate_compiled_with() {
    let pairs = print_artifacts(None, None);
    assert_eq!(value(&pairs, "version"), env!("CARGO_PKG_VERSION"));
}

/// The name, in full, for a version this repository does not have -- so the
/// assertion is on the rule and not on today's number.
#[test]
fn the_dmg_is_named_for_the_version_and_the_architecture() {
    let manifest = std::env::temp_dir().join(format!(
        "openflow-bundle-naming-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    std::fs::write(
        &manifest,
        "[package]\nname = \"openflow-native\"\nversion = \"9.9.9\"\nedition = \"2021\"\n\n\
         [dependencies]\nserde = \"1\"\n",
    )
    .expect("the temp directory is writable");

    let pairs = print_artifacts(Some(&manifest), Some("aarch64"));
    let _ = std::fs::remove_file(&manifest);

    assert_eq!(value(&pairs, "version"), "9.9.9");
    assert_eq!(value(&pairs, "arch"), "aarch64");
    assert_eq!(value(&pairs, "dmg"), "OpenFlow_9.9.9_aarch64.dmg");
}

/// An Intel build must not be handed an aarch64 name, and the machine word in
/// the name is a rust triple's, not `uname`'s -- `arm64` is what `uname -m`
/// says on this Mac and `aarch64` is what the target is called.
#[test]
fn the_architecture_is_part_of_the_name() {
    let pairs = print_artifacts(None, Some("x86_64"));
    let version = value(&pairs, "version");
    assert_eq!(
        value(&pairs, "dmg"),
        format!("OpenFlow_{}_x86_64.dmg", version)
    );
}

/// A manifest with no `version` line under `[package]` is the failure that
/// would otherwise produce `OpenFlow__aarch64.dmg` and upload it happily.
#[test]
fn a_manifest_with_no_version_stops_the_script() {
    let manifest = std::env::temp_dir().join(format!(
        "openflow-bundle-naming-{}-{}.toml",
        std::process::id(),
        line!()
    ));
    std::fs::write(&manifest, "[package]\nname = \"openflow-native\"\n")
        .expect("the temp directory is writable");

    let root = repo_root();
    let output = Command::new("bash")
        .arg(root.join("scripts/bundle-native.sh"))
        .arg("--print-artifacts")
        .env("OPENFLOW_CARGO_TOML", &manifest)
        .current_dir(&root)
        .output()
        .expect("bash is on the path");
    let _ = std::fs::remove_file(&manifest);

    assert!(!output.status.success(), "it should refuse, not guess");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("No version"),
        "and say why: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
