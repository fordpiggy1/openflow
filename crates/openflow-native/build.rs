//! Bake the commit the binary was built from into the binary.
//!
//! A version number alone cannot tell two builds apart: `0.1.0` is what every
//! build between two releases calls itself, and the builds people actually run
//! are the ones in between. A bug report that says `OpenFlow 0.1.0 (a1b2c3d)`
//! names a tree; one that says `OpenFlow 0.1.0` names a fortnight.
//!
//! Three sources, in order:
//!
//! 1. `OPENFLOW_COMMIT` from the environment. CI sets it, because a workflow
//!    knows the commit it checked out and a shallow or detached checkout does
//!    not always let `git` say the same thing.
//! 2. `git rev-parse --short HEAD`, for a normal build in a checkout.
//! 3. `unknown`, when neither works -- a `cargo install` from a crates.io
//!    tarball, or a source drop with no `.git`. That is a fallback and not an
//!    error: a build that refuses to happen outside a git checkout would be a
//!    worse trade than one that admits it does not know.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    // Both inputs, so a rebuild after a commit (or after CI changes its mind)
    // picks up the new value instead of serving the cached one. `logs/HEAD` is
    // the reflog: a plain file appended to by every commit and every checkout,
    // including the ones that only move a packed ref, which is what watching
    // `refs/heads` alone would miss.
    //
    // The paths are asked of git rather than spelled `../../.git/...`, because
    // `.git` is not always a directory. In a linked worktree -- which is how
    // this port is being developed -- it is a one-line file pointing at
    // `<main repo>/.git/worktrees/<name>`, so every hardcoded path misses,
    // every `exists()` says no, nothing is tracked, and the build script never
    // runs again: the binary keeps reporting the commit it was first built at
    // while the tree moves on underneath it. That is a stale version string
    // that looks exactly like a correct one, which is the worst kind.
    println!("cargo::rerun-if-env-changed=OPENFLOW_COMMIT");
    for name in ["HEAD", "logs/HEAD", "refs/heads"] {
        if let Some(path) = git_path(name) {
            if path.exists() {
                println!("cargo::rerun-if-changed={}", path.display());
            }
        }
    }

    println!("cargo::rustc-env=OPENFLOW_BUILD_COMMIT={}", commit());
}

/// Where git actually keeps `name` for this checkout: `.git/HEAD` in a plain
/// clone, `.git/worktrees/<name>/HEAD` in a linked worktree, and nothing at all
/// outside a repository.
fn git_path(name: &str) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn commit() -> String {
    if let Ok(from_env) = std::env::var("OPENFLOW_COMMIT") {
        if let Some(value) = clean(&from_env) {
            return value;
        }
    }
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            if let Some(value) = clean(&String::from_utf8_lossy(&output.stdout)) {
                return value;
            }
        }
    }
    "unknown".to_string()
}

/// Keep the value to something that can be printed on one line and matched by
/// a test: a git short hash, and nothing else. An empty `OPENFLOW_COMMIT` (a
/// workflow expression that expanded to nothing) falls through to git rather
/// than baking a blank into the version string.
fn clean(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let usable = !trimmed.is_empty()
        && trimmed.len() <= 40
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    usable.then(|| trimmed.to_string())
}
