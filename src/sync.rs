use crate::config::{self, Config, Paths};
use crate::db::{self, Item, Kind};
use crate::library;
use crate::progress::fmt_duration;
use crate::review;
use crate::sources::{mpv_events, watch_later, zathura};
use anyhow::Result;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A forward jump bigger than this in one zathura session is navigation (say, to the index),
/// not reading, so it moves the bookmark without counting pages.
const MAX_PAGES_PER_SESSION: i64 = 100;

const EVENTS_OFFSET: &str = "events_offset";

#[derive(Debug, Default)]
pub struct Report {
    pub new_items: usize,
    /// Titles of lectures and books that became complete during this sync.
    pub completed: Vec<String>,
    /// One line per change, for `patrak sync -v`.
    pub changes: Vec<String>,
    pub new_cards: usize,
}

pub fn sync(conn: &mut Connection, cfg: &Config, paths: &Paths) -> Result<Report> {
    let tx = conn.transaction()?;
    let mut report = Report::default();

    // Library: new files, renamed courses, probing changed files.
    let mut seen = Vec::new();
    let mut fresh = HashSet::new();
    let mut to_probe = Vec::new();
    for s in library::scan(cfg) {
        let (id, is_new, changed) = db::upsert_scanned(&tx, &s)?;
        seen.push(id);
        if is_new {
            fresh.insert(id);
            report.new_items += 1;
            report.changes.push(format!("new      {}", s.title));
        }
        if changed {
            to_probe.push((id, s.kind, PathBuf::from(s.path)));
        }
    }
    db::mark_missing_except(&tx, &seen)?;
    for (id, probe) in library::probe_all(to_probe) {
        db::set_probe(&tx, id, probe.duration, &probe.chapters, probe.total_pages)?;
    }

    let mut items: HashMap<i64, Item> = db::items(&tx)?.into_iter().map(|i| (i.id, i)).collect();
    let was_done: HashSet<i64> = items
        .values()
        .filter(|i| i.is_done())
        .map(|i| i.id)
        .collect();
    let index = PathIndex::new(items.values());

    // mpv script log: real watch time, positions and finished lectures.
    let log = &paths.events_log;
    let events_mode = log.exists();
    let offset = db::meta_get(&tx, EVENTS_OFFSET)?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let (events, new_offset) = mpv_events::read_from(log, offset)?;
    for ev in events {
        let Some(item) = index.find(&ev.path).and_then(|id| items.get_mut(&id)) else {
            continue;
        };
        if ev.watched > 0.0 {
            db::add_daily(&tx, cfg.day_of(ev.ts), item.id, ev.watched, 0)?;
            report.changes.push(format!(
                "watched  {} {}",
                fmt_duration(ev.watched),
                item.title
            ));
        }
        if let (None, Some(d)) = (item.duration, ev.duration) {
            db::set_duration_if_unknown(&tx, item.id, d)?;
            item.duration = Some(d);
        }
        if let Some(pos) = ev.pos
            && ev.ts >= item.position_ts
        {
            db::set_position(&tx, item.id, pos, ev.ts)?;
            item.position = pos;
            item.position_ts = ev.ts;
        }
        if ev.eof && !item.is_done() {
            db::set_completed(&tx, item.id, Some(ev.ts))?;
            item.completed_at = Some(ev.ts);
        }
    }
    db::meta_set(&tx, EVENTS_OFFSET, &new_offset.to_string())?;

    // mpv resume files: positions, plus watch time when the script isn't installed.
    let wl_dir = config::expand(&cfg.mpv_watch_later_dir);
    for item in items
        .values_mut()
        .filter(|i| i.kind == Kind::Video && !i.missing)
    {
        let Some(resume) = watch_later::lookup(&wl_dir, &item.path) else {
            continue;
        };
        if resume.mtime <= item.position_ts {
            continue;
        }
        let delta = resume.start - item.position;
        if !events_mode && !fresh.contains(&item.id) && delta > 0.0 {
            db::add_daily(&tx, cfg.day_of(resume.mtime), item.id, delta, 0)?;
            report
                .changes
                .push(format!("watched  {} {}", fmt_duration(delta), item.title));
        }
        db::set_position(&tx, item.id, resume.start, resume.mtime)?;
        item.position = resume.start;
        item.position_ts = resume.mtime;
    }

    // zathura: current page of each book. A locked or unreadable database just skips this source.
    for entry in zathura::read(&config::expand(&cfg.zathura_db)).unwrap_or_default() {
        let Some(item) = index.find(&entry.file).and_then(|id| items.get_mut(&id)) else {
            continue;
        };
        if item.kind != Kind::Book || entry.ts <= item.position_ts {
            continue;
        }
        let delta = entry.page - item.position as i64;
        if !fresh.contains(&item.id) && delta > 0 && delta <= MAX_PAGES_PER_SESSION {
            db::add_daily(&tx, cfg.day_of(entry.ts), item.id, 0.0, delta)?;
            report
                .changes
                .push(format!("read     {delta} pages of {}", item.title));
        }
        db::set_position(&tx, item.id, entry.page as f64, entry.ts)?;
        item.position = entry.page as f64;
        item.position_ts = entry.ts;
    }

    for item in items.values_mut().filter(|i| !i.is_done()) {
        let done = match item.kind {
            Kind::Video => item
                .duration
                .is_some_and(|d| d > 0.0 && item.position / d >= cfg.complete_threshold),
            Kind::Book => item
                .total_pages
                .is_some_and(|t| t > 0 && item.position as i64 >= t),
        };
        if done {
            let at = item.position_ts.max(1);
            db::set_completed(&tx, item.id, Some(at))?;
            item.completed_at = Some(at);
        }
    }

    for item in items.values() {
        if item.is_done() && !was_done.contains(&item.id) {
            report.changes.push(format!("done     {}", item.title));
            if !fresh.contains(&item.id) {
                report.completed.push(item.title.clone());
            }
        }
    }

    // Review cards in the notes of every course and book.
    let note_dirs: HashSet<PathBuf> = items
        .values()
        .filter(|i| !i.missing)
        .map(|i| i.course_dir.join(library::NOTES_DIR))
        .collect();
    let notes: Vec<PathBuf> = note_dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    report.new_cards = review::sync_cards(&tx, &notes, cfg.today())?;
    if report.new_cards > 0 {
        report
            .changes
            .push(format!("cards    {} new", report.new_cards));
    }

    tx.commit()?;
    Ok(report)
}

/// Marks an item done, or not done. Clearing also rewinds it, so the completion
/// rules don't immediately mark it done again.
pub fn set_done(conn: &Connection, id: i64, done: bool) -> Result<()> {
    let now = config::now();
    if done {
        db::set_completed(conn, id, Some(now))
    } else {
        db::set_completed(conn, id, None)?;
        db::set_position(conn, id, 0.0, now)
    }
}

/// Matches paths from mpv or zathura to items, by stored path or canonical path.
struct PathIndex(HashMap<PathBuf, i64>);

impl PathIndex {
    fn new<'a>(items: impl Iterator<Item = &'a Item>) -> Self {
        let mut map = HashMap::new();
        for item in items {
            if let Ok(canon) = item.path.canonicalize() {
                map.insert(canon, item.id);
            }
            map.insert(item.path.clone(), item.id);
        }
        Self(map)
    }

    fn find(&self, path: &str) -> Option<i64> {
        let path = Path::new(path);
        self.0
            .get(path)
            .or_else(|| self.0.get(&path.canonicalize().ok()?))
            .copied()
    }
}
