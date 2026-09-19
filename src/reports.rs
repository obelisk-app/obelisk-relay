//! NIP-56 moderation reports (kind 1984).
//!
//! Before this, a kind 1984 event had nowhere to go. It carries no `h` tag, so
//! `ValidationMiddleware` refused it outright ("group events must contain an 'h'
//! tag") unless a client invented one — and a client that did got its report
//! filed as ordinary group content, indistinguishable from a chat message. The
//! only trace of the kind anywhere in the codebase was a label in the storage
//! screen's pruning list. So reports could not be made, and if made could not be
//! found.
//!
//! Three things make them manageable, and they are separable on purpose:
//!
//! * **Parsing.** A 1984 is a loose shape — `p` and `e` tags with an optional
//!   type in the third position, a free-text `content`, and per NIP-56 possibly
//!   several targets in one event. `Report::parse` turns that into something a
//!   queue can sort.
//! * **Grouping.** Ten people reporting one message is one decision, not ten.
//!   Reports collapse by target, so the queue reflects work to do rather than
//!   volume received.
//! * **Resolution.** Whether a target has been dealt with is relay state, not a
//!   property of anybody's event — the reporter does not decide whether their
//!   report was acted on. It lives in a sidecar file next to the blacklist.
//!
//! Deliberately *not* a trust system. A report is a claim by one pubkey about
//! another; this module records and groups claims so an admin can judge them.
//! Nothing here treats a report count as evidence.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::Error;

const REPORTS_FILE: &str = "reports_state.json";

/// NIP-56 kind for a moderation report.
pub const KIND_REPORT_1984: Kind = Kind::Custom(1984);

/// The reason given for a report.
///
/// NIP-56 names seven; anything else is preserved verbatim under `Other` rather
/// than discarded, because a type this relay does not know is still information
/// the admin reading the queue might want.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportType {
    Nudity,
    Malware,
    Profanity,
    Illegal,
    Spam,
    Impersonation,
    Other(String),
}

impl ReportType {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "nudity" => Self::Nudity,
            "malware" => Self::Malware,
            "profanity" => Self::Profanity,
            "illegal" => Self::Illegal,
            "spam" => Self::Spam,
            "impersonation" => Self::Impersonation,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Nudity => "nudity",
            Self::Malware => "malware",
            Self::Profanity => "profanity",
            Self::Illegal => "illegal",
            Self::Spam => "spam",
            Self::Impersonation => "impersonation",
            Self::Other(s) => s,
        }
    }
}

/// What a report points at.
///
/// A single 1984 may name both an event and a pubkey; NIP-56 treats the `e` tag
/// as the more specific claim, so a report naming both is filed against the
/// event and the pubkey is kept as context.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReportTarget {
    Event { id: String },
    Pubkey { hex: String },
}

impl ReportTarget {
    /// Stable key for grouping and for the resolution file.
    pub fn key(&self) -> String {
        match self {
            Self::Event { id } => format!("e:{id}"),
            Self::Pubkey { hex } => format!("p:{hex}"),
        }
    }
}

/// One parsed 1984 event.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// The 1984 event's own id, so an admin can find the original.
    pub report_id: String,
    pub reporter: String,
    pub target: ReportTarget,
    pub report_type: ReportType,
    /// The reporter's free text. Untrusted: rendered as text, never as markup.
    pub content: String,
    pub created_at: u64,
    /// The pubkey behind a reported event, when the report names both.
    pub reported_pubkey: Option<String>,
}

