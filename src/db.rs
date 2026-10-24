use anyhow::{Context, Result};
use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Version 2: course deadlines and review cards.
const SCHEMA_V2: &str = "
CREATE TABLE course_plans (
    course_key TEXT PRIMARY KEY,
    deadline   TEXT NOT NULL
);
CREATE TABLE cards (
    id          INTEGER PRIMARY KEY,
    note        TEXT    NOT NULL,
    question    TEXT    NOT NULL,
    answer      TEXT    NOT NULL,
    due         TEXT    NOT NULL,
    interval    REAL    NOT NULL DEFAULT 0,
    ease        REAL    NOT NULL DEFAULT 2.5,
    reps        INTEGER NOT NULL DEFAULT 0,
    lapses      INTEGER NOT NULL DEFAULT 0,
    last_review INTEGER,
    missing     INTEGER NOT NULL DEFAULT 0,
    UNIQUE (note, question)
);
CREATE TABLE review_daily (
    date    TEXT PRIMARY KEY,
    seconds REAL    NOT NULL DEFAULT 0,
    cards   INTEGER NOT NULL DEFAULT 0
);
";

/// Version 3: custom habits. `created` is the first study day a habit counts on.
const SCHEMA_V3: &str = "
CREATE TABLE habits (
    id      INTEGER PRIMARY KEY,
    name    TEXT    NOT NULL UNIQUE COLLATE NOCASE,
    unit    TEXT    NOT NULL,
    goal    INTEGER NOT NULL,
    created TEXT    NOT NULL
);
CREATE TABLE habit_daily (
    date     TEXT    NOT NULL,
    habit_id INTEGER NOT NULL REFERENCES habits(id) ON DELETE CASCADE,
    amount   INTEGER NOT NULL,
    PRIMARY KEY (date, habit_id)
);
";

/// Version 4: git projects. `added` is the first study day a project's goal counts on; `refs`
/// fingerprints the branch and tag tips its commits were last counted at. `project_commits` has
/// one row per commit of the user's, by author time.
const SCHEMA_V4: &str = "
CREATE TABLE projects (
    id      INTEGER PRIMARY KEY,
    path    TEXT    NOT NULL UNIQUE,
    name    TEXT    NOT NULL,
    goal    INTEGER NOT NULL,
    added   TEXT    NOT NULL,
    refs    TEXT,
    missing INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE project_commits (
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    ts         INTEGER NOT NULL,
    PRIMARY KEY (project_id, ts)
);
";

/// Version 5: holidays, days whose goals are off and that neither break nor extend a streak.
const SCHEMA_V5: &str = "
CREATE TABLE holidays (
    date TEXT PRIMARY KEY
);
";

/// Version 6: a project's first-commit hash, which survives moving the repository, and the study
/// day it was found missing, so its goal stops counting from then on. Projects that were already
/// gone count up to their last commit.
const SCHEMA_V6: &str = "
ALTER TABLE projects ADD COLUMN root TEXT;
ALTER TABLE projects ADD COLUMN gone TEXT;
UPDATE projects SET gone = COALESCE(
    (SELECT date(MAX(ts), 'unixepoch', 'localtime', '+1 day') FROM project_commits
     WHERE project_id = projects.id), added)
WHERE missing = 1;
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
    if version < 2 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V2} PRAGMA user_version = 2; COMMIT;"
        ))?;
    }
    if version < 3 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V3} PRAGMA user_version = 3; COMMIT;"
        ))?;
    }
    if version < 4 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V4} PRAGMA user_version = 4; COMMIT;"
        ))?;
    }
    if version < 5 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V5} PRAGMA user_version = 5; COMMIT;"
        ))?;
    }
    if version < 6 {
        conn.execute_batch(&format!(
            "BEGIN; {SCHEMA_V6} PRAGMA user_version = 6; COMMIT;"
        ))?;
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

/// An item whose file turned up at a new path.
#[derive(Debug, Clone, PartialEq)]
pub struct Moved {
    pub id: i64,
    pub title: String,
}

/// Points items whose file vanished at the files that appeared with the same kind, size and
/// modified time, which is what `mv` leaves behind. Position, completion, watch history, course
/// deadlines and review cards move with them. Call before [`upsert_scanned`].
pub fn relink_moved(conn: &Connection, scanned: &[Scanned]) -> Result<Vec<Moved>> {
    let present: HashSet<&str> = scanned.iter().map(|s| s.path.as_str()).collect();
    let mut known: HashSet<String> = HashSet::new();
    let mut orphans: Vec<(i64, Item)> = Vec::new();
    for item in items(conn)? {
        let path = item.path.to_string_lossy().into_owned();
        if present.contains(path.as_str()) {
            known.insert(path);
        } else {
            orphans.push((item.id, item));
        }
    }
    let mut moved = Vec::new();
    for s in scanned {
        if known.contains(&s.path) || s.size == 0 || s.mtime == 0 {
            continue;
        }
        let same =
            |o: &&(i64, Item)| o.1.kind == s.kind && o.1.size == s.size && o.1.mtime == s.mtime;
        let candidates: Vec<&(i64, Item)> = orphans.iter().filter(same).collect();
        let pick = match candidates.as_slice() {
            [only] => Some(*only),
            many => {
                let mut titled = many.iter().filter(|o| o.1.title == s.title);
                match (titled.next(), titled.next()) {
                    (Some(one), None) => Some(*one),
                    _ => None,
                }
            }
        };
        let Some(&(id, ref old)) = pick else { continue };
        conn.execute(
            "UPDATE items SET path = ?2 WHERE id = ?1",
            params![id, s.path],
        )?;
        if old.course_key != s.course_key {
            conn.execute(
                "UPDATE OR IGNORE course_plans SET course_key = ?2 WHERE course_key = ?1",
                params![old.course_key, s.course_key],
            )?;
        }
        // The note moves along with its folder, keeping its name.
        let note = |dir: &str| format!("{dir}/notes/{}.md", old.title);
        let (old_note, new_note) = (note(&old.course_dir.to_string_lossy()), note(&s.course_dir));
        if old_note != new_note {
            conn.execute(
                "UPDATE OR IGNORE cards SET note = ?2 WHERE note = ?1",
                params![old_note, new_note],
            )?;
        }
        moved.push(Moved {
            id,
            title: old.title.clone(),
        });
        known.insert(s.path.clone());
        orphans.retain(|o| o.0 != id);
    }
    Ok(moved)
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

pub fn deadlines(conn: &Connection) -> Result<HashMap<String, NaiveDate>> {
    let mut stmt = conn.prepare("SELECT course_key, deadline FROM course_plans")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (key, date) = row?;
        if let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
            out.insert(key, date);
        }
    }
    Ok(out)
}

