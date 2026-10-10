use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Book,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Video => "video",
            Kind::Book => "book",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chapter {
    pub start: f64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: i64,
    pub kind: Kind,
    pub path: PathBuf,
    /// Groups lectures into a course: the course folder, or the file itself for a loose video or a book.
    pub course_key: String,
    pub course: String,
    /// Folder that holds the course; notes live in `<course_dir>/notes/`.
    pub course_dir: PathBuf,
    pub title: String,
    pub ord: i64,
    pub duration: Option<f64>,
    pub chapters: Vec<Chapter>,
    pub total_pages: Option<i64>,
    /// Seconds into a video, or the 1-based page of a book.
    pub position: f64,
    pub position_ts: i64,
    pub completed_at: Option<i64>,
    pub size: i64,
    pub mtime: i64,
    pub missing: bool,
}

impl Item {
    pub fn is_done(&self) -> bool {
        self.completed_at.is_some()
    }

    /// Progress from 0 to 1, or `None` when the length is unknown.
    pub fn fraction(&self) -> Option<f64> {
        if self.is_done() {
            return Some(1.0);
        }
        let total = match self.kind {
            Kind::Video => self.duration?,
            Kind::Book => self.total_pages? as f64,
        };
        (total > 0.0).then(|| (self.position / total).clamp(0.0, 1.0))
    }

    /// The chapter the saved position falls in, as (index, chapter).
    pub fn current_chapter(&self) -> Option<(usize, &Chapter)> {
        self.chapters
            .iter()
            .enumerate()
            .rev()
            .find(|(_, c)| c.start <= self.position)
    }
}

const SCHEMA: &str = "
CREATE TABLE items (
    id           INTEGER PRIMARY KEY,
    kind         TEXT    NOT NULL,
    path         TEXT    NOT NULL UNIQUE,
    course_key   TEXT    NOT NULL,
    course       TEXT    NOT NULL,
    course_dir   TEXT    NOT NULL,
    title        TEXT    NOT NULL,
    ord          INTEGER NOT NULL,
    duration     REAL,
    chapters     TEXT,
    total_pages  INTEGER,
    position     REAL    NOT NULL DEFAULT 0,
    position_ts  INTEGER NOT NULL DEFAULT 0,
    completed_at INTEGER,
    size         INTEGER NOT NULL DEFAULT 0,
    mtime        INTEGER NOT NULL DEFAULT 0,
    missing      INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE daily (
    date    TEXT    NOT NULL,
    item_id INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
    seconds REAL    NOT NULL DEFAULT 0,
    pages   INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (date, item_id)
);
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

pub fn open(path: &Path) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    init(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", 1)?;
    }
    Ok(())
}

const ITEM_COLS: &str = "id, kind, path, course_key, course, course_dir, title, ord, duration, chapters, \
     total_pages, position, position_ts, completed_at, size, mtime, missing";

fn item_from_row(r: &Row) -> rusqlite::Result<Item> {
    let kind: String = r.get(1)?;
    let path: String = r.get(2)?;
    let course_dir: String = r.get(5)?;
    let chapters: Option<String> = r.get(9)?;
    Ok(Item {
        id: r.get(0)?,
        kind: if kind == "book" {
            Kind::Book
        } else {
            Kind::Video
        },
        path: path.into(),
        course_key: r.get(3)?,
        course: r.get(4)?,
        course_dir: course_dir.into(),
        title: r.get(6)?,
        ord: r.get(7)?,
        duration: r.get(8)?,
        chapters: chapters
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default(),
        total_pages: r.get(10)?,
        position: r.get(11)?,
        position_ts: r.get(12)?,
        completed_at: r.get(13)?,
        size: r.get(14)?,
        mtime: r.get(15)?,
        missing: r.get(16)?,
    })
}

