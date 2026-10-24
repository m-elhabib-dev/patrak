use crate::config::{Config, days_back};
use crate::db::{self, Item, Kind};
use crate::download::Source;
use crate::habits::{self, HabitStatus};
use crate::projects::{self, Project, ProjectStatus};
use crate::review;
use anyhow::Result;
use chrono::{Datelike, Duration, NaiveDate};
use rusqlite::Connection;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Days of history used to estimate a course's pace and finishing date.
const PACE_DAYS: i64 = 14;

#[derive(Debug, Clone, PartialEq)]
pub struct Course {
    pub key: String,
    pub name: String,
    pub dir: PathBuf,
    /// Lectures in playback order, including those whose file disappeared, which keep their progress.
    pub lectures: Vec<Item>,
    pub done: usize,
    pub total_secs: f64,
    pub remaining_secs: f64,
    pub fraction: f64,
    pub last_active: i64,
    /// Index into `lectures` of the lecture to watch next.
    pub next: Option<usize>,
    pub eta: Option<NaiveDate>,
    /// Average seconds watched per day over the last two weeks.
    pub pace: f64,
    pub today_secs: f64,
    pub deadline: Option<NaiveDate>,
    /// Every lecture's file is gone.
    pub missing: bool,
    /// Set for courses downloaded with `patrak add`.
    pub source: Option<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    OnTrack,
    Behind,
    Overdue,
    Finished,
}

impl PlanStatus {
    pub fn label(self) -> &'static str {
        match self {
            PlanStatus::OnTrack => "on track",
            PlanStatus::Behind => "behind",
            PlanStatus::Overdue => "overdue",
            PlanStatus::Finished => "finished",
        }
    }
}

impl Course {
    /// Seconds a day needed from today on (counting what was already watched today) to
    /// finish by the deadline.
    /// Past the deadline, everything left is due today.
    pub fn needed_per_day(&self, today: NaiveDate) -> Option<f64> {
        let days = ((self.deadline? - today).num_days() + 1).max(1);
        (self.remaining_secs > 0.0).then(|| (self.remaining_secs + self.today_secs) / days as f64)
    }

    /// Seconds still to watch today to stay on schedule.
    pub fn left_today(&self, today: NaiveDate) -> f64 {
        self.needed_per_day(today)
            .map_or(0.0, |n| (n - self.today_secs).max(0.0))
    }

    pub fn plan_status(&self, today: NaiveDate) -> Option<PlanStatus> {
        let deadline = self.deadline?;
        Some(
            if self.next.is_none() || (self.total_secs > 0.0 && self.remaining_secs <= 0.0) {
                PlanStatus::Finished
            } else if deadline < today {
                PlanStatus::Overdue
            } else if self.pace + 1.0 >= self.needed_per_day(today).unwrap_or(0.0) {
                PlanStatus::OnTrack
            } else {
                PlanStatus::Behind
            },
        )
    }

    /// "due Nov 15 · 36 days left · need 25m/day, averaging 12m/day · behind"
    pub fn plan_summary(&self, today: NaiveDate) -> Option<String> {
        let deadline = self.deadline?;
        let status = self.plan_status(today)?;
        let mut parts = vec![format!("due {}", deadline.format("%b %-d"))];
        match status {
            PlanStatus::Finished => {}
            PlanStatus::Overdue => parts.push(match (today - deadline).num_days() {
                1 => "1 day late".into(),
                n => format!("{n} days late"),
            }),
            PlanStatus::OnTrack | PlanStatus::Behind => {
                let days = (deadline - today).num_days() + 1;
                parts.push(if days == 1 {
                    "last day".into()
                } else {
                    format!("{days} days left")
                });
                if let Some(need) = self.needed_per_day(today) {
                    parts.push(format!(
                        "need {}/day, averaging {}/day",
                        fmt_duration(need),
                        fmt_duration(self.pace)
                    ));
                }
            }
        }
        parts.push(status.label().into());
        Some(parts.join("  ·  "))
    }

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
}