pub fn set_deadline(
    conn: &Connection,
    course_key: &str,
    deadline: Option<NaiveDate>,
) -> Result<()> {
    match deadline {
        Some(d) => conn.execute(
            "INSERT INTO course_plans (course_key, deadline) VALUES (?1, ?2)
             ON CONFLICT (course_key) DO UPDATE SET deadline = ?2",
            params![course_key, d.to_string()],
        )?,
        None => conn.execute(
            "DELETE FROM course_plans WHERE course_key = ?1",
            [course_key],
        )?,
    };
    Ok(())
}

pub fn holidays(conn: &Connection) -> Result<BTreeSet<NaiveDate>> {
    let mut stmt = conn.prepare("SELECT date FROM holidays")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = BTreeSet::new();
    for row in rows {
        if let Ok(date) = NaiveDate::parse_from_str(&row?, "%Y-%m-%d") {
            out.insert(date);
        }
    }
    Ok(out)
}

/// Makes `date` a holiday, or a study day again.
pub fn set_holiday(conn: &Connection, date: NaiveDate, on: bool) -> Result<()> {
    if on {
        conn.execute(
            "INSERT OR IGNORE INTO holidays (date) VALUES (?1)",
            [date.to_string()],
        )?;
    } else {
        conn.execute("DELETE FROM holidays WHERE date = ?1", [date.to_string()])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrading_to_v6_dates_when_missing_projects_went() {
        let conn = Connection::open_in_memory().unwrap();
        for (schema, v) in [
            (SCHEMA, 1),
            (SCHEMA_V2, 2),
            (SCHEMA_V3, 3),
            (SCHEMA_V4, 4),
            (SCHEMA_V5, 5),
        ] {
            conn.execute_batch(schema).unwrap();
            conn.pragma_update(None, "user_version", v).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO projects (id, path, name, goal, added, missing) VALUES
                 (1, '/a', 'a', 1, '2026-01-01', 1), (2, '/b', 'b', 1, '2026-02-03', 1),
                 (3, '/c', 'c', 1, '2026-01-01', 0);
             INSERT INTO project_commits VALUES (1, 1767225600 + 86400 * 4);",
        )
        .unwrap();
        init(&conn).unwrap();
        let gone = |id: i64| -> Option<String> {
            conn.query_row("SELECT gone FROM projects WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // The day after the last commit; without commits, the day it was added.
        assert!(gone(1).is_some_and(|g| g.starts_with("2026-01-0")));
        assert_eq!(gone(2).as_deref(), Some("2026-02-03"));
        assert_eq!(gone(3), None);
    }
}
