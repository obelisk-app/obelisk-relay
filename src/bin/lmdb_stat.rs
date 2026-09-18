//! Report how much of an LMDB file is live data and how much is free-list slack.
//!
//! This exists as a separate process on purpose. heed refuses a second open of a
//! path already open in the same process — and LMDB's documentation forbids it
//! outright — so the running relay cannot measure its own database. A short-lived
//! child process opening the same environment read-only is both legal and cheap,
//! which is how the admin API answers "how much would a compaction reclaim?"
//! without a restart.
//!
//!     lmdb_stat --db /app/db
//!     {"file_bytes":633888768,"live_bytes":214958080,"reclaimable_bytes":418930688,...}
//!
//! Exits non-zero with a plain-text reason on stderr if the database cannot be
//! read, so a caller can distinguish "no slack" from "could not measure".

use clap::Parser;
use groups_relay::compaction;

#[derive(Parser, Debug)]
#[command(
    name = "lmdb_stat",
    about = "Report LMDB file size, live pages, and reclaimable free-list slack"
)]
struct Args {
    /// Directory holding data.mdb.
    #[arg(long, default_value = "/app/db")]
    db: String,

    /// Also report free space on the filesystem holding the database.
    #[arg(long, default_value_t = true)]
    with_free_disk: bool,
}

fn main() {
    let args = Args::parse();

    let measurement = match compaction::measure(&args.db) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let free_disk = args
        .with_free_disk
        .then(|| compaction::free_disk_bytes(&args.db))
        .flatten();

    let report = serde_json::json!({
        "db_path": args.db,
        "file_bytes": measurement.file_bytes,
        "live_bytes": measurement.live_bytes,
        "reclaimable_bytes": measurement.reclaimable_bytes,
        "measured_at": measurement.measured_at,
        "required_free_bytes": compaction::required_free_bytes(&measurement),
        "free_disk_bytes": free_disk,
    });

    println!("{report}");
}
