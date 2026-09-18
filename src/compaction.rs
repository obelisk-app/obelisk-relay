//! Reclaiming the disk that pruning does not.
//!
//! Deleting events frees LMDB *pages*, not disk. Freed pages go on the
//! environment's free list and are reused by later writes, so `data.mdb` stays
//! at its historical high-water mark forever — a relay that pruned 58,032 gift
//! wraps still reports the same 5.2 GB it did before. The only way to give the
//! space back is `mdb_env_copy2(MDB_CP_COMPACT)`, which walks the live pages
//! into a fresh file and leaves the free list behind.
//!
//! Two constraints shape everything here.
//!
//! **The swap has to happen with the database closed.** The copy itself is safe
//! against a live relay — it runs inside a read transaction, so it sees a
//! consistent snapshot — but any write that lands *after* that snapshot would be
//! silently lost the moment the copy replaced the original. The one moment this
//! process is guaranteed to have nothing open is startup, before
//! `RelayDatabase::new`. So the admin endpoint only stages a request and
//! restarts; [`run_pending`] is what actually compacts.
//!
//! **We have to open LMDB ourselves.** `NostrLmdb` keeps its `Env` in a private
//! field and exposes no copy, backup or stat API, and `RelayDatabase` wraps it
//! without an escape hatch. Opening the environment directly with heed is the
//! same thing relay_builder's own `nostr-lmdb-dump` and `nostr-lmdb-integrity`
//! tools do.
//!
//! Measuring is a separate problem: heed refuses a second open of a path already
//! open in this process (and LMDB's own documentation forbids it), so the
//! running relay cannot measure its own free-list slack in-process. That is what
//! the `lmdb_stat` binary is for — a different process opening the same
//! environment read-only is both legal and cheap.

use std::path::{Path, PathBuf};

use heed::{CompactionOption, EnvOpenOptions};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Matches what nostr-lmdb opens with (`store/lmdb/mod.rs`): 32 GiB on 64-bit.
/// A copy only writes the pages actually in use, so this bounds nothing on disk.
const MAP_SIZE: usize = 32 * 1024 * 1024 * 1024;

/// nostr-lmdb uses `max_dbs(19 + additional)`. `non_free_pages_size` opens
/// *every* named database in order to stat it, so an undersized value here would
/// silently undercount live data — which would overstate what a compaction can
/// reclaim. Room to spare costs nothing but a few slots in the main database.
const MAX_DBS: u32 = 128;

const REQUEST_FILE: &str = "compaction-request.json";
const LOG_FILE: &str = "compaction-log.json";

/// Keep the log short enough to return whole in an API response.
const MAX_LOG_ENTRIES: usize = 20;

