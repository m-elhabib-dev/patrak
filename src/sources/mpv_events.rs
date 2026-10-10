//! Events appended by `assets/patrak.lua`, one JSON object per line.

use anyhow::Result;
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Event {
    pub ts: i64,
    pub path: String,
    #[serde(default)]
    pub pos: Option<f64>,
    #[serde(default)]
    pub duration: Option<f64>,
    /// Seconds of actual playback since the previous event for this file.
    #[serde(default)]
    pub watched: f64,
    /// The file played to its end.
    #[serde(default)]
    pub eof: bool,
}

/// Reads the complete lines after byte `offset` and returns them with the new offset.
/// Starts over when the log is shorter than `offset` (it was truncated or replaced).
pub fn read_from(log: &Path, offset: u64) -> Result<(Vec<Event>, u64)> {
    let mut file = match std::fs::File::open(log) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(e.into()),
    };
    let offset = if file.metadata()?.len() < offset {
        0
    } else {
        offset
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    // A line without its newline is still being written; leave it for the next read.
    let complete = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    let events = String::from_utf8_lossy(&buf[..complete])
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    Ok((events, offset + complete as u64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_incrementally_and_restarts_after_truncation() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("events.jsonl");
        let mut f = std::fs::File::create(&log).unwrap();
        writeln!(
            f,
            r#"{{"ts":1,"path":"/a.mp4","pos":60.0,"duration":600.0,"watched":60.0,"eof":false}}"#
        )
        .unwrap();
        write!(f, r#"{{"ts":2,"path":"/a.mp4","#).unwrap();

        let (events, offset) = read_from(&log, 0).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].watched, 60.0);

        writeln!(f, r#""pos":600.0,"watched":540.0,"eof":true}}"#).unwrap();
        let (events, offset2) = read_from(&log, offset).unwrap();
        assert_eq!(events.len(), 1);
        assert!(events[0].eof);
        assert_eq!(read_from(&log, offset2).unwrap().0.len(), 0);

        std::fs::write(&log, "{\"ts\":3,\"path\":\"/b.mp4\"}\n").unwrap();
        let (events, _) = read_from(&log, offset2).unwrap();
        assert_eq!(events[0].path, "/b.mp4");
        assert_eq!(
            read_from(&tmp.path().join("missing"), 5).unwrap(),
            (vec![], 0)
        );
    }
}
