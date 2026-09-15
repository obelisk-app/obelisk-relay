//! A small, bounded time series of how much disk the relay is using.
//!
//! The database reached 5.2GB before anyone noticed, and by then the only
//! numbers available were the current size and a guess. There was no way to
//! answer "when did this start" or "how fast is it growing", which is exactly
//! what you need to decide whether a retention window is working.
//!
//! This records one sample per tick and keeps the last `MAX_SAMPLES`. It is
//! deliberately a flat JSON file rather than a metrics backend: the relay
//! already exports Prometheus counters, but nothing here scrapes them, and a
//! graph that only works when someone has stood up a monitoring stack is a
//! graph nobody looks at. A file survives restarts, needs no dependency, and is
//! bounded so it cannot become the next thing that fills a disk.
//!
//! Sizes come from the LMDB file itself, not from summing event sizes, so it
//! reflects what the filesystem actually holds — including the free-list slack
//! that a compaction reclaims. That slack is the interesting part: a flat event
//! count next to a rising file size is the signature of a database that needs
//! rebuilding rather than pruning.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Roughly six months at the default six-hour cadence. Each sample is a few
/// dozen bytes, so the whole file stays well under 100KB.
const MAX_SAMPLES: usize = 720;

const FILE_NAME: &str = "storage-history.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSample {
    /// Unix seconds.
    pub at: i64,
    /// Size of the LMDB data file, including free-list slack.
    pub db_bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageHistory {
    pub samples: Vec<StorageSample>,
}

fn path_in(config_dir: &str) -> PathBuf {
    Path::new(config_dir).join(FILE_NAME)
}

/// Read the series, or an empty one. A corrupt or unreadable file is not worth
/// failing a request over — the history is advisory, so start a fresh one.
pub fn load(config_dir: &str) -> StorageHistory {
    let path = path_in(config_dir);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!("Storage history at {path:?} is unreadable, starting fresh: {e}");
            StorageHistory::default()
        }),
        Err(_) => StorageHistory::default(),
    }
}

/// Append one sample and trim to `MAX_SAMPLES`, keeping the newest.
///
/// Writes through a temporary file and renames, so a crash mid-write cannot
/// leave a half-written series behind.
pub fn record(config_dir: &str, db_bytes: u64, at: i64) {
    let mut history = load(config_dir);
    history.samples.push(StorageSample { at, db_bytes });

    if history.samples.len() > MAX_SAMPLES {
        let excess = history.samples.len() - MAX_SAMPLES;
        history.samples.drain(0..excess);
    }

    let path = path_in(config_dir);
    let tmp = path.with_extension("json.tmp");
    let encoded = match serde_json::to_string(&history) {
        Ok(text) => text,
        Err(e) => {
            warn!("Could not encode storage history: {e}");
            return;
        }
    };

    if let Err(e) = std::fs::write(&tmp, encoded).and_then(|_| std::fs::rename(&tmp, &path)) {
        warn!("Could not persist storage history to {path:?}: {e}");
        return;
    }

    debug!(
        "Storage history: recorded {} bytes ({} samples retained)",
        db_bytes,
        history.samples.len()
    );
}

/// Total bytes of the LMDB files in `db_path`.
pub fn measure_db_bytes(db_path: &str) -> u64 {
    let dir = Path::new(db_path);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("mdb"))
        })
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> String {
        let dir =
            std::env::temp_dir().join(format!("obelisk-history-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(FILE_NAME));
        dir.to_string_lossy().into_owned()
    }

    #[test]
    fn an_absent_file_reads_as_an_empty_series() {
        let dir = temp_dir("absent");
        assert!(load(&dir).samples.is_empty());
    }

    #[test]
    fn samples_round_trip_and_keep_their_order() {
        let dir = temp_dir("roundtrip");
        record(&dir, 100, 1_000);
        record(&dir, 200, 2_000);
        let got = load(&dir);
        assert_eq!(got.samples.len(), 2);
        assert_eq!(got.samples[0].db_bytes, 100);
        assert_eq!(got.samples[1].db_bytes, 200);
    }

    #[test]
    fn the_series_is_bounded_and_drops_the_oldest() {
        let dir = temp_dir("bounded");
        for i in 0..(MAX_SAMPLES + 25) {
            record(&dir, i as u64, i as i64);
        }
        let got = load(&dir);
        assert_eq!(
            got.samples.len(),
            MAX_SAMPLES,
            "must not grow without bound"
        );
        // The newest are kept, so the oldest surviving sample is the 25th.
        assert_eq!(got.samples[0].db_bytes, 25);
        assert_eq!(
            got.samples.last().unwrap().db_bytes,
            (MAX_SAMPLES + 24) as u64
        );
    }

    #[test]
    fn a_corrupt_file_does_not_take_the_endpoint_down() {
        let dir = temp_dir("corrupt");
        std::fs::write(path_in(&dir), "{ this is not json").unwrap();
        assert!(load(&dir).samples.is_empty());
        // And it recovers: the next record starts a clean series.
        record(&dir, 42, 1);
        assert_eq!(load(&dir).samples.len(), 1);
    }

    #[test]
    fn measuring_a_missing_directory_is_zero_not_a_panic() {
        assert_eq!(measure_db_bytes("/nonexistent/path/for/test"), 0);
    }

    #[test]
    fn measure_sums_only_mdb_files() {
        let dir = temp_dir("measure");
        std::fs::write(Path::new(&dir).join("data.mdb"), vec![0u8; 500]).unwrap();
        std::fs::write(Path::new(&dir).join("lock.mdb"), vec![0u8; 20]).unwrap();
        std::fs::write(Path::new(&dir).join("notes.txt"), vec![0u8; 9_999]).unwrap();
        assert_eq!(measure_db_bytes(&dir), 520);
    }
}
