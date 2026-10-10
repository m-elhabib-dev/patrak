use crate::config::{Config, days_back};
use crate::db::{self, Item, Kind};
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Connection;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Days of history used to estimate a course's finishing date.
const PACE_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct Course {
    pub key: String,
    pub name: String,
    pub dir: PathBuf,
    /// Lectures in playback order, files that disappeared from disk excluded.
    pub lectures: Vec<Item>,
    pub done: usize,
    pub total_secs: f64,
    pub remaining_secs: f64,
    pub fraction: f64,
    pub last_active: i64,
    /// Index into `lectures` of the lecture to watch next.
    pub next: Option<usize>,
    pub eta: Option<NaiveDate>,
}

impl Course {
    pub fn next_lecture(&self) -> Option<&Item> {
        self.next.map(|i| &self.lectures[i])
    }

    pub fn is_single_file(&self) -> bool {
        self.lectures.len() == 1
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TodayEntry {
    pub item: Item,
    pub seconds: f64,
    pub pages: i64,
    pub minutes: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub today: NaiveDate,
    pub goal: f64,
    pub minutes_per_page: f64,
    pub minutes_by_day: BTreeMap<NaiveDate, f64>,
    pub today_entries: Vec<TodayEntry>,
    pub streak: usize,
    pub best_streak: usize,
    /// Most recently active first.
    pub courses: Vec<Course>,
    /// Most recently read first.
    pub books: Vec<Item>,
}

impl Snapshot {
    pub fn today_minutes(&self) -> f64 {
        self.minutes_on(self.today)
    }

    pub fn minutes_on(&self, day: NaiveDate) -> f64 {
        self.minutes_by_day.get(&day).copied().unwrap_or(0.0)
    }

    /// The course to continue: the most recently active one with lectures left.
    pub fn current_course(&self) -> Option<&Course> {
        self.courses.iter().find(|c| c.next.is_some())
    }

    pub fn last_book(&self) -> Option<&Item> {
        self.books
            .iter()
            .find(|b| !b.is_done() && b.position_ts > 0)
    }

    pub fn item(&self, id: i64) -> Option<&Item> {
        self.courses
            .iter()
            .flat_map(|c| &c.lectures)
            .chain(&self.books)
            .find(|i| i.id == id)
    }

    pub fn course_of(&self, item: &Item) -> Option<&Course> {
        self.courses.iter().find(|c| c.key == item.course_key)
    }

    pub fn find_courses(&self, query: &str) -> Vec<&Course> {
        self.courses
            .iter()
            .filter(|c| matches(&c.name, query))
            .collect()
    }

    pub fn find_lectures(&self, query: &str) -> Vec<&Item> {
        self.courses
            .iter()
            .flat_map(|c| &c.lectures)
            .filter(|l| matches(&l.title, query))
            .collect()
    }

    pub fn find_books(&self, query: &str) -> Vec<&Item> {
        self.books
            .iter()
            .filter(|b| matches(&b.title, query))
            .collect()
    }

    /// Minutes per week for the last `weeks` weeks (Monday first), oldest first.
    pub fn weekly_minutes(&self, weeks: usize) -> Vec<(NaiveDate, f64)> {
        let this_monday =
            self.today - Duration::days(self.today.weekday().num_days_from_monday() as i64);
        (0..weeks as i64)
            .rev()
            .map(|w| {
                let start = this_monday - Duration::weeks(w);
                let total = (0..7)
                    .map(|d| self.minutes_on(start + Duration::days(d)))
                    .sum();
                (start, total)
            })
            .collect()
    }

    /// 0 for nothing, up to 4 for reaching the goal; used for heatmap colours.
    pub fn heat_level(&self, day: NaiveDate) -> u8 {
        let ratio = self.minutes_on(day) / self.goal;
        match ratio {
            r if r <= 0.0 => 0,
            r if r < 1.0 / 3.0 => 1,
            r if r < 2.0 / 3.0 => 2,
            r if r < 1.0 => 3,
            _ => 4,
        }
    }
}

fn matches(text: &str, query: &str) -> bool {
    text.to_lowercase().contains(&query.to_lowercase())
}

pub fn build(conn: &Connection, cfg: &Config) -> Result<Snapshot> {
    let today = cfg.today();
    let goal = cfg.goal();
    let items: Vec<Item> = db::items(conn)?
        .into_iter()
        .filter(|i| !i.missing)
        .collect();
    let daily = db::daily(conn)?;

    let by_id: HashMap<i64, &Item> = items.iter().map(|i| (i.id, i)).collect();
    let mut minutes_by_day = BTreeMap::new();
    let mut today_entries = Vec::new();
    let mut recent_secs: HashMap<&str, f64> = HashMap::new();
    let pace_from = days_back(today, PACE_DAYS - 1);
    for row in &daily {
        let minutes = row.seconds / 60.0 + row.pages as f64 * cfg.minutes_per_page;
        *minutes_by_day.entry(row.date).or_insert(0.0) += minutes;
        let Some(item) = by_id.get(&row.item_id) else {
            continue;
        };
        if row.date >= pace_from {
            *recent_secs.entry(item.course_key.as_str()).or_insert(0.0) += row.seconds;
        }
        if row.date == today {
            today_entries.push(TodayEntry {
                item: (*item).clone(),
                seconds: row.seconds,
                pages: row.pages,
                minutes,
            });
        }
    }
    today_entries.sort_by(|a, b| b.minutes.total_cmp(&a.minutes));

    let mut grouped: BTreeMap<&str, Vec<&Item>> = BTreeMap::new();
    for item in items.iter().filter(|i| i.kind == Kind::Video) {
        grouped
            .entry(item.course_key.as_str())
            .or_default()
            .push(item);
    }
    let mut courses: Vec<Course> = grouped
        .into_iter()
        .map(|(key, lectures)| {
            let mut lectures: Vec<Item> = lectures.into_iter().cloned().collect();
            lectures.sort_by_key(|l| l.ord);
            let pace = recent_secs.get(key).copied().unwrap_or(0.0) / PACE_DAYS as f64;
            course(key, lectures, pace, today)
        })
        .collect();
    courses.sort_by(|a, b| {
        b.last_active
            .cmp(&a.last_active)
            .then_with(|| natord::compare(&a.name, &b.name))
    });

    let mut books: Vec<Item> = items.into_iter().filter(|i| i.kind == Kind::Book).collect();
    books.sort_by(|a, b| touched(b).cmp(&touched(a)).then(a.ord.cmp(&b.ord)));

    let (streak, best_streak) = streaks(&minutes_by_day, goal, today);
    Ok(Snapshot {
        today,
        goal,
        minutes_per_page: cfg.minutes_per_page,
        minutes_by_day,
        today_entries,
        streak,
        best_streak,
        courses,
        books,
    })
}

fn course(key: &str, lectures: Vec<Item>, secs_per_day: f64, today: NaiveDate) -> Course {
    let done = lectures.iter().filter(|l| l.is_done()).count();
    let total_secs: f64 = lectures.iter().filter_map(|l| l.duration).sum();
    let remaining_secs: f64 = lectures
        .iter()
        .filter(|l| !l.is_done())
        .filter_map(|l| l.duration.map(|d| (d - l.position).max(0.0)))
        .sum();
    let fraction = if total_secs > 0.0 {
        (total_secs - remaining_secs) / total_secs
    } else {
        done as f64 / lectures.len().max(1) as f64
    };
    let eta = (secs_per_day > 0.0 && remaining_secs > 0.0)
        .then(|| today + Duration::days((remaining_secs / secs_per_day).ceil() as i64));
    let first = &lectures[0];
    Course {
        key: key.to_owned(),
        name: first.course.clone(),
        dir: first.course_dir.clone(),
        done,
        total_secs,
        remaining_secs,
        fraction,
        last_active: lectures.iter().map(touched).max().unwrap_or(0),
        next: next_lecture(&lectures),
        eta,
        lectures,
    }
}

fn touched(item: &Item) -> i64 {
    item.position_ts.max(item.completed_at.unwrap_or(0))
}

/// Resume the lecture touched last if it is unfinished; otherwise take the first unfinished
/// lecture after it, falling back to the first unfinished one in the course.
fn next_lecture(lectures: &[Item]) -> Option<usize> {
    let last = lectures
        .iter()
        .enumerate()
        .filter(|(_, l)| touched(l) > 0)
        .max_by_key(|(_, l)| touched(l))
        .map(|(i, _)| i);
    last.and_then(|i| (i..lectures.len()).find(|&j| !lectures[j].is_done()))
        .or_else(|| lectures.iter().position(|l| !l.is_done()))
}

/// (current, best). Today only counts toward the current streak once the goal is met,
/// so an unfinished today doesn't break a streak that ran through yesterday.
pub fn streaks(minutes: &BTreeMap<NaiveDate, f64>, goal: f64, today: NaiveDate) -> (usize, usize) {
    let met = |d: NaiveDate| minutes.get(&d).is_some_and(|&m| m >= goal);
    let mut day = if met(today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    while met(day) {
        current += 1;
        day -= Duration::days(1);
    }

    let mut best = 0;
    let mut run = 0;
    let mut prev: Option<NaiveDate> = None;
    for (&d, &m) in minutes {
        if m < goal {
            continue;
        }
        run = if prev == Some(d - Duration::days(1)) {
            run + 1
        } else {
            1
        };
        best = best.max(run);
        prev = Some(d);
    }
    (current, best.max(current))
}

/// "1h 05m", "12m" or "40s".
pub fn fmt_duration(secs: f64) -> String {
    let secs = secs.max(0.0).round() as i64;
    match (secs / 3600, (secs % 3600) / 60) {
        (0, 0) if secs == 0 => "0m".to_owned(),
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

/// "mm:ss", or "h:mm:ss" past an hour.
pub fn fmt_clock(secs: f64) -> String {
    let secs = secs.max(0.0) as i64;
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

/// One-line status of a lecture or book, e.g. "▶ 45%  12:30 / 27:40".
pub fn item_status(item: &Item) -> String {
    match item.kind {
        Kind::Video => {
            let mark = if item.is_done() {
                "✓".to_owned()
            } else if item.position > 0.0 {
                format!("▶ {:.0}%", item.fraction().unwrap_or(0.0) * 100.0)
            } else {
                "·".to_owned()
            };
            match item.duration {
                Some(d) if item.position > 0.0 && !item.is_done() => {
                    format!("{mark}  {} / {}", fmt_clock(item.position), fmt_clock(d))
                }
                Some(d) => format!("{mark}  {}", fmt_clock(d)),
                None => mark,
            }
        }
        Kind::Book => {
            let page = item.position.max(0.0) as i64;
            let mark = if item.is_done() { "✓ " } else { "" };
            match item.total_pages {
                Some(t) if page > 0 => format!("{mark}page {page} / {t}"),
                Some(1) => format!("{mark}1 page"),
                Some(t) => format!("{mark}{t} pages"),
                None if page > 0 => format!("{mark}page {page}"),
                None => format!("{mark}not started"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn streak_survives_an_unfinished_today() {
        let mut m = BTreeMap::new();
        for (day, mins) in [
            ("2026-10-01", 70.0),
            ("2026-10-02", 60.0),
            ("2026-10-03", 10.0),
            ("2026-10-08", 60.0),
            ("2026-10-09", 65.0),
            ("2026-10-10", 90.0),
        ] {
            m.insert(d(day), mins);
        }
        assert_eq!(streaks(&m, 60.0, d("2026-10-11")), (3, 3));
        m.insert(d("2026-10-11"), 61.0);
        assert_eq!(streaks(&m, 60.0, d("2026-10-11")), (4, 4));
        assert_eq!(streaks(&m, 60.0, d("2026-10-13")), (0, 4));
    }

    fn lecture(ord: i64, position_ts: i64, done: bool) -> Item {
        Item {
            id: ord,
            kind: Kind::Video,
            path: format!("/c/{ord}.mp4").into(),
            course_key: "/c".into(),
            course: "c".into(),
            course_dir: "/c".into(),
            title: format!("{ord}"),
            ord,
            duration: Some(600.0),
            chapters: vec![],
            total_pages: None,
            position: if done {
                600.0
            } else if position_ts > 0 {
                300.0
            } else {
                0.0
            },
            position_ts,
            completed_at: done.then_some(position_ts),
            size: 0,
            mtime: 0,
            missing: false,
        }
    }

    #[test]
    fn next_lecture_follows_the_last_one_touched() {
        // Nothing watched yet: start at the beginning.
        assert_eq!(
            next_lecture(&[lecture(0, 0, false), lecture(1, 0, false)]),
            Some(0)
        );
        // Skipped the intro, half way through lecture 2: resume it.
        assert_eq!(
            next_lecture(&[
                lecture(0, 0, false),
                lecture(1, 5, true),
                lecture(2, 9, false)
            ]),
            Some(2)
        );
        // Finished lecture 2: move on to 3, not back to the skipped intro.
        assert_eq!(
            next_lecture(&[
                lecture(0, 0, false),
                lecture(1, 9, true),
                lecture(2, 0, false)
            ]),
            Some(2)
        );
        // Finished the last lecture: go back for the skipped intro.
        assert_eq!(
            next_lecture(&[lecture(0, 0, false), lecture(1, 9, true)]),
            Some(0)
        );
        assert_eq!(next_lecture(&[lecture(0, 3, true)]), None);
    }

    #[test]
    fn course_eta_uses_recent_pace() {
        let c = course(
            "/c",
            vec![
                lecture(0, 5, true),
                lecture(1, 9, false),
                lecture(2, 0, false),
            ],
            300.0,
            d("2026-10-11"),
        );
        assert_eq!(c.done, 1);
        assert_eq!(c.remaining_secs, 900.0);
        assert!((c.fraction - 0.5).abs() < 1e-9);
        assert_eq!(c.eta, Some(d("2026-10-14")));
    }

    #[test]
    fn formats_durations() {
        assert_eq!(fmt_duration(0.0), "0m");
        assert_eq!(fmt_duration(40.0), "40s");
        assert_eq!(fmt_duration(12.0 * 60.0), "12m");
        assert_eq!(fmt_duration(3900.0), "1h 05m");
        assert_eq!(fmt_clock(3062.4), "51:02");
        assert_eq!(fmt_clock(37205.0), "10:20:05");
    }
}
