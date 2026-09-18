//! What this build is, so the console can say what it is running.
//!
//! Three separate facts, none of which the process could previously report:
//! the crate version, the commit it was built from (stamped by `build.rs`), and
//! the container image tag it was started under. The last one cannot be
//! discovered from inside the container — Docker tells a process nothing about
//! its own image — so compose passes it in as `RELAY_IMAGE_TAG`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Crate version. Rarely bumped in this fork; kept for completeness.
    pub package_version: String,
    /// Short commit, `-dirty` when built from an uncommitted tree, or
    /// `"unknown"` when built without git and without a build argument.
    pub git_sha: String,
    /// When the image was built, as passed by the Docker build.
    pub build_time: String,
    /// The image tag this container was started under, if compose passed it.
    /// `None` means the deployment predates that wiring — treat "which tag am I"
    /// as unanswerable rather than guessing.
    pub image_tag: Option<String>,
}

pub fn current() -> VersionInfo {
    // `option_env!`, not `env!`: these come from `build.rs`, and a packaging
    // that does not ship it — the Docker build did not copy it at first — should
    // produce an unstamped relay, not a relay that will not compile. "unknown"
    // is an honest answer; failing the build over a version string is not a
    // trade worth making.
    VersionInfo {
        package_version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: option_env!("BUILD_GIT_SHA")
            .unwrap_or("unknown")
            .to_string(),
        build_time: option_env!("BUILD_TIME").unwrap_or("unknown").to_string(),
        image_tag: std::env::var("RELAY_IMAGE_TAG")
            .ok()
            .map(|tag| tag.trim().to_string())
            .filter(|tag| !tag.is_empty()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_is_always_stamped_with_something() {
        let info = current();
        assert!(!info.git_sha.is_empty());
        assert!(!info.build_time.is_empty());
        assert_eq!(info.package_version, env!("CARGO_PKG_VERSION"));
    }
}
