use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use nostr::event::borrow::EventBorrow;
use nostr_database::flatbuffers::FlatBufferDecodeBorrowed;
use serde_json::Value;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    group_id: String,
    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let data = fs::read(&args.input)
        .with_context(|| format!("failed to read {}", args.input.display()))?;
    let mut seen = HashSet::new();
    let mut recovered = BTreeMap::new();
    let mut decoded = 0usize;
    let mut valid = 0usize;
    let mut kind9 = 0usize;

    for offset in 0..data.len().saturating_sub(4) {
        let root = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        if !(4..=512).contains(&root) {
            continue;
        }
        let Some(table) = offset.checked_add(root) else {
            continue;
        };
        if table + 4 > data.len() {
            continue;
        }
        let vtable_back = i32::from_le_bytes(data[table..table + 4].try_into().unwrap());
        if !(4..=512).contains(&vtable_back) {
            continue;
        }
        let Some(vtable) = table.checked_sub(vtable_back as usize) else {
            continue;
        };
        if vtable + 4 > data.len() {
            continue;
        }
        let vtable_len = u16::from_le_bytes(data[vtable..vtable + 2].try_into().unwrap());
        let object_len = u16::from_le_bytes(data[vtable + 2..vtable + 4].try_into().unwrap());
        if !(4..=128).contains(&vtable_len) || !(4..=256).contains(&object_len) {
            continue;
        }

        let borrowed = match EventBorrow::decode(&data[offset..]) {
            Ok(event) => event,
            Err(_) => continue,
        };
        decoded += 1;
        let event = borrowed.into_owned();
        if event.verify().is_err() || !seen.insert(event.id) {
            continue;
        }
        valid += 1;
        if event.kind.as_u16() != 9 {
            continue;
        }
        kind9 += 1;

        let value = serde_json::to_value(&event)?;
        let matches_group = args.group_id == "*"
            || value
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| {
                    tags.iter().any(|tag| {
                        tag.as_array().is_some_and(|parts| {
                            parts.first().and_then(Value::as_str) == Some("h")
                                && parts.get(1).and_then(Value::as_str)
                                    == Some(args.group_id.as_str())
                        })
                    })
                });
        if matches_group {
            recovered.insert(event.id.to_string(), serde_json::to_string(&event)?);
        }
    }

    let output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&args.output)
        .with_context(|| format!("failed to create {}", args.output.display()))?;
    let mut writer = BufWriter::new(output);
    for event in recovered.values() {
        writeln!(writer, "{event}")?;
    }
    writer.flush()?;

    println!(
        "decoded={decoded} valid_unique={valid} kind9_unique={kind9} recovered={} output={}",
        recovered.len(),
        args.output.display()
    );
    Ok(())
}