impl Report {
    /// Pull every target out of one 1984 event.
    ///
    /// Returns a Vec because NIP-56 permits several `p`/`e` tags in a single
    /// report, and each is a separate thing an admin might act on. An event with
    /// no usable target yields an empty Vec rather than an error — a malformed
    /// report is not worth refusing, it is worth ignoring.
    pub fn parse(event: &Event) -> Vec<Self> {
        if event.kind != KIND_REPORT_1984 {
            return Vec::new();
        }

        let reporter = event.pubkey.to_hex();
        let created_at = event.created_at.as_secs();
        let content = event.content.clone();

        // A `p` tag alongside an `e` tag is the author of the reported event,
        // not a separate accusation. Captured as context for the event report.
        let pubkey_tags: Vec<(String, Option<String>)> = event
            .tags
            .iter()
            .filter_map(|t| {
                let v = t.as_slice();
                (v.first().map(String::as_str) == Some("p") && v.len() >= 2)
                    .then(|| (v[1].clone(), v.get(2).cloned()))
            })
            .collect();

        let event_tags: Vec<(String, Option<String>)> = event
            .tags
            .iter()
            .filter_map(|t| {
                let v = t.as_slice();
                (v.first().map(String::as_str) == Some("e") && v.len() >= 2)
                    .then(|| (v[1].clone(), v.get(2).cloned()))
            })
            .collect();

        let context_pubkey = pubkey_tags.first().map(|(hex, _)| hex.clone());
        let mut reports = Vec::new();

        for (id, ty) in &event_tags {
            reports.push(Self {
                report_id: event.id.to_hex(),
                reporter: reporter.clone(),
                target: ReportTarget::Event { id: id.clone() },
                report_type: ty
                    .as_deref()
                    .map(ReportType::parse)
                    .unwrap_or_else(|| ReportType::Other("unspecified".into())),
                content: content.clone(),
                created_at,
                reported_pubkey: context_pubkey.clone(),
            });
        }

        // Only treat a `p` tag as its own target when the report is *about* the
        // person -- otherwise every event report would also open a case against
        // its author, which is a different and much heavier accusation.
        if event_tags.is_empty() {
            for (hex, ty) in &pubkey_tags {
                reports.push(Self {
                    report_id: event.id.to_hex(),
                    reporter: reporter.clone(),
                    target: ReportTarget::Pubkey { hex: hex.clone() },
                    report_type: ty
                        .as_deref()
                        .map(ReportType::parse)
                        .unwrap_or_else(|| ReportType::Other("unspecified".into())),
                    content: content.clone(),
                    created_at,
                    reported_pubkey: Some(hex.clone()),
                });
            }
        }

        reports
    }
}

/// What an admin did about a target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolutionAction {
    /// Judged a false positive, or otherwise not actionable.
    Dismissed,
    DeletedEvent,
    RemovedFromGroup,
    Blacklisted,
}

impl ResolutionAction {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "dismissed" | "dismiss" => Ok(Self::Dismissed),
            "deleted_event" | "delete_event" => Ok(Self::DeletedEvent),
            "removed_from_group" | "remove_from_group" => Ok(Self::RemovedFromGroup),
            "blacklisted" | "blacklist" => Ok(Self::Blacklisted),
            other => Err(Error::notice(format!("Unknown resolution action: {other}"))),
        }
    }
}

/// The record of a decision. Kept after the fact so the queue can show what was
/// already done and by whom -- a moderation log, not just a filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    pub action: ResolutionAction,
    /// Admin pubkey, hex.
    pub resolved_by: String,
    pub resolved_at: u64,
    /// Optional free text from the admin.
    #[serde(default)]
    pub note: String,
}

/// Resolution state, keyed by `ReportTarget::key()`.
///
/// Sidecar file rather than events, for the same reason the blacklist is: this
/// is the relay operator's decision about someone else's claim, and it must not
/// be something the reporter or the reported can publish, replace or delete.
#[derive(Debug, Clone)]
pub struct ReportsState {
    inner: Arc<RwLock<HashMap<String, Resolution>>>,
}

impl Default for ReportsState {
    fn default() -> Self {
        Self::new(None)
    }
}

impl ReportsState {
    pub fn new(config_dir: Option<&Path>) -> Self {
        let mut map = HashMap::new();

        if let Some(dir) = config_dir {
            let path = dir.join(REPORTS_FILE);
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => {
                        match serde_json::from_str::<HashMap<String, Resolution>>(&contents) {
                            Ok(loaded) => {
                                info!("Loaded {} report resolutions", loaded.len());
                                map = loaded;
                            }
                            // A corrupt file must not take the relay down or, worse,
                            // silently reopen every case that was already judged.
                            Err(e) => warn!("Failed to parse {}: {}", path.display(), e),
                        }
                    }
                    Err(e) => warn!("Failed to read {}: {}", path.display(), e),
                }
            }
        }

        Self {
            inner: Arc::new(RwLock::new(map)),
        }
    }

    pub fn get(&self, target: &ReportTarget) -> Option<Resolution> {
        self.inner.read().get(&target.key()).cloned()
    }

    pub fn is_resolved(&self, target: &ReportTarget) -> bool {
        self.inner.read().contains_key(&target.key())
    }

    pub fn resolve(&self, target: &ReportTarget, resolution: Resolution) {
        self.inner.write().insert(target.key(), resolution);
    }

    /// Reopen a target, for when a decision turns out to be wrong.
    pub fn reopen(&self, target: &ReportTarget) -> bool {
        self.inner.write().remove(&target.key()).is_some()
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    pub fn persist(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let path = config_dir.join(REPORTS_FILE);
        let snapshot = self.inner.read().clone();
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(path, json)
    }
}