pub fn items(conn: &Connection) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ITEM_COLS} FROM items ORDER BY kind, course_key, ord"
    ))?;
    let rows = stmt.query_map([], item_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn item(conn: &Connection, id: i64) -> Result<Option<Item>> {
    Ok(conn
        .query_row(
            &format!("SELECT {ITEM_COLS} FROM items WHERE id = ?1"),
            [id],
            item_from_row,
        )
        .optional()?)
}

/// What the library scanner found on disk for one file.
#[derive(Debug, Clone)]
pub struct Scanned {
    pub kind: Kind,
    pub path: String,
    pub course_key: String,
    pub course: String,
    pub course_dir: String,
    pub title: String,
    pub ord: i64,
    pub size: i64,
    pub mtime: i64,
}

/// Inserts or updates a scanned file. Returns the item id, whether it is new,
/// and whether its size or mtime changed (meaning it needs probing again).
pub fn upsert_scanned(conn: &Connection, s: &Scanned) -> Result<(i64, bool, bool)> {
    let existing: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT id, size, mtime FROM items WHERE path = ?1",
            [&s.path],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    match existing {
        Some((id, size, mtime)) => {
            conn.execute(
                "UPDATE items SET course_key = ?2, course = ?3, course_dir = ?4, title = ?5, ord = ?6,
                 size = ?7, mtime = ?8, missing = 0 WHERE id = ?1",
                params![id, s.course_key, s.course, s.course_dir, s.title, s.ord, s.size, s.mtime],
            )?;
            Ok((id, false, size != s.size || mtime != s.mtime))
        }
        None => {
            conn.execute(
                "INSERT INTO items (kind, path, course_key, course, course_dir, title, ord, size, mtime)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![s.kind.as_str(), s.path, s.course_key, s.course, s.course_dir, s.title, s.ord, s.size, s.mtime],
            )?;
            Ok((conn.last_insert_rowid(), true, true))
        }
    }
}

pub fn mark_missing_except(conn: &Connection, seen: &[i64]) -> Result<()> {
    conn.execute("UPDATE items SET missing = 1", [])?;
    let mut stmt = conn.prepare("UPDATE items SET missing = 0 WHERE id = ?1")?;
    for id in seen {
        stmt.execute([id])?;
    }
    Ok(())
}

pub fn set_probe(
    conn: &Connection,
    id: i64,
    duration: Option<f64>,
    chapters: &[Chapter],
    total_pages: Option<i64>,
) -> Result<()> {
    let chapters = (!chapters.is_empty())
        .then(|| serde_json::to_string(chapters))
        .transpose()?;
    conn.execute(
        "UPDATE items SET duration = ?2, chapters = ?3, total_pages = ?4 WHERE id = ?1",
        params![id, duration, chapters, total_pages],
    )?;
    Ok(())
}

pub fn set_duration_if_unknown(conn: &Connection, id: i64, duration: f64) -> Result<()> {
    conn.execute(
        "UPDATE items SET duration = ?2 WHERE id = ?1 AND duration IS NULL",
        params![id, duration],
    )?;
    Ok(())
}

pub fn set_position(conn: &Connection, id: i64, position: f64, ts: i64) -> Result<()> {
    conn.execute(
        "UPDATE items SET position = ?2, position_ts = ?3 WHERE id = ?1",
        params![id, position, ts],
    )?;
    Ok(())
}

pub fn set_completed(conn: &Connection, id: i64, at: Option<i64>) -> Result<()> {
    conn.execute(
        "UPDATE items SET completed_at = ?2 WHERE id = ?1",
        params![id, at],
    )?;
    Ok(())
}

pub fn add_daily(
    conn: &Connection,
    date: NaiveDate,
    item_id: i64,
    seconds: f64,
    pages: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO daily (date, item_id, seconds, pages) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (date, item_id) DO UPDATE SET seconds = seconds + ?3, pages = pages + ?4",
        params![date.to_string(), item_id, seconds, pages],
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct DailyRow {
    pub date: NaiveDate,
    pub item_id: i64,
    pub seconds: f64,
    pub pages: i64,
}

pub fn daily(conn: &Connection) -> Result<Vec<DailyRow>> {
    let mut stmt = conn.prepare("SELECT date, item_id, seconds, pages FROM daily ORDER BY date")?;
    let rows = stmt.query_map([], |r| {
        let date: String = r.get(0)?;
        Ok((date, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (date, item_id, seconds, pages) = row?;
        if let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
            out.push(DailyRow {
                date,
                item_id,
                seconds,
                pages,
            });
        }
    }
    Ok(out)
}

pub fn meta_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
        params![key, value],
    )?;
    Ok(())
}
