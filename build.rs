//! Build script: capture the short git commit hash so `--version` can report
//! exactly which commit a binary was built from.
//!
//! The hash is exposed to the crate as the `VLSCII_GIT_HASH` env var (read via
//! `env!` at compile time). When git isn't available or this isn't a checkout
//! (e.g. a packaged source tarball), the var is set to `unknown` and the build
//! still succeeds.

use std::process::Command;

fn main() {
    let hash = git_short_hash().unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VLSCII_GIT_HASH={hash}");

    // Re-run if HEAD moves (new commit / checkout) so the baked hash stays fresh.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");
}

/// `git rev-parse --short HEAD`, with a `-dirty` suffix if the tree has
/// uncommitted changes. Returns None if git or the repo isn't available.
fn git_short_hash() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut hash = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }

    // Mark a dirty working tree so a hash can't misrepresent local edits.
    if let Ok(status) = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        && !status.stdout.is_empty()
    {
        hash.push_str("-dirty");
    }

    Some(hash)
}