/// What was studied on one day.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Day {
    /// Lectures watched plus cards reviewed.
    pub minutes: f64,
    pub pages: i64,
    /// Habits that counted this day (those added by then; none on a holiday).
    pub habits_due: u32,
    /// Habits whose goal was reached.
    pub habits_met: u32,
    /// Sum of each habit's share of its goal, each capped at 1.
    pub habits_share: f64,
    /// Commits in every project, with a goal or not.
    pub commits: i64,
    /// Projects whose goal counted this day (those with a goal, found by then; none on a
    /// holiday).
    pub projects_due: u32,
    /// Projects whose goal was reached.
    pub projects_met: u32,
    /// Sum of each project's share of its goal, each capped at 1.
    pub projects_share: f64,
}

impl Day {
    pub fn is_empty(&self) -> bool {
        self.minutes <= 0.0 && self.pages <= 0 && self.habits_share <= 0.0 && self.commits <= 0
    }
}

impl std::ops::AddAssign for Day {
    fn add_assign(&mut self, other: Day) {
        self.minutes += other.minutes;
        self.pages += other.pages;
        self.habits_due += other.habits_due;
        self.habits_met += other.habits_met;
        self.habits_share += other.habits_share;
        self.commits += other.commits;
        self.projects_due += other.projects_due;
        self.projects_met += other.projects_met;
        self.projects_share += other.projects_share;
    }
}

impl std::iter::Sum for Day {
    fn sum<I: Iterator<Item = Day>>(iter: I) -> Day {
        let mut total = Day::default();
        for day in iter {
            total += day;
        }
        total
    }
}

/// Daily goals; 0 turns a goal off. Each habit, and each project with a goal, due that day is a
/// goal too.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Goals {
    pub minutes: f64,
    pub pages: f64,
}

impl Goals {
    /// 0 to 1: the average share of each goal that's on, habits and projects included, each
    /// share capped at 1. With no goal on, any study counts as 1.
    pub fn progress(&self, day: Day) -> f64 {
        let shares: Vec<f64> = [(day.minutes, self.minutes), (day.pages as f64, self.pages)]
            .into_iter()
            .filter(|&(_, goal)| goal > 0.0)
            .map(|(done, goal)| (done / goal).clamp(0.0, 1.0))
            .collect();
        let goals = shares.len() + day.habits_due as usize + day.projects_due as usize;
        if goals == 0 {
            return if day.is_empty() { 0.0 } else { 1.0 };
        }
        (shares.iter().sum::<f64>() + day.habits_share + day.projects_share) / goals as f64
    }

    pub fn met(&self, day: Day) -> bool {
        self.progress(day) >= 1.0
    }

    /// (goals met, goals on), counting the same goals as [`Goals::progress`].
    pub fn tally(&self, day: Day) -> (u32, u32) {
        let (mut met, mut total) = (
            day.habits_met + day.projects_met,
            day.habits_due + day.projects_due,
        );
        for (done, goal) in [(day.minutes, self.minutes), (day.pages as f64, self.pages)] {
            if goal > 0.0 {
                total += 1;
                met += u32::from(done >= goal);
            }
        }
        (met, total)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub today: NaiveDate,
    pub goals: Goals,
    pub days: BTreeMap<NaiveDate, Day>,
    /// Days whose goals are off; they neither break nor extend a streak.
    pub holidays: BTreeSet<NaiveDate>,
    pub today_entries: Vec<TodayEntry>,
    pub streak: usize,
    pub best_streak: usize,
    /// Most recently active first.
    pub courses: Vec<Course>,
    /// Most recently read first.
    pub books: Vec<Item>,
    pub cards_due: usize,
    pub cards_total: usize,
    pub reviewed_today: i64,
    /// Oldest first.
    pub habits: Vec<HabitStatus>,
    /// Most recently committed to first.
    pub projects: Vec<ProjectStatus>,
}

impl Snapshot {
    pub fn studied_today(&self) -> Day {
        self.studied_on(self.today)
    }

    pub fn studied_on(&self, day: NaiveDate) -> Day {
        self.days.get(&day).copied().unwrap_or_default()
    }

    pub fn is_holiday(&self, day: NaiveDate) -> bool {
        self.holidays.contains(&day)
    }

    pub fn holiday_today(&self) -> bool {
        self.is_holiday(self.today)
    }

    /// The course to continue: the deadline course needing the most today if any is short of
    /// today's share, otherwise the most recently active one with lectures left.
    pub fn current_course(&self) -> Option<&Course> {
        self.plan()
            .into_iter()
            .filter(|c| c.next.is_some() && c.left_today(self.today) > 0.0)
            .max_by(|a, b| {
                a.left_today(self.today)
                    .total_cmp(&b.left_today(self.today))
            })
            .or_else(|| self.courses.iter().find(|c| c.next.is_some()))
    }

