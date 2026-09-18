//! Stamp the build with the commit it came from.
//!
//! "What version is this relay running?" had no answer from inside the process:
//! `/health` returns the literal string `OK`, the crate version has been 0.1.0
//! since the fork, and the image tag is not visible to the code running in it.
//! That is fine until an operator needs to decide whether to update, at which
//! point it is the only question that matters.
//!
//! Values come from build arguments where there are any (the Docker build passes
//! them), and from git otherwise, so a plain `cargo build` on a workstation is
//! stamped too. Everything degrades to "unknown" rather than failing: a missing
//! git binary must never break the build.

use std::process::Command;

fn main() {
    // Re-run when the build arguments change. Without this cargo caches the
    // stamp and a rebuilt image reports the commit of the one before it.
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-env-changed=BUILD_TIME");
    println!("cargo:rerun-if-changed=build.rs");

    let git_sha = std::env::var("GIT_SHA")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(git_describe)
        .unwrap_or_else(|| "unknown".to_string());

    let build_time = std::env::var("BUILD_TIME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BUILD_GIT_SHA={git_sha}");
    println!("cargo:rustc-env=BUILD_TIME={build_time}");
}

/// Short commit, with `-dirty` when the tree has uncommitted changes — a local
/// build that does not correspond to any commit should say so.
fn git_describe() -> Option<String> {
    let sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let sha = String::from_utf8(sha.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        return None;
    }

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .is_some_and(|out| out.status.success() && !out.stdout.is_empty());

    Some(if dirty { format!("{sha}-dirty") } else { sha })
}
