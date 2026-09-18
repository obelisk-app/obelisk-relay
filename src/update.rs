//! Moving the relay to a different published image, from the console.
//!
//! The relay can restart itself — exit, and `restart: unless-stopped` brings it
//! back — but compose does not re-resolve the image tag on a restart, so that
//! returns the *same* build. A genuine update means `docker compose pull` and
//! recreating the container, which is host-level work, and the relay container
//! has no Docker socket.
//!
//! It deliberately does not get one. A socket mount is root on the host, handed
//! to a process that terminates untrusted WebSocket traffic from the open
//! internet; any remote-code-execution bug in the relay would become a host
//! compromise. So the relay only ever *asks*: it writes a request file into its
//! config directory, and a small root-owned agent on the host
//! (`scripts/relay-updater.sh`, driven by a systemd path unit) picks it up,
//! validates it, and does the work.
//!
//! That split is also why the tag is validated twice. The relay checks the
//! requested tag against what GHCR actually publishes; the agent re-validates it
//! against a character allowlist and supplies the repository itself, because the
//! request file is written by a network-facing service and the agent runs as
//! root. Neither side trusts the other's checking.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// The only repository an update may come from. The request carries a tag, never
/// a full image reference, so there is no way to point this at another registry.
pub const IMAGE_REPOSITORY: &str = "ghcr.io/obelisk-app/obelisk-relay";

const REQUEST_FILE: &str = "update-request.json";
const RESULT_FILE: &str = "update-result.json";
const AGENT_FILE: &str = "update-agent.json";

/// How long the agent's heartbeat stays believable. The path unit fires on
/// demand, so the agent also touches this on a timer; past this, the console
/// reports it as not running rather than letting a request sit forever.
pub const AGENT_STALE_SECS: i64 = 30 * 60;

/// Published tags are cached: the console polls this screen, and GHCR's token
/// endpoint is rate limited.
const TAG_CACHE_TTL_SECS: i64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub requested_tag: String,
    pub requested_by: String,
    pub requested_at: i64,
    /// Distinguishes two requests for the same tag, so the console can tell
    /// "my request was handled" from "a previous one was".
    pub nonce: String,
}

/// Written by the host agent when it finishes. `status` is one of `ok`,
/// `rolled-back`, `rejected`, or `failed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateResult {
    pub status: String,
    pub requested_tag: Option<String>,
    pub previous_tag: Option<String>,
    pub finished_at: i64,
    pub nonce: Option<String>,
    pub detail: Option<String>,
    /// Tail of the agent's log, so a failure can be read without SSH.
    pub log: Option<String>,
}

/// The host agent's heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub at: i64,
    pub version: Option<String>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn path_in(config_dir: &str, file: &str) -> PathBuf {
    Path::new(config_dir).join(file)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Option<T> {
    let text = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str(&text) {
        Ok(value) => Some(value),
        Err(e) => {
            warn!("Update: could not parse {path:?}: {e}");
            None
        }
    }
}

pub fn pending_request(config_dir: &str) -> Option<UpdateRequest> {
    read_json(path_in(config_dir, REQUEST_FILE))
}

pub fn last_result(config_dir: &str) -> Option<UpdateResult> {
    read_json(path_in(config_dir, RESULT_FILE))
}

pub fn agent_heartbeat(config_dir: &str) -> Option<AgentHeartbeat> {
    read_json(path_in(config_dir, AGENT_FILE))
}

/// Whether the host agent has checked in recently enough to be trusted to act.
pub fn agent_is_live(config_dir: &str) -> bool {
    agent_heartbeat(config_dir)
        .is_some_and(|beat| now_unix().saturating_sub(beat.at) < AGENT_STALE_SECS)
}

/// Ask the host agent to move the relay to `tag`.
///
/// Writing the file *is* the request; there is no other channel. The caller is
/// responsible for having checked that the tag exists and that the agent is
/// alive — a request nothing will read is worse than a refusal, because it looks
/// like it worked.
pub fn stage_request(config_dir: &str, tag: &str, requested_by: &str) -> Result<String, String> {
    let nonce = crate::admin::random_hex(16);
    let request = UpdateRequest {
        requested_tag: tag.to_string(),
        requested_by: requested_by.to_string(),
        requested_at: now_unix(),
        nonce: nonce.clone(),
    };

    let encoded =
        serde_json::to_string(&request).map_err(|e| format!("could not encode request: {e}"))?;

    // Write and rename: the agent watches this path, and must never see a
    // half-written file.
    let path = path_in(config_dir, REQUEST_FILE);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, encoded)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("could not write {path:?}: {e}"))?;

    Ok(nonce)
}

