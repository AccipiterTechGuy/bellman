//! Tolerant JSONL reader: skip unparseable lines, count skips.

use super::record::EventRecord;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Stats from a tolerant read pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadStats {
    /// Successfully parsed records.
    pub records: usize,
    /// Lines skipped (empty lines are not counted; only non-empty unparseable).
    pub skipped: usize,
    /// Total non-empty lines seen.
    pub lines: usize,
}

/// Read all parseable events from `path`. Unparseable non-empty lines increment
/// `skipped` and are ignored — never abort on a torn tail.
pub fn read_events(path: impl AsRef<Path>) -> std::io::Result<(Vec<EventRecord>, ReadStats)> {
    let file = File::open(path.as_ref())?;
    Ok(read_events_from(BufReader::new(file)))
}

/// Read from any buffered reader (tests inject torn tails via `&[u8]`).
pub fn read_events_from<R: BufRead>(reader: R) -> (Vec<EventRecord>, ReadStats) {
    let mut out = Vec::new();
    let mut stats = ReadStats::default();
    for line in reader.lines() {
        let Ok(line) = line else {
            stats.skipped += 1;
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        stats.lines += 1;
        match serde_json::from_str::<EventRecord>(trimmed) {
            Ok(rec) => {
                stats.records += 1;
                out.push(rec);
            }
            Err(_) => {
                stats.skipped += 1;
            }
        }
    }
    (out, stats)
}