/// A compaction needs room for the live data, and the swap keeps the original
/// until the new file has been proven openable. 1.2x leaves a little headroom on
/// top of that for the log and the lock file.
const FREE_DISK_FACTOR: f64 = 1.2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    /// Size of `data.mdb` on disk, free-list slack included.
    pub file_bytes: u64,
    /// Bytes held by pages that are actually in use. `None` when the walk over
    /// named databases could not be completed — see [`measure`].
    pub live_bytes: Option<u64>,
    /// What a compaction would hand back, or `None` if `live_bytes` is unknown.
    pub reclaimable_bytes: Option<u64>,
    pub measured_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionRequest {
    pub requested_by: String,
    pub requested_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEntry {
    pub at: i64,
    /// `ok`, `failed`, or `refused`.
    pub status: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
    pub duration_ms: u64,
    pub requested_by: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionLog {
    pub entries: Vec<CompactionEntry>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn data_file(db_path: &str) -> PathBuf {
    Path::new(db_path).join("data.mdb")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Open the environment, hand it to `f`, and close it before returning.
///
/// The closing is not housekeeping — it is required for correctness. heed keeps
/// a process-wide registry of open environments keyed by path and hands back the
/// *existing* handle when the same path is opened again; a second open with
/// different options fails outright with "an environment is already opened with
/// different options". Leaving this environment registered would therefore leave
/// a handle onto a file [`compact_now`] is about to move aside.
/// `prepare_for_closing().wait()` blocks until it is genuinely gone.
fn with_env<T>(
    db_path: &str,
    f: impl FnOnce(&heed::Env) -> Result<T, String>,
) -> Result<T, String> {
    let dir = Path::new(db_path);
    if !data_file(db_path).exists() {
        return Err(format!("no LMDB database at {}/data.mdb", db_path));
    }

    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(MAP_SIZE)
            .max_dbs(MAX_DBS)
            .open(dir)
    }
    .map_err(|e| format!("could not open {db_path}: {e}"))?;

    let result = f(&env);
    env.prepare_for_closing().wait();
    result
}

/// Open the environment read-only and report how much of the file is live.
///
/// Callers must not already hold this environment open in the same process.
///
/// This goes to LMDB directly rather than using heed's `non_free_pages_size`,
/// which is unusable against a nostr-lmdb database. That function enumerates
/// databases by walking the unnamed database and decoding every key as a UTF-8
/// database name — but scoped-heed puts the *default scope's own records* in the
/// unnamed database, so the first 32-byte event id it meets panics on
/// `String::from_utf8(..).unwrap()`. Measured against a 605 MB production copy,
/// it panics immediately.
///
/// The free list is the honest source anyway: it is exactly the set of pages a
/// compaction would drop.
pub fn measure(db_path: &str) -> Result<Measurement, String> {
    let file_bytes = file_len(&data_file(db_path));
    let pages = read_page_usage(db_path)?;

    let live_bytes = pages
        .total_pages
        .saturating_sub(pages.free_pages)
        .saturating_mul(pages.page_size);

    Ok(Measurement {
        file_bytes,
        live_bytes: Some(live_bytes),
        reclaimable_bytes: Some(file_bytes.saturating_sub(live_bytes)),
        measured_at: now_unix(),
    })
}

struct PageUsage {
    page_size: u64,
    /// Pages the file spans, including the two meta pages.
    total_pages: u64,
    /// Pages on the free list, reusable by writes but never returned to the OS.
    free_pages: u64,
}

/// Walk LMDB's free list (`FREE_DBI`, database 0) and total the pages on it.
///
/// Each free-list record's value is an array of `MDB_ID` (pointer-sized): the
/// first element is a count, followed by that many page numbers. This is the
/// same arithmetic `mdb_stat -ef` prints, and the format has been stable across
/// LMDB's lifetime.
fn read_page_usage(db_path: &str) -> Result<PageUsage, String> {
    use lmdb_master_sys as ffi;

    let data = data_file(db_path);
    if !data.exists() {
        return Err(format!("no LMDB database at {}", data.display()));
    }
    let c_path =
        std::ffi::CString::new(db_path).map_err(|_| format!("{db_path} is not a usable path"))?;

    // Safety: every pointer below is either freshly created by LMDB or a valid
    // stack slot, and each resource is released on every exit path — the `close`
    // closure runs before each early return.
    unsafe {
        let mut env: *mut ffi::MDB_env = std::ptr::null_mut();
        if ffi::mdb_env_create(&mut env) != 0 {
            return Err("could not create an LMDB environment handle".to_string());
        }

        let close = |env: *mut ffi::MDB_env| ffi::mdb_env_close(env);

        // Matching nostr-lmdb's map size matters: opening with a smaller one
        // than the file needs fails outright.
        if ffi::mdb_env_set_mapsize(env, MAP_SIZE) != 0 {
            close(env);
            return Err("could not set the LMDB map size".to_string());
        }
        if ffi::mdb_env_set_maxdbs(env, MAX_DBS) != 0 {
            close(env);
            return Err("could not set the LMDB database limit".to_string());
        }

        // Read-only: this runs against live databases from `lmdb_stat`, and it
        // must not be able to modify one.
        // 0600: this creates lock.mdb if it is absent, and the relay's own files
        // are owner-only. A measurement must not leave a laxer file behind.
        let rc = ffi::mdb_env_open(env, c_path.as_ptr(), ffi::MDB_RDONLY, 0o600);
        if rc != 0 {
            close(env);
            return Err(format!("could not open {db_path}: LMDB error {rc}"));
        }

        let mut stat: ffi::MDB_stat = std::mem::zeroed();
        if ffi::mdb_env_stat(env, &mut stat) != 0 {
            close(env);
            return Err(format!("could not stat {db_path}"));
        }
        let mut info: ffi::MDB_envinfo = std::mem::zeroed();
        if ffi::mdb_env_info(env, &mut info) != 0 {
            close(env);
            return Err(format!("could not read environment info for {db_path}"));
        }

        let mut txn: *mut ffi::MDB_txn = std::ptr::null_mut();
        let rc = ffi::mdb_txn_begin(env, std::ptr::null_mut(), ffi::MDB_RDONLY, &mut txn);
        if rc != 0 {
            close(env);
            return Err(format!(
                "could not begin a read transaction: LMDB error {rc}"
            ));
        }

        let mut cursor: *mut ffi::MDB_cursor = std::ptr::null_mut();
        // FREE_DBI is database 0 and needs no open.
        let rc = ffi::mdb_cursor_open(txn, 0, &mut cursor);
        if rc != 0 {
            ffi::mdb_txn_abort(txn);
            close(env);
            return Err(format!("could not read the free list: LMDB error {rc}"));
        }

        let id_size = std::mem::size_of::<ffi::mdb_size_t>();
        let mut free_pages: u64 = 0;
        let mut key: ffi::MDB_val = std::mem::zeroed();
        let mut value: ffi::MDB_val = std::mem::zeroed();
        let mut op = ffi::MDB_FIRST;

        while ffi::mdb_cursor_get(cursor, &mut key, &mut value, op) == 0 {
            op = ffi::MDB_NEXT;
            if value.mv_size >= id_size && !value.mv_data.is_null() {
                let count = std::ptr::read_unaligned(value.mv_data as *const ffi::mdb_size_t);
                free_pages = free_pages.saturating_add(count as u64);
            }
        }

        ffi::mdb_cursor_close(cursor);
        ffi::mdb_txn_abort(txn);
        close(env);

        Ok(PageUsage {
            page_size: stat.ms_psize as u64,
            // me_last_pgno is the last page *id*, so the count is one more.
            total_pages: (info.me_last_pgno as u64).saturating_add(1),
            free_pages,
        })
    }
}

/// Free bytes on the filesystem holding `path`, or `None` if it cannot be read.
pub fn free_disk_bytes(path: &str) -> Option<u64> {
    let c_path = std::ffi::CString::new(path).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // Safety: `c_path` is a valid NUL-terminated string and `stat` is a
    // correctly sized, writable statvfs.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    // f_bavail is what an unprivileged writer can actually use; f_bfree includes
    // the root reserve, which the relay does not have.
    // Both fields are u64 on the platforms this builds for; left unconverted so
    // that a target where they are not stops the build rather than silently
    // truncating a size.
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// Bytes that must be free before a compaction is safe to start.
pub fn required_free_bytes(measurement: &Measurement) -> u64 {
    let live = measurement.live_bytes.unwrap_or(measurement.file_bytes);
    (live as f64 * FREE_DISK_FACTOR) as u64
}

fn request_path(config_dir: &str) -> PathBuf {
    Path::new(config_dir).join(REQUEST_FILE)
}

fn log_path(config_dir: &str) -> PathBuf {
    Path::new(config_dir).join(LOG_FILE)
}

/// Stage a compaction for the next startup. The caller is expected to restart
/// the process; this on its own does nothing.
pub fn stage_request(config_dir: &str, requested_by: &str) -> Result<(), String> {
    let request = CompactionRequest {
        requested_by: requested_by.to_string(),
        requested_at: now_unix(),
    };
    let encoded =
        serde_json::to_string(&request).map_err(|e| format!("could not encode request: {e}"))?;
    let path = request_path(config_dir);
    std::fs::write(&path, encoded).map_err(|e| format!("could not write {path:?}: {e}"))
}

/// Whether a compaction is already staged for the next start.
pub fn request_pending(config_dir: &str) -> bool {
    request_path(config_dir).exists()
}

pub fn load_log(config_dir: &str) -> CompactionLog {
    match std::fs::read_to_string(log_path(config_dir)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => CompactionLog::default(),
    }
}

/// Append one entry, newest last, trimmed to [`MAX_LOG_ENTRIES`].
///
/// Temp-write-then-rename, like `storage_history::record`: a crash mid-write
/// must not leave a half-written log that reads as corrupt forever.
fn append_log(config_dir: &str, entry: CompactionEntry) {
    let mut log = load_log(config_dir);
    log.entries.push(entry);
    if log.entries.len() > MAX_LOG_ENTRIES {
        let excess = log.entries.len() - MAX_LOG_ENTRIES;
        log.entries.drain(0..excess);
    }

    let path = log_path(config_dir);
    let tmp = path.with_extension("json.tmp");
    let encoded = match serde_json::to_string(&log) {
        Ok(text) => text,
        Err(e) => {
            warn!("Could not encode compaction log: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&tmp, encoded).and_then(|_| std::fs::rename(&tmp, &path)) {
        warn!("Could not persist compaction log to {path:?}: {e}");
    }
}

/// Run a staged compaction, if one is staged.
///
/// Call this **before** the relay opens its database and never after. Blocking
/// and synchronous on purpose: nothing else may touch the environment while it
/// runs, and startup is the only point where that is guaranteed.
pub fn run_pending(db_path: &str, config_dir: &str) {
    let path = request_path(config_dir);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };

    // Consume the request before doing any work. A compaction that kills the
    // process — OOM, a full disk mid-copy — must not be retried on every
    // restart, which would turn one bad run into a boot loop.
    if let Err(e) = std::fs::remove_file(&path) {
        warn!("Compaction: could not clear {path:?}, skipping to avoid a restart loop: {e}");
        return;
    }

    let requested_by = serde_json::from_str::<CompactionRequest>(&text)
        .ok()
        .map(|r| r.requested_by);

    info!("Compaction: request found, compacting {db_path} before opening the database");
    let started = std::time::Instant::now();
    let before_bytes = file_len(&data_file(db_path));

    let entry = match compact_now(db_path) {
        Ok(after_bytes) => {
            let reclaimed = before_bytes.saturating_sub(after_bytes);
            info!(
                "Compaction: {} -> {} bytes, {} reclaimed in {:?}",
                before_bytes,
                after_bytes,
                reclaimed,
                started.elapsed()
            );
            CompactionEntry {
                at: now_unix(),
                status: "ok".to_string(),
                before_bytes,
                after_bytes,
                duration_ms: started.elapsed().as_millis() as u64,
                requested_by,
                detail: None,
            }
        }
        Err(CompactError::Refused(detail)) => {
            warn!("Compaction refused: {detail}");
            CompactionEntry {
                at: now_unix(),
                status: "refused".to_string(),
                before_bytes,
                after_bytes: before_bytes,
                duration_ms: started.elapsed().as_millis() as u64,
                requested_by,
                detail: Some(detail),
            }
        }
        Err(CompactError::Failed(detail)) => {
            warn!("Compaction failed: {detail}");
            CompactionEntry {
                at: now_unix(),
                status: "failed".to_string(),
                before_bytes,
                after_bytes: file_len(&data_file(db_path)),
                duration_ms: started.elapsed().as_millis() as u64,
                requested_by,
                detail: Some(detail),
            }
        }
    };

    append_log(config_dir, entry);
}

#[derive(Debug)]
pub enum CompactError {
    /// Preconditions were not met; the database was not touched.
    Refused(String),
    /// The attempt started and did not finish; the original has been restored.
    Failed(String),
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactError::Refused(detail) => write!(f, "refused: {detail}"),
            CompactError::Failed(detail) => write!(f, "failed: {detail}"),
        }
    }
}

/// Compact `db_path` in place, returning the new file size.
///
/// Caller must guarantee that no process — including this one — has the
/// environment open.
pub fn compact_now(db_path: &str) -> Result<u64, CompactError> {
    let dir = Path::new(db_path);
    let live_file = data_file(db_path);
    let staged = dir.join("data.mdb.compacting");
    let backup = dir.join("data.mdb.pre-compact");

    if !live_file.exists() {
        return Err(CompactError::Refused(format!(
            "no LMDB database at {live_file:?}"
        )));
    }

    let measurement = measure(db_path).map_err(CompactError::Refused)?;
    let needed = required_free_bytes(&measurement);
    if let Some(free) = free_disk_bytes(db_path) {
        if free < needed {
            return Err(CompactError::Refused(format!(
                "needs {needed} bytes free to copy {} bytes of live data, but only {free} are available",
                measurement.live_bytes.unwrap_or(measurement.file_bytes)
            )));
        }
    } else {
        warn!("Compaction: could not read free space for {db_path}; proceeding without the check");
    }

    // Leftovers from an interrupted earlier attempt. The staged file is
    // incomplete by definition; the backup is not ours to judge, so refuse
    // rather than overwrite it.
    if staged.exists() {
        let _ = std::fs::remove_file(&staged);
    }
    if backup.exists() {
        return Err(CompactError::Refused(format!(
            "{backup:?} already exists — an earlier compaction did not finish. \
             Verify which file is current and remove it by hand."
        )));
    }

    // The database holds gift wraps and every other private message the relay
    // has ever stored. `copy_to_file` creates the new file with the process
    // umask, which on this image is more permissive than the 0600 LMDB itself
    // uses — so carry the original's mode across rather than widening it.
    let original_mode = std::fs::metadata(&live_file).ok().map(|m| {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode()
    });

    with_env(db_path, |env| {
        let file = env
            .copy_to_file(&staged, CompactionOption::Enabled)
            .map_err(|e| format!("copy failed: {e}"))?;

        if let Some(mode) = original_mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))
                .map_err(|e| format!("could not set permissions on the compacted copy: {e}"))?;
        }

        // The copy is only worth swapping in once it is durable.
        file.sync_all()
            .map_err(|e| format!("could not flush the compacted copy: {e}"))
    })
    .map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        CompactError::Failed(e)
    })?;

    std::fs::rename(&live_file, &backup)
        .map_err(|e| CompactError::Failed(format!("could not set the original aside: {e}")))?;

    if let Err(e) = std::fs::rename(&staged, &live_file) {
        let _ = std::fs::rename(&backup, &live_file);
        return Err(CompactError::Failed(format!(
            "could not move the compacted copy into place, original restored: {e}"
        )));
    }

    // The reader table in lock.mdb describes the file we just replaced. LMDB
    // recreates it on the next open; leaving the stale one risks readers being
    // matched against transactions that no longer exist.
    let _ = std::fs::remove_file(dir.join("lock.mdb"));

    // Prove the result opens before discarding the only copy of the original.
    if let Err(e) = measure(db_path) {
        let _ = std::fs::remove_file(&live_file);
        let _ = std::fs::rename(&backup, &live_file);
        return Err(CompactError::Failed(format!(
            "the compacted database would not open, original restored: {e}"
        )));
    }

    let after = file_len(&live_file);
    if let Err(e) = std::fs::remove_file(&backup) {
        warn!("Compaction: succeeded but could not remove {backup:?}: {e}");
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a test environment with the same options the module uses, so heed's
    /// per-path registry never sees a conflicting set, and close it fully before
    /// returning.
    fn with_test_env(dir: &Path, f: impl FnOnce(&heed::Env)) {
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(MAP_SIZE)
                .max_dbs(MAX_DBS)
                .open(dir)
        }
        .expect("open");
        f(&env);
        env.prepare_for_closing().wait();
    }

    type TestDb = heed::Database<heed::types::Str, heed::types::Bytes>;

    fn file_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777
    }

    /// A minimal LMDB environment with enough data to have pages to count.
    fn seed_env(dir: &Path, entries: usize) {
        with_test_env(dir, |env| {
            let mut txn = env.write_txn().expect("write txn");
            let db: TestDb = env
                .create_database(&mut txn, Some("events"))
                .expect("create");
            let payload = vec![7u8; 4096];
            for i in 0..entries {
                db.put(&mut txn, &format!("key-{i:08}"), &payload)
                    .expect("put");
            }
            txn.commit().expect("commit");
        });
    }

    #[test]
    fn measure_reports_a_file_and_its_live_pages() {
        let dir = tempfile::tempdir().expect("tempdir");
        seed_env(dir.path(), 200);

        let m = measure(dir.path().to_str().unwrap()).expect("measure");
        assert!(m.file_bytes > 0, "an existing database has a size");
        let live = m
            .live_bytes
            .expect("named databases are plain strings here");
        assert!(live > 0 && live <= m.file_bytes);
    }

    #[test]
    fn measure_rejects_a_directory_with_no_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = measure(dir.path().to_str().unwrap()).unwrap_err();
        assert!(err.contains("no LMDB database"), "{err}");
    }

    #[test]
    fn compaction_reclaims_deleted_pages_and_keeps_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().unwrap().to_string();
        seed_env(dir.path(), 400);

        // Delete most of it. The file will not shrink on its own — that is the
        // whole premise of this module.
        with_test_env(dir.path(), |env| {
            let mut txn = env.write_txn().expect("write txn");
            let db: TestDb = env
                .open_database(&txn, Some("events"))
                .expect("open db")
                .expect("db exists");
            for i in 0..350 {
                db.delete(&mut txn, &format!("key-{i:08}")).expect("delete");
            }
            txn.commit().expect("commit");
        });

        let before = measure(&path).expect("measure before");
        let after_bytes = compact_now(&path).expect("compact");

        assert!(
            after_bytes < before.file_bytes,
            "compaction should shrink the file: {} -> {after_bytes}",
            before.file_bytes
        );
        assert!(!dir.path().join("data.mdb.pre-compact").exists());
        assert!(!dir.path().join("data.mdb.compacting").exists());

        // The surviving 50 entries must still be there and still be readable.
        with_test_env(dir.path(), |env| {
            let txn = env.read_txn().expect("read txn");
            let db: TestDb = env
                .open_database(&txn, Some("events"))
                .expect("open db")
                .expect("db exists");
            assert_eq!(db.len(&txn).expect("len"), 50);
            assert_eq!(
                db.get(&txn, "key-00000399").expect("get").map(|v| v.len()),
                Some(4096)
            );
        });
    }

    #[test]
    fn a_leftover_backup_blocks_compaction_rather_than_being_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_str().unwrap().to_string();
        seed_env(dir.path(), 20);
        std::fs::write(dir.path().join("data.mdb.pre-compact"), b"older database")
            .expect("write backup");

        match compact_now(&path) {
            Err(CompactError::Refused(detail)) => {
                assert!(detail.contains("did not finish"), "{detail}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_request_is_consumed_even_when_there_is_nothing_to_compact() {
        let config = tempfile::tempdir().expect("tempdir");
        let db = tempfile::tempdir().expect("tempdir");
        let config_dir = config.path().to_str().unwrap().to_string();

        stage_request(&config_dir, "test-admin").expect("stage");
        assert!(config.path().join(REQUEST_FILE).exists());

        // No database here at all, so the run refuses — but the request must
        // still be gone, or every restart would retry it forever.
        run_pending(db.path().to_str().unwrap(), &config_dir);

        assert!(!config.path().join(REQUEST_FILE).exists());
        let log = load_log(&config_dir);
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].status, "refused");
        assert_eq!(log.entries[0].requested_by.as_deref(), Some("test-admin"));
    }

    #[test]
    fn run_pending_does_nothing_without_a_request() {
        let config = tempfile::tempdir().expect("tempdir");
        let db = tempfile::tempdir().expect("tempdir");
        let config_dir = config.path().to_str().unwrap().to_string();

        run_pending(db.path().to_str().unwrap(), &config_dir);

        assert!(load_log(&config_dir).entries.is_empty());
    }

    #[test]
    fn the_log_stays_bounded() {
        let config = tempfile::tempdir().expect("tempdir");
        let config_dir = config.path().to_str().unwrap().to_string();

        for i in 0..MAX_LOG_ENTRIES + 5 {
            append_log(
                &config_dir,
                CompactionEntry {
                    at: i as i64,
                    status: "ok".to_string(),
                    before_bytes: 100,
                    after_bytes: 50,
                    duration_ms: 1,
                    requested_by: None,
                    detail: None,
                },
            );
        }

        let log = load_log(&config_dir);
        assert_eq!(log.entries.len(), MAX_LOG_ENTRIES);
        assert_eq!(log.entries[0].at, 5, "the oldest entries are dropped");
    }

    /// Compact a real database, for when the synthetic ones are not convincing.
    ///
    /// Ignored by default because it needs a database to point at. Give it a
    /// *copy* — it rewrites what it is given:
    ///
    /// ```text
    /// cp -a /var/lib/docker/volumes/…-relay-db/_data /tmp/dbtest
    /// OBELISK_COMPACT_TEST_DB=/tmp/dbtest cargo test --lib compacts_a_real_database -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs OBELISK_COMPACT_TEST_DB pointing at a copy of a real database"]
    fn compacts_a_real_database() {
        let path = std::env::var("OBELISK_COMPACT_TEST_DB")
            .expect("set OBELISK_COMPACT_TEST_DB to a copy of a real database");

        let mode_before = file_mode(&data_file(&path));
        let before = measure(&path).expect("measure before");
        println!(
            "before: file={} live={} reclaimable={}",
            before.file_bytes,
            before.live_bytes.unwrap_or(0),
            before.reclaimable_bytes.unwrap_or(0)
        );

        let started = std::time::Instant::now();
        let after_bytes = compact_now(&path).expect("compact");
        let after = measure(&path).expect("measure after");
        println!(
            "after: file={} live={} reclaimable={} in {:?}",
            after_bytes,
            after.live_bytes.unwrap_or(0),
            after.reclaimable_bytes.unwrap_or(0),
            started.elapsed()
        );

        assert!(after_bytes < before.file_bytes, "the file should shrink");
        assert_eq!(
            file_mode(&data_file(&path)),
            mode_before,
            "a database of gift wraps must not come back more readable than it went in"
        );
        // Live data is what survives; the file should now be about that size.
        let live_before = before.live_bytes.expect("live bytes before");
        let live_after = after.live_bytes.expect("live bytes after");
        assert!(
            live_after <= live_before,
            "compaction must not invent data: {live_before} -> {live_after}"
        );
        assert!(
            after_bytes < live_before * 2,
            "the compacted file should be close to the live size"
        );
    }

    #[test]
    fn required_free_space_covers_the_live_data_with_headroom() {
        let m = Measurement {
            file_bytes: 1000,
            live_bytes: Some(400),
            reclaimable_bytes: Some(600),
            measured_at: 0,
        };
        assert_eq!(required_free_bytes(&m), 480);

        // With no live figure, assume the worst: the whole file is live.
        let unknown = Measurement {
            file_bytes: 1000,
            live_bytes: None,
            reclaimable_bytes: None,
            measured_at: 0,
        };
        assert_eq!(required_free_bytes(&unknown), 1200);
    }
}