/// A tag is well-formed if Docker would accept it and the agent's allowlist
/// would too. Checked here so an obviously bad tag never reaches a root process.
pub fn tag_is_well_formed(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Whether a tag names something that can actually be run.
///
/// GHCR lists cosign signatures and attestations as tags alongside real images
/// — `sha256-<digest>.sig`, `.att` — and this repository has several. They are
/// well-formed tags that pull successfully and contain no relay, so offering one
/// in the version picker would let an operator update the relay into a
/// signature blob and rely on the rollback to save them.
fn is_runnable_tag(tag: &str) -> bool {
    !(tag.starts_with("sha256-") && (tag.ends_with(".sig") || tag.ends_with(".att")))
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishedTags {
    pub tags: Vec<String>,
    pub fetched_at: i64,
    /// Why the list is empty or stale, when it is. An empty list and an
    /// unreachable registry must not look the same to an operator.
    pub error: Option<String>,
}

static TAG_CACHE: once_cell::sync::OnceCell<parking_lot::RwLock<Option<PublishedTags>>> =
    once_cell::sync::OnceCell::new();

/// Tags published for [`IMAGE_REPOSITORY`], newest-looking first.
///
/// GHCR requires a token even for public packages, so this is two requests: an
/// anonymous pull token, then the tag list.
pub async fn published_tags(refresh: bool) -> PublishedTags {
    let cache = TAG_CACHE.get_or_init(|| parking_lot::RwLock::new(None));

    if !refresh {
        if let Some(cached) = cache.read().as_ref() {
            if now_unix().saturating_sub(cached.fetched_at) < TAG_CACHE_TTL_SECS {
                return cached.clone();
            }
        }
    }

    let fetched = fetch_tags().await;

    // A failed fetch must not discard a good list: the console would go from
    // showing tags to showing none, which reads as "nothing published".
    let result = match (fetched, cache.read().clone()) {
        (Ok(tags), _) => PublishedTags {
            tags,
            fetched_at: now_unix(),
            error: None,
        },
        (Err(e), Some(previous)) if !previous.tags.is_empty() => PublishedTags {
            tags: previous.tags,
            fetched_at: previous.fetched_at,
            error: Some(e),
        },
        (Err(e), _) => PublishedTags {
            tags: Vec::new(),
            fetched_at: now_unix(),
            error: Some(e),
        },
    };

    *cache.write() = Some(result.clone());
    result
}

async fn fetch_tags() -> Result<Vec<String>, String> {
    // `ghcr.io/owner/name` -> `owner/name`
    let repository = IMAGE_REPOSITORY
        .strip_prefix("ghcr.io/")
        .ok_or_else(|| format!("{IMAGE_REPOSITORY} is not a ghcr.io reference"))?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("could not build an HTTP client: {e}"))?;

    #[derive(Deserialize)]
    struct TokenResponse {
        token: String,
    }

    let token: TokenResponse = client
        .get(format!(
            "https://ghcr.io/token?scope=repository:{repository}:pull&service=ghcr.io"
        ))
        .send()
        .await
        .map_err(|e| format!("could not reach ghcr.io: {e}"))?
        .error_for_status()
        .map_err(|e| format!("ghcr.io refused a pull token: {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not read the pull token: {e}"))?;

    #[derive(Deserialize)]
    struct TagList {
        #[serde(default)]
        tags: Vec<String>,
    }

    let list: TagList = client
        .get(format!("https://ghcr.io/v2/{repository}/tags/list?n=200"))
        .bearer_auth(token.token)
        .send()
        .await
        .map_err(|e| format!("could not list tags: {e}"))?
        .error_for_status()
        .map_err(|e| format!("ghcr.io refused the tag list: {e}"))?
        .json()
        .await
        .map_err(|e| format!("could not read the tag list: {e}"))?;

    let mut tags: Vec<String> = list
        .tags
        .into_iter()
        .filter(|t| tag_is_well_formed(t) && is_runnable_tag(t))
        .collect();

    // The registry returns tags in lexical order. These are dated
    // (`v2026.09.15-observability`), so reversing puts the newest first, and
    // that is the one an operator almost always wants.
    tags.sort();
    tags.reverse();

    debug!(
        "Update: {} tags published for {IMAGE_REPOSITORY}",
        tags.len()
    );
    Ok(tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_tags_are_accepted() {
        assert!(tag_is_well_formed("v2026.09.15-observability"));
        assert!(tag_is_well_formed("latest"));
        assert!(tag_is_well_formed("sha-5c154eb"));
        assert!(tag_is_well_formed("1"));
    }

    #[test]
    fn a_tag_can_never_smuggle_a_registry_or_a_shell() {
        // The agent runs as root and the request file is written by a
        // network-facing service, so these are the cases that matter.
        assert!(!tag_is_well_formed(""));
        assert!(!tag_is_well_formed("-rf"), "must not look like a flag");
        assert!(!tag_is_well_formed("v1;rm -rf /"));
        assert!(!tag_is_well_formed("v1 --privileged"));
        assert!(!tag_is_well_formed("evil.io/owner/img:tag"));
        assert!(!tag_is_well_formed("v1$(id)"));
        assert!(!tag_is_well_formed("v1`id`"));
        assert!(!tag_is_well_formed("../../etc/passwd"));
        assert!(!tag_is_well_formed(&"a".repeat(129)));
    }

    #[test]
    fn signature_artifacts_are_not_offered_as_versions() {
        // Real entries from this repository's tag list.
        assert!(!is_runnable_tag(
            "sha256-dea17816a40062360ed19e88b2b414d4ad06c8606d62a2a1f396f2dbbff06bd4.sig"
        ));
        assert!(!is_runnable_tag("sha256-abc.att"));
        assert!(is_runnable_tag("v2026.09.15-observability"));
        assert!(is_runnable_tag("sha-e8888d3"));
    }

    #[test]
    fn a_request_round_trips_through_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().to_str().unwrap().to_string();

        assert!(pending_request(&config_dir).is_none());

        let nonce =
            stage_request(&config_dir, "v2026.09.15-observability", "abc123").expect("stage");
        let request = pending_request(&config_dir).expect("request");

        assert_eq!(request.requested_tag, "v2026.09.15-observability");
        assert_eq!(request.requested_by, "abc123");
        assert_eq!(request.nonce, nonce);
        assert!(!dir.path().join("update-request.json.tmp").exists());
    }

    #[test]
    fn an_agent_that_has_not_checked_in_is_not_live() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().to_str().unwrap().to_string();

        assert!(!agent_is_live(&config_dir), "no heartbeat at all");

        let stale = AgentHeartbeat {
            at: now_unix() - AGENT_STALE_SECS - 1,
            version: None,
        };
        std::fs::write(
            dir.path().join(AGENT_FILE),
            serde_json::to_string(&stale).unwrap(),
        )
        .unwrap();
        assert!(!agent_is_live(&config_dir), "heartbeat too old");

        let fresh = AgentHeartbeat {
            at: now_unix(),
            version: Some("1".to_string()),
        };
        std::fs::write(
            dir.path().join(AGENT_FILE),
            serde_json::to_string(&fresh).unwrap(),
        )
        .unwrap();
        assert!(agent_is_live(&config_dir));
    }

    #[test]
    fn an_unreadable_file_reads_as_absent_rather_than_panicking() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_dir = dir.path().to_str().unwrap().to_string();
        std::fs::write(dir.path().join(RESULT_FILE), b"{not json").unwrap();

        assert!(last_result(&config_dir).is_none());
    }
}