    /// Courses with a deadline that aren't finished, soonest deadline first.
    pub fn plan(&self) -> Vec<&Course> {
        let mut plan: Vec<&Course> = self
            .courses
            .iter()
            .filter(|c| {
                c.plan_status(self.today)
                    .is_some_and(|s| s != PlanStatus::Finished)
            })
            .collect();
        plan.sort_by_key(|c| c.deadline);
        plan
    }

    pub fn last_book(&self) -> Option<&Item> {
        self.books
            .iter()
            .find(|b| !b.is_done() && !b.missing && b.position_ts > 0)
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

    /// Habits whose name contains `query`, or just the one named exactly that, so "Run" can
    /// still be picked next to "Running".
    pub fn find_habits(&self, query: &str) -> Vec<&HabitStatus> {
        find_named(&self.habits, |h| &h.habit.name, query)
    }

    /// Projects whose name contains `query`, or just the one named exactly that.
    pub fn find_projects(&self, query: &str) -> Vec<&ProjectStatus> {
        find_named(&self.projects, |p| &p.project.name, query)
    }

    /// Totals per week for the last `weeks` weeks (Monday first), oldest first.
    pub fn weekly(&self, weeks: usize) -> Vec<(NaiveDate, Day)> {
        let this_monday =
            self.today - Duration::days(self.today.weekday().num_days_from_monday() as i64);
        (0..weeks as i64)
            .rev()
            .map(|w| {
                let start = this_monday - Duration::weeks(w);
                let total = (0..7)
                    .map(|d| self.studied_on(start + Duration::days(d)))
                    .sum();
                (start, total)
            })
            .collect()
    }

    /// 0 for nothing, up to 4 for reaching every goal; used for heatmap colours.
    pub fn heat_level(&self, day: NaiveDate) -> u8 {
        let studied = self.studied_on(day);
        if studied.is_empty() {
            return 0;
        }
        match self.goals.progress(studied) {
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

/// Things whose name contains `query`, or just the one named exactly that.
fn find_named<'a, T>(all: &'a [T], name: impl Fn(&T) -> &str, query: &str) -> Vec<&'a T> {
    let found: Vec<&T> = all.iter().filter(|t| matches(name(t), query)).collect();
    match found
        .iter()
        .find(|t| name(t).to_lowercase() == query.trim().to_lowercase())
    {
        Some(&exact) => vec![exact],
        None => found,
    }
}

pub fn build(conn: &Connection, cfg: &Config) -> Result<Snapshot> {
    let today = cfg.today();
    let goals = Goals {
        minutes: f64::from(cfg.daily_goal_minutes),
        pages: f64::from(cfg.daily_goal_pages),
    };
    let items: Vec<Item> = db::items(conn)?;
    let daily = db::daily(conn)?;

    let by_id: HashMap<i64, &Item> = items.iter().map(|i| (i.id, i)).collect();
    let mut days: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    let mut today_entries = Vec::new();
    // Per course: seconds watched in the pace window and the first day with any.
    let mut recent_secs: HashMap<&str, (f64, NaiveDate)> = HashMap::new();
    let mut today_secs: HashMap<&str, f64> = HashMap::new();
    let pace_from = days_back(today, PACE_DAYS - 1);
    for row in &daily {
        let day = days.entry(row.date).or_default();
        day.minutes += row.seconds / 60.0;
        day.pages += row.pages;
        let Some(item) = by_id.get(&row.item_id) else {
            continue;
        };
        if row.date >= pace_from && row.seconds > 0.0 {
            let entry = recent_secs
                .entry(item.course_key.as_str())
                .or_insert((0.0, row.date));
            entry.0 += row.seconds;
            entry.1 = entry.1.min(row.date);
        }
        if row.date == today {
            *today_secs.entry(item.course_key.as_str()).or_insert(0.0) += row.seconds;
            today_entries.push(TodayEntry {
                item: (*item).clone(),
                seconds: row.seconds,
                pages: row.pages,
            });
        }
    }
    // Lectures first, longest first; then books, most pages first.
    today_entries.sort_by(|a, b| {
        (a.item.kind == Kind::Book)
            .cmp(&(b.item.kind == Kind::Book))
            .then(b.seconds.total_cmp(&a.seconds))
            .then(b.pages.cmp(&a.pages))
    });

    let mut reviewed_today = 0;
    for (date, seconds, cards) in review::daily(conn)? {
        days.entry(date).or_default().minutes += seconds / 60.0;
        if date == today {
            reviewed_today = cards;
        }
    }
    let (cards_due, cards_total) = review::counts(conn, today)?;
    let deadlines = db::deadlines(conn)?;
    let holidays = db::holidays(conn)?;

    let habit_list = habits::list(conn)?;
    let habit_logs = habits::daily(conn)?;
    let project_list = projects::list(conn)?;
    let commit_logs = projects::daily(conn, cfg)?;
    for &(date, _, commits) in &commit_logs {
        days.entry(date).or_default().commits += commits;
    }
    let goal_projects: Vec<&Project> = project_list.iter().filter(|p| p.has_goal()).collect();
    if !habit_list.is_empty() || !goal_projects.is_empty() {
        let amounts: HashMap<(NaiveDate, i64), i64> = habit_logs
            .iter()
            .map(|&(date, id, amount)| ((date, id), amount))
            .collect();
        let commits: HashMap<(NaiveDate, i64), i64> = commit_logs
            .iter()
            .map(|&(date, id, commits)| ((date, id), commits))
            .collect();
        // Today too, so a day with nothing logged yet isn't counted as met.
        let mut dates: BTreeSet<NaiveDate> = days.keys().copied().collect();
        dates.extend(habit_logs.iter().map(|l| l.0));
        dates.insert(today);
        // A holiday has no goals.
        for date in dates.into_iter().filter(|d| !holidays.contains(d)) {
            let day = days.entry(date).or_default();
            for habit in habit_list.iter().filter(|h| h.created <= date) {
                let amount = amounts.get(&(date, habit.id)).copied().unwrap_or(0);
                day.habits_due += 1;
                day.habits_met += u32::from(habit.met(amount));
                day.habits_share += habit.share(amount);
            }
            for project in goal_projects.iter().filter(|p| p.counts_on(date)) {
                let n = commits.get(&(date, project.id)).copied().unwrap_or(0);
                day.projects_due += 1;
                day.projects_met += u32::from(project.met(n));
                day.projects_share += project.share(n);
            }
        }
    }
    let habits = habits::statuses(&habit_list, &habit_logs, &holidays, today);
    let projects = projects::statuses(&project_list, &commit_logs, &holidays, today);

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
            // Averaged from the first day studied in the window, so a course started
            // yesterday isn't judged by the twelve days before it.
            let pace = recent_secs.get(key).map_or(0.0, |&(secs, first)| {
                secs / ((today - first).num_days() + 1) as f64
            });
            let mut c = course(key, lectures, pace, today);
            c.today_secs = today_secs.get(key).copied().unwrap_or(0.0);
            c.deadline = deadlines.get(key).copied();
            // Only folder courses can carry a source file.
            if Path::new(key) == c.dir {
                c.source = Source::load(&c.dir);
            }
            c
        })
        .collect();
    courses.sort_by(|a, b| {
        b.last_active
            .cmp(&a.last_active)
            .then_with(|| natord::compare(&a.name, &b.name))
    });

