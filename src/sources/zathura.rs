//! zathura's history database. `fileinfo.page` is the 0-based page shown when the file was
//! closed, and `time` comes from SQLite's `DATETIME('now')`, which is UTC.

use anyhow::Result;
use chrono::NaiveDateTime;
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub file: String,
    /// 1-based page number.
    pub page: i64,
    pub ts: i64,
}

pub fn read(db: &Path) -> Result<Vec<Entry>> {
    if !db.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut stmt = conn.prepare("SELECT file, page, time FROM fileinfo WHERE file IS NOT NULL")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<i64>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (file, page, time) = row?;
        let ts = time
            .and_then(|t| NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S").ok())
            .map(|t| t.and_utc().timestamp());
        if let (Some(page), Some(ts)) = (page, ts) {
            out.push(Entry {
                file,
                page: page + 1,
                ts,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fileinfo() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("bookmarks.sqlite");
        let conn = Connection::open(&db).unwrap();
        // The real schema, copied from ~/.local/share/zathura/bookmarks.sqlite.
        conn.execute_batch(
            "CREATE TABLE fileinfo (file TEXT PRIMARY KEY,page INTEGER,offset INTEGER,zoom FLOAT,rotation INTEGER,
             pages_per_row INTEGER,first_page_column TEXT,position_x FLOAT,position_y FLOAT,time TIMESTAMP,
             page_right_to_left INTEGER,sha256 BLOB,adjust_mode INTEGER);
             INSERT INTO fileinfo (file, page, time) VALUES ('/books/a.pdf', 9, '2026-10-11 18:00:00');",
        )
        .unwrap();
        drop(conn);
        let entries = read(&db).unwrap();
        assert_eq!(
            entries,
            [Entry {
                file: "/books/a.pdf".into(),
                page: 10,
                ts: 1_791_741_600
            }]
        );
    }
}