/// One row of the moderation queue: everything reported about a single target.
#[derive(Debug, Clone, Serialize)]
pub struct ReportCase {
    pub target: ReportTarget,
    /// Grouping key, so the client can round-trip a resolution without
    /// reconstructing it.
    pub key: String,
    pub reports: Vec<Report>,
    /// Distinct reporters. Ten reports from one pubkey is not ten opinions.
    pub reporter_count: usize,
    pub types: Vec<String>,
    /// Most recent report, for sorting.
    pub last_reported_at: u64,
    pub resolution: Option<Resolution>,
    /// The reported event's content, when the target is an event and it is still
    /// stored. An admin cannot judge a report without seeing what was reported.
    pub reported_content: Option<String>,
    pub reported_pubkey: Option<String>,
    /// Group the reported event belongs to, derived from its `h` tag. Drives
    /// which actions apply: without it, "remove from group" is meaningless.
    pub group_id: Option<String>,
}

/// Collapse individual reports into one case per target.
///
/// Sorted most-recently-reported first: a queue ordered by volume would let a
/// brigade set the agenda.
pub fn group_into_cases(reports: Vec<Report>, state: &ReportsState) -> Vec<ReportCase> {
    let mut by_target: HashMap<String, Vec<Report>> = HashMap::new();
    for report in reports {
        by_target
            .entry(report.target.key())
            .or_default()
            .push(report);
    }

    let mut cases: Vec<ReportCase> = by_target
        .into_iter()
        .map(|(key, reports)| {
            let target = reports[0].target.clone();

            let mut reporters: Vec<&str> = reports.iter().map(|r| r.reporter.as_str()).collect();
            reporters.sort_unstable();
            reporters.dedup();

            let mut types: Vec<String> = reports
                .iter()
                .map(|r| r.report_type.as_str().to_string())
                .collect();
            types.sort();
            types.dedup();

            let last_reported_at = reports.iter().map(|r| r.created_at).max().unwrap_or(0);
            let reported_pubkey = reports.iter().find_map(|r| r.reported_pubkey.clone());

            ReportCase {
                resolution: state.get(&target),
                target,
                key,
                reporter_count: reporters.len(),
                types,
                last_reported_at,
                reports,
                reported_content: None,
                reported_pubkey,
                group_id: None,
            }
        })
        .collect();

    cases.sort_by(|a, b| b.last_reported_at.cmp(&a.last_reported_at));
    cases
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn report_event(tags: Vec<Tag>, content: &str) -> Event {
        EventBuilder::new(KIND_REPORT_1984, content)
            .tags(tags)
            .sign(&Keys::generate())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_event_report_names_the_event_not_its_author() {
        let author = Keys::generate().public_key();
        let target = EventId::all_zeros();
        let event = report_event(
            vec![
                Tag::parse(["e", &target.to_hex(), "spam"]).unwrap(),
                Tag::parse(["p", &author.to_hex()]).unwrap(),
            ],
            "buying followers",
        )
        .await;

        let reports = Report::parse(&event);
        assert_eq!(reports.len(), 1, "one target, not one per tag");
        assert_eq!(
            reports[0].target,
            ReportTarget::Event {
                id: target.to_hex()
            },
            "a p tag next to an e tag is the author, not a second accusation"
        );
        assert_eq!(reports[0].report_type, ReportType::Spam);
        assert_eq!(reports[0].reported_pubkey, Some(author.to_hex()));
    }

    #[tokio::test]
    async fn a_bare_pubkey_report_targets_the_person() {
        let subject = Keys::generate().public_key();
        let event = report_event(
            vec![Tag::parse(["p", &subject.to_hex(), "impersonation"]).unwrap()],
            "pretending to be someone else",
        )
        .await;

        let reports = Report::parse(&event);
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].target,
            ReportTarget::Pubkey {
                hex: subject.to_hex()
            }
        );
        assert_eq!(reports[0].report_type, ReportType::Impersonation);
    }

    #[tokio::test]
    async fn an_unknown_report_type_is_preserved_not_dropped() {
        let event = report_event(
            vec![
                Tag::parse(["p", &Keys::generate().public_key().to_hex(), "ban-evasion"]).unwrap(),
            ],
            "",
        )
        .await;

        let reports = Report::parse(&event);
        assert_eq!(
            reports[0].report_type,
            ReportType::Other("ban-evasion".into()),
            "a type this relay does not know is still information"
        );
    }

    #[tokio::test]
    async fn a_report_with_no_target_is_ignored_not_an_error() {
        let event = report_event(vec![], "something bad happened").await;
        assert!(Report::parse(&event).is_empty());
    }

    #[tokio::test]
    async fn a_non_report_kind_yields_nothing() {
        let event = EventBuilder::new(Kind::Custom(9), "hello")
            .sign(&Keys::generate())
            .await
            .unwrap();
        assert!(Report::parse(&event).is_empty());
    }

    #[tokio::test]
    async fn several_reports_of_one_thing_are_one_case() {
        let target = EventId::all_zeros();
        let mut all = Vec::new();
        for _ in 0..5 {
            let e = report_event(
                vec![Tag::parse(["e", &target.to_hex(), "spam"]).unwrap()],
                "spam",
            )
            .await;
            all.extend(Report::parse(&e));
        }

        let cases = group_into_cases(all, &ReportsState::default());
        assert_eq!(cases.len(), 1, "one target is one decision");
        assert_eq!(cases[0].reports.len(), 5);
        assert_eq!(cases[0].reporter_count, 5);
    }

    #[tokio::test]
    async fn repeat_reports_from_one_pubkey_count_once() {
        // Otherwise a single determined user could push a target up a
        // volume-ordered queue on their own.
        let keys = Keys::generate();
        let target = EventId::all_zeros();
        let mut all = Vec::new();
        for i in 0..4 {
            let e = EventBuilder::new(KIND_REPORT_1984, format!("report {i}"))
                .tags(vec![Tag::parse(["e", &target.to_hex(), "spam"]).unwrap()])
                .sign(&keys)
                .await
                .unwrap();
            all.extend(Report::parse(&e));
        }

        let cases = group_into_cases(all, &ReportsState::default());
        assert_eq!(cases[0].reports.len(), 4);
        assert_eq!(
            cases[0].reporter_count, 1,
            "four reports from one pubkey is one reporter"
        );
    }

    #[test]
    fn resolving_a_target_takes_it_out_of_the_queue() {
        let state = ReportsState::default();
        let target = ReportTarget::Pubkey { hex: "abc".into() };
        assert!(!state.is_resolved(&target));

        state.resolve(
            &target,
            Resolution {
                action: ResolutionAction::Dismissed,
                resolved_by: "admin".into(),
                resolved_at: 1,
                note: "false positive".into(),
            },
        );

        assert!(state.is_resolved(&target));
        assert_eq!(
            state.get(&target).unwrap().action,
            ResolutionAction::Dismissed
        );
    }

    #[test]
    fn a_decision_can_be_reversed() {
        let state = ReportsState::default();
        let target = ReportTarget::Event { id: "def".into() };
        state.resolve(
            &target,
            Resolution {
                action: ResolutionAction::Blacklisted,
                resolved_by: "admin".into(),
                resolved_at: 1,
                note: String::new(),
            },
        );

        assert!(state.reopen(&target));
        assert!(!state.is_resolved(&target), "reopened targets requeue");
        assert!(!state.reopen(&target), "reopening twice is a no-op");
    }

    #[test]
    fn resolutions_survive_a_round_trip_through_disk() {
        let dir = std::env::temp_dir().join(format!("obelisk-reports-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let state = ReportsState::default();
        let target = ReportTarget::Event { id: "keep".into() };
        state.resolve(
            &target,
            Resolution {
                action: ResolutionAction::DeletedEvent,
                resolved_by: "admin".into(),
                resolved_at: 42,
                note: "removed".into(),
            },
        );
        state.persist(&dir).unwrap();

        let reloaded = ReportsState::new(Some(&dir));
        assert!(
            reloaded.is_resolved(&target),
            "a restart must not reopen decided cases"
        );
        assert_eq!(reloaded.get(&target).unwrap().resolved_at, 42);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_actions_are_refused() {
        assert!(ResolutionAction::parse("dismiss").is_ok());
        assert!(ResolutionAction::parse("blacklist").is_ok());
        assert!(ResolutionAction::parse("launch_the_missiles").is_err());
    }
}