    let mut books: Vec<Item> = items.into_iter().filter(|i| i.kind == Kind::Book).collect();
    books.sort_by(|a, b| touched(b).cmp(&touched(a)).then(a.ord.cmp(&b.ord)));

    let (streak, best_streak) = streaks(&days, goals, &holidays, today);
    Ok(Snapshot {
        today,
        goals,
        days,
        holidays,
        today_entries,
        streak,
        best_streak,
        courses,
        books,
        cards_due,
        cards_total,
        reviewed_today,
        habits,
        projects,
    })
}

fn course(key: &str, lectures: Vec<Item>, secs_per_day: f64, today: NaiveDate) -> Course {
    let done = lectures.iter().filter(|l| l.is_done()).count();
    let total_secs: f64 = lectures.iter().filter_map(|l| l.duration).sum();
    let left = |l: &&Item| !l.is_done();
    let secs_left = |l: &Item| l.duration.map(|d| (d - l.position).max(0.0));
    let remaining_all: f64 = lectures.iter().filter(left).filter_map(secs_left).sum();
    // What can still be watched: lectures whose file is gone can't be.
    let remaining_secs: f64 = lectures
        .iter()
        .filter(|l| !l.missing)
        .filter(left)
        .filter_map(secs_left)
        .sum();
    let fraction = if total_secs > 0.0 {
        (total_secs - remaining_all) / total_secs
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
        pace: secs_per_day,
        today_secs: 0.0,
        deadline: None,
        missing: lectures.iter().all(|l| l.missing),
        source: None,
        lectures,
    }
}

fn touched(item: &Item) -> i64 {
    item.position_ts.max(item.completed_at.unwrap_or(0))
}

/// Resume the lecture touched last if it is unfinished; otherwise take the first unfinished
/// lecture after it, falling back to the first unfinished one in the course. Lectures whose
/// file is gone are never picked.
fn next_lecture(lectures: &[Item]) -> Option<usize> {
    let open = |j: usize| !lectures[j].is_done() && !lectures[j].missing;
    let last = lectures
        .iter()
        .enumerate()
        .filter(|(_, l)| touched(l) > 0)
        .max_by_key(|(_, l)| touched(l))
        .map(|(i, _)| i);
    last.and_then(|i| (i..lectures.len()).find(|&j| open(j)))
        .or_else(|| (0..lectures.len()).find(|&j| open(j)))
}

/// (current, best) streaks of days that met the goals.
pub fn streaks(
    days: &BTreeMap<NaiveDate, Day>,
    goals: Goals,
    holidays: &BTreeSet<NaiveDate>,
    today: NaiveDate,
) -> (usize, usize) {
    streaks_by(
        days.keys().copied(),
        |d| days.get(&d).is_some_and(|&s| goals.met(s)),
        |d| holidays.contains(&d),
        today,
    )
}

/// (current, best) streaks of days that `met` accepts; `days` lists, in order, every day that
/// might be. A holiday neither breaks a streak nor adds to it. Today only counts toward the
/// current streak once it is met, so an unfinished today doesn't break a streak that ran
/// through yesterday.
pub fn streaks_by(
    days: impl IntoIterator<Item = NaiveDate>,
    met: impl Fn(NaiveDate) -> bool,
    holiday: impl Fn(NaiveDate) -> bool,
    today: NaiveDate,
) -> (usize, usize) {
    let counts = |d: NaiveDate| !holiday(d) && met(d);
    let mut day = if counts(today) {
        today
    } else {
        today - Duration::days(1)
    };
    let mut current = 0;
    loop {
        if counts(day) {
            current += 1;
        } else if !holiday(day) {
            break;
        }
        day -= Duration::days(1);
    }

    let mut best = 0;
    let mut run = 0;
    let mut prev: Option<NaiveDate> = None;
    for d in days {
        if !counts(d) {
            continue;
        }
        // Joined to the previous day counted when only holidays lie between them.
        let joined =
            prev.is_some_and(|p| (1..(d - p).num_days()).all(|i| holiday(p + Duration::days(i))));
        run = if joined { run + 1 } else { 1 };
        best = best.max(run);
        prev = Some(d);
    }
    (current, best.max(current))
}

/// "5-day streak", "5-day streak, best 12" or "best: 12 days"; `None` before the first day the
/// goal was met.
pub fn streak_text(streak: usize, best: usize) -> Option<String> {
    let days = |n: usize| {
        if n == 1 {
            "1 day".into()
        } else {
            format!("{n} days")
        }
    };
    match (streak, best) {
        (0, 0) => None,
        (0, best) => Some(format!("best: {}", days(best))),
        (s, best) if best > s => Some(format!("{s}-day streak, best {best}")),
        (s, _) => Some(format!("{s}-day streak")),
    }
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

/// `title` without a leading course name: "Course ｜ Episode 5" becomes "Episode 5". Only
/// strips the name when a separator follows it, and never leaves nothing.
pub fn short_title<'a>(title: &'a str, course: &str) -> &'a str {
    const SEPARATORS: &[char] = &['|', '｜', '-', '–', '—', ':', '·'];
    let Some(rest) = title.strip_prefix(course) else {
        return title;
    };
    let rest = rest.trim_start();
    if course.is_empty() || !rest.starts_with(SEPARATORS) {
        return title;
    }
    match rest.trim_start_matches(|c: char| c.is_whitespace() || SEPARATORS.contains(&c)) {
        "" => title,
        short => short,
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
                "○".to_owned()
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
    use chrono::{Local, TimeZone};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn studied(minutes: f64, pages: i64) -> Day {
        Day {
            minutes,
            pages,
            ..Day::default()
        }
    }

    const MINUTES_ONLY: Goals = Goals {
        minutes: 60.0,
        pages: 0.0,
    };

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
            m.insert(d(day), studied(mins, 0));
        }
        let none = BTreeSet::new();
        assert_eq!(streaks(&m, MINUTES_ONLY, &none, d("2026-10-11")), (3, 3));
        m.insert(d("2026-10-11"), studied(61.0, 0));
        assert_eq!(streaks(&m, MINUTES_ONLY, &none, d("2026-10-11")), (4, 4));
        assert_eq!(streaks(&m, MINUTES_ONLY, &none, d("2026-10-13")), (0, 4));
    }

    #[test]
    fn streak_needs_every_goal_that_is_on() {
        let both = Goals {
            minutes: 60.0,
            pages: 20.0,
        };
        let mut m = BTreeMap::new();
        m.insert(d("2026-10-09"), studied(60.0, 25));
        m.insert(d("2026-10-10"), studied(90.0, 5));
        let none = BTreeSet::new();
        assert_eq!(streaks(&m, both, &none, d("2026-10-11")), (0, 1));
        assert_eq!(streaks(&m, MINUTES_ONLY, &none, d("2026-10-11")), (2, 2));
    }

    #[test]
    fn holidays_bridge_the_streak() {
        let mut m = BTreeMap::new();
        for day in ["2026-10-05", "2026-10-07", "2026-10-08", "2026-10-11"] {
            m.insert(d(day), studied(60.0, 0));
        }
        let none = BTreeSet::new();
        assert_eq!(streaks(&m, MINUTES_ONLY, &none, d("2026-10-11")), (1, 2));

        // The 6th joins the 5th to the 7th; the 9th and 10th join the 8th to the 11th,
        // without adding to the count.
        let holidays: BTreeSet<NaiveDate> = ["2026-10-06", "2026-10-09", "2026-10-10"]
            .into_iter()
            .map(d)
            .collect();
        assert_eq!(
            streaks(&m, MINUTES_ONLY, &holidays, d("2026-10-11")),
            (4, 4)
        );
        // A holiday today keeps the streak that ran through yesterday.
        let mut today_off = holidays.clone();
        today_off.insert(d("2026-10-12"));
        assert_eq!(
            streaks(&m, MINUTES_ONLY, &today_off, d("2026-10-12")),
            (4, 4)
        );
        // So does a run of holidays; a day that is neither still breaks it.
        today_off.insert(d("2026-10-13"));
        assert_eq!(
            streaks(&m, MINUTES_ONLY, &today_off, d("2026-10-14")),
            (4, 4)
        );
        assert_eq!(
            streaks(&m, MINUTES_ONLY, &today_off, d("2026-10-15")),
            (0, 4)
        );
        // Studying on a holiday doesn't add to the streak either.
        m.insert(d("2026-10-06"), studied(90.0, 0));
        assert_eq!(
            streaks(&m, MINUTES_ONLY, &holidays, d("2026-10-11")),
            (4, 4)
        );
    }

    #[test]
    fn a_holiday_turns_off_the_days_goals() {
        let conn = db::open_in_memory().unwrap();
        let cfg = Config {
            daily_goal_minutes: 0,
            daily_goal_pages: 0,
            ..Config::default()
        };
        let today = cfg.today();
        let day = |n: i64| today - Duration::days(n);
        let eat = habits::add(&conn, "Eat", habits::Unit::Count, 3, day(3)).unwrap();
        // Only one meal yesterday.
        for (date, amount) in [(day(3), 3), (day(2), 3), (day(1), 1), (today, 3)] {
            habits::set_amount(&conn, eat, date, amount).unwrap();
        }
        let snap = build(&conn, &cfg).unwrap();
        assert_eq!((snap.streak, snap.best_streak), (1, 2));

        db::set_holiday(&conn, day(1), true).unwrap();
        db::set_holiday(&conn, day(1), true).unwrap();
        let snap = build(&conn, &cfg).unwrap();
        assert!(snap.is_holiday(day(1)) && !snap.holiday_today());
        assert_eq!(snap.studied_on(day(1)).habits_due, 0);
        assert_eq!((snap.streak, snap.best_streak), (3, 3));
        assert_eq!((snap.habits[0].streak, snap.habits[0].best_streak), (3, 3));

        db::set_holiday(&conn, day(1), false).unwrap();
        let snap = build(&conn, &cfg).unwrap();
        assert!(snap.holidays.is_empty());
        assert_eq!(snap.streak, 1);
    }

    #[test]
    fn goal_progress_averages_capped_shares() {
        let both = Goals {
            minutes: 60.0,
            pages: 20.0,
        };
        // Twice the minutes can't make up for no pages.
        assert_eq!(both.progress(studied(120.0, 0)), 0.5);
        assert_eq!(both.progress(studied(30.0, 10)), 0.5);
        assert!(both.met(studied(60.0, 20)));
        assert!(!both.met(studied(600.0, 19)));

        let pages_only = Goals {
            minutes: 0.0,
            pages: 20.0,
        };
        assert!(pages_only.met(studied(0.0, 20)));
        assert_eq!(pages_only.progress(studied(90.0, 5)), 0.25);

        // No goal: any study counts.
        let none = Goals {
            minutes: 0.0,
            pages: 0.0,
        };
        assert!(!none.met(Day::default()));
        assert!(none.met(studied(0.0, 1)));
    }

    #[test]
    fn goal_progress_counts_habits() {
        // 60 minutes watched, one of two habits done and the other half way.
        let day = Day {
            habits_due: 2,
            habits_met: 1,
            habits_share: 1.5,
            ..studied(60.0, 0)
        };
        assert_eq!(MINUTES_ONLY.progress(day), 2.5 / 3.0);
        assert!(!MINUTES_ONLY.met(day));
        let all_done = Day {
            habits_met: 2,
            habits_share: 2.0,
            ..day
        };
        assert!(MINUTES_ONLY.met(all_done));
        // Habits alone are the goal when the others are off.
        let none = Goals {
            minutes: 0.0,
            pages: 0.0,
        };
        assert_eq!(none.progress(day), 0.75);
    }

    #[test]
    fn tally_counts_each_goal_that_is_on() {
        let both = Goals {
            minutes: 60.0,
            pages: 20.0,
        };
        let day = Day {
            habits_due: 2,
            habits_met: 1,
            projects_due: 1,
            projects_met: 1,
            ..studied(60.0, 18)
        };
        assert_eq!(both.tally(day), (3, 5));
        assert_eq!(MINUTES_ONLY.tally(day), (3, 4));
        assert_eq!(MINUTES_ONLY.tally(Day::default()), (0, 1));
    }

    #[test]
    fn habits_gate_the_streak_from_the_day_they_were_added() {
        let conn = db::open_in_memory().unwrap();
        let cfg = Config {
            daily_goal_minutes: 0,
            daily_goal_pages: 0,
            ..Config::default()
        };
        let today = cfg.today();
        let day = |n: i64| today - Duration::days(n);
        let eat = habits::add(&conn, "Eat", habits::Unit::Count, 3, day(2)).unwrap();
        let run = habits::add(&conn, "Run", habits::Unit::Minutes, 30, day(1)).unwrap();
        for (date, id, amount) in [
            (day(3), eat, 3),
            (day(2), eat, 3),
            (day(1), eat, 3),
            (day(1), run, 45),
            (today, eat, 3),
        ] {
            habits::set_amount(&conn, id, date, amount).unwrap();
        }

        let snap = build(&conn, &cfg).unwrap();
        // Day 3 came before "Eat" was added; today "Run" is still missing.
        assert_eq!(snap.studied_on(day(3)).habits_due, 0);
        assert_eq!(snap.studied_on(day(1)).habits_met, 2);
        assert!(!snap.goals.met(snap.studied_today()));
        assert_eq!((snap.streak, snap.best_streak), (2, 2));
        assert_eq!(snap.habits.len(), 2);
        assert_eq!((snap.habits[0].today, snap.habits[0].streak), (3, 3));
        assert!(snap.habits[0].done() && !snap.habits[1].done());

        habits::set_amount(&conn, run, today, 30).unwrap();
        let snap = build(&conn, &cfg).unwrap();
        assert!(snap.goals.met(snap.studied_today()));
        assert_eq!(snap.streak, 3);
    }

    #[test]
    fn projects_gate_the_streak_from_the_day_they_were_found() {
        let conn = db::open_in_memory().unwrap();
        let cfg = Config {
            daily_goal_minutes: 0,
            daily_goal_pages: 0,
            ..Config::default()
        };
        let today = cfg.today();
        let day = |n: i64| today - Duration::days(n);
        let app = projects::add(&conn, Path::new("/p/app"), "app", None, day(2)).unwrap();
        let notes = projects::add(&conn, Path::new("/p/notes"), "notes", None, day(2)).unwrap();
        projects::set_goal(&conn, app, 2).unwrap();
        // No goal: tracked, never due.
        projects::set_goal(&conn, notes, 0).unwrap();
        let commit = |date: NaiveDate, id: i64, n: i64| {
            let noon = Local
                .from_local_datetime(&date.and_hms_opt(12, 0, 0).unwrap())
                .unwrap()
                .timestamp();
            for i in 0..n {
                projects::record(&conn, id, noon + i).unwrap();
            }
        };
        for (date, id, commits) in [
            (day(4), app, 1),
            (day(2), app, 2),
            (day(1), app, 3),
            (day(1), notes, 1),
            (today, app, 1),
        ] {
            commit(date, id, commits);
        }

        let snap = build(&conn, &cfg).unwrap();
        // Day 4 came before the project was found: its commit shows but isn't due.
        assert_eq!(snap.studied_on(day(4)).commits, 1);
        assert_eq!(snap.studied_on(day(4)).projects_due, 0);
        assert_eq!(snap.studied_on(day(1)).commits, 4);
        assert_eq!(snap.studied_on(day(1)).projects_due, 1);
        assert_eq!(snap.studied_today().projects_share, 0.5);
        assert!(!snap.goals.met(snap.studied_today()));
        assert_eq!((snap.streak, snap.best_streak), (2, 2));
        assert_eq!(snap.find_projects("APP").len(), 1);
        let status = &snap.projects[0];
        assert_eq!(status.project.name, "app");
        assert_eq!((status.today, status.total, status.streak), (1, 7, 2));

        // Today's noon commit is already there, so this adds one more.
        commit(today, app, 2);
        let snap = build(&conn, &cfg).unwrap();
        assert!(snap.goals.met(snap.studied_today()));
        assert_eq!(snap.streak, 3);
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
    fn deadline_needs_a_daily_share() {
        let today = d("2026-10-11");
        let mut c = course(
            "/c",
            vec![
                lecture(0, 5, true),
                lecture(1, 9, false),
                lecture(2, 0, false),
            ],
            300.0,
            today,
        );
        assert_eq!(c.plan_status(today), None);

        // 900 s left, 300 s watched today, 3 days including today: 400 s a day.
        c.today_secs = 300.0;
        c.deadline = Some(d("2026-10-13"));
        assert_eq!(c.needed_per_day(today), Some(400.0));
        assert_eq!(c.left_today(today), 100.0);
        assert_eq!(c.plan_status(today), Some(PlanStatus::Behind));
        c.pace = 450.0;
        assert_eq!(c.plan_status(today), Some(PlanStatus::OnTrack));
        assert_eq!(c.plan_status(d("2026-10-14")), Some(PlanStatus::Overdue));
        // Overdue: the rest is due today.
        assert_eq!(c.needed_per_day(d("2026-10-14")), Some(1200.0));
        assert!(
            c.plan_summary(today)
                .unwrap()
                .starts_with("due Oct 13  ·  3 days left  ·  need 6m/day")
        );
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

    #[test]
    fn short_title_drops_a_leading_course_name() {
        let course = "Distributed Systems بالعربي";
        assert_eq!(
            short_title("Distributed Systems بالعربي ｜ Episode 5 ｜ Clocks", course),
            "Episode 5 ｜ Clocks"
        );
        assert_eq!(
            short_title("Rust Book - 03: Ownership", "Rust Book"),
            "03: Ownership"
        );
        // No separator after the name, or nothing after it: left alone.
        assert_eq!(
            short_title("Rust Book Advanced", "Rust Book"),
            "Rust Book Advanced"
        );
        assert_eq!(short_title("Rust Book", "Rust Book"), "Rust Book");
        assert_eq!(short_title("Rust Book |", "Rust Book"), "Rust Book |");
        assert_eq!(short_title("Intro", "Rust Book"), "Intro");
        assert_eq!(short_title("- Intro", ""), "- Intro");
    }
}
