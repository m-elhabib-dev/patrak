use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, NaiveDate};
use patrak::Session;
use patrak::db::{self, Item, Kind};
use patrak::habits::{self, Habit, HabitStatus};
use patrak::progress::{Course, Day, Snapshot, fmt_clock, fmt_duration, item_status};
use patrak::projects::{self, ProjectStatus};
use patrak::review::{self, Card, Grade};
use patrak::{config::Config, download, launch, setup, sync::set_done};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Instant;

fn color() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
}

fn paint(code: &str, s: &str) -> String {
    if color() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_owned()
    }
}

fn bold(s: &str) -> String {
    paint("1", s)
}

fn dim(s: &str) -> String {
    paint("2", s)
}

fn green(s: &str) -> String {
    paint("32", s)
}

fn purple(s: &str) -> String {
    paint("35", s)
}

fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let done = "█".repeat(filled);
    let left = "░".repeat(width - filled);
    format!(
        "{}{}",
        if fraction >= 1.0 { green(&done) } else { done },
        dim(&left)
    )
}

/// Opens the database and syncs, which every command starts with.
fn session() -> Result<(Session, Snapshot)> {
    let mut s = Session::open()?;
    s.sync()?;
    let snap = s.snapshot()?;
    Ok((s, snap))
}

pub fn today() -> Result<()> {
    let (_, snap) = session()?;
    print_today(&snap);
    Ok(())
}

/// "████░░ 32 / 45 min", or just "32 min" when the goal is off.
fn goal_part(done: f64, goal: f64, unit: &str) -> String {
    let done_text = bold(&format!("{done:.0}"));
    if goal > 0.0 {
        format!("{} {done_text} / {goal:.0} {unit}", bar(done / goal, 12))
    } else {
        format!("{done_text} {unit}")
    }
}

fn print_today(snap: &Snapshot) {
    let studied = snap.studied_today();
    let date = bold(&snap.today.format("%a %d %b").to_string());
    let streak = format!(
        "streak {} (best {})",
        bold(&snap.streak.to_string()),
        snap.best_streak
    );
    if snap.holiday_today() {
        // Goals are off: only what was done.
        println!(
            "{date}  {}   {}   {}   {streak}",
            purple("holiday · goals off"),
            goal_part(studied.minutes, 0.0, "min"),
            goal_part(studied.pages as f64, 0.0, "pages"),
        );
    } else {
        let habits = if snap.habits.is_empty() {
            String::new()
        } else {
            format!(
                "   habits {}/{}",
                bold(&studied.habits_met.to_string()),
                studied.habits_due
            )
        };
        let projects = if studied.projects_due > 0 {
            format!(
                "   projects {}/{}",
                bold(&studied.projects_met.to_string()),
                studied.projects_due
            )
        } else {
            String::new()
        };
        println!(
            "{date}  {}   {}{habits}{projects}   {streak}",
            goal_part(studied.minutes, snap.goals.minutes, "min"),
            goal_part(studied.pages as f64, snap.goals.pages, "pages"),
        );
    }

    if !snap.habits.is_empty() {
        println!("\n{}", bold("Habits"));
        for h in &snap.habits {
            print_habit(h);
        }
    }

    let active: Vec<&ProjectStatus> = snap
        .projects
        .iter()
        .filter(|p| (p.project.has_goal() && !p.project.missing) || p.today > 0)
        .collect();
    if !active.is_empty() {
        println!("\n{}", bold("Projects"));
        for p in active {
            print_project(p);
        }
    }

    if !snap.today_entries.is_empty() {
        println!("\n{}", bold("Today"));
        for e in &snap.today_entries {
            let what = match e.item.kind {
                Kind::Video => fmt_duration(e.seconds),
                Kind::Book => format!("{} pages", e.pages),
            };
            println!("  {:>8}  {}  {}", what, e.item.title, dim(&e.item.course));
        }
    }

    let plan = snap.plan();
    if !plan.is_empty() {
        println!("\n{}", bold("Plan"));
        for c in plan {
            let need = c.needed_per_day(snap.today).unwrap_or(0.0);
            let share = if need > 0.0 { c.today_secs / need } else { 1.0 };
            println!(
                "  {}  {:>4} of {:<4} today  {}",
                bar(share, 10),
                fmt_duration(c.today_secs),
                fmt_duration(need),
                bold(&c.name)
            );
            if let Some(summary) = c.plan_summary(snap.today) {
                println!("  {}", dim(&summary));
            }
        }
    }

    if snap.cards_total > 0 {
        println!("\n{}", bold("Review"));
        if snap.cards_due > 0 {
            println!("  {} cards due   {}", snap.cards_due, dim("patrak review"));
        } else {
            println!(
                "  {}",
                dim(&format!(
                    "all caught up ({} reviewed today)",
                    snap.reviewed_today
                ))
            );
        }
    }

    println!("\n{}", bold("Next up"));
    match snap.current_course() {
        Some(course) => {
            let lecture = course
                .next_lecture()
                .expect("current course has a next lecture");
            println!("  {}  {}", lecture.title, dim(&item_status(lecture)));
            println!("    {}", dim(&course.name));
        }
        None if snap.courses.is_empty() => println!(
            "  {}",
            dim("No courses yet. Put a folder of lectures in your video_dirs.")
        ),
        None => println!("  {}", green("Every course is finished.")),
    }
    if let Some(book) = snap.last_book() {
        println!("  📖 {}  {}", book.title, dim(&item_status(book)));
    }
}

/// "████████████ 3 / 3 times  Eat  · 4-day streak"
fn print_habit(h: &HabitStatus) {
    let streak = h
        .streak_text()
        .map(|s| dim(&format!("  · {s}")))
        .unwrap_or_default();
    println!(
        "  {}  {}{streak}",
        goal_part(
            h.today as f64,
            h.habit.goal as f64,
            h.habit.unit.word(h.habit.goal)
        ),
        bold(&h.habit.name)
    );
}

/// "████████████ 2 / 1 commit  patrak  · 4-day streak"
fn print_project(p: &ProjectStatus) {
    let streak = p
        .streak_text()
        .map(|s| dim(&format!("  · {s}")))
        .unwrap_or_default();
    let unit = projects::word(if p.project.has_goal() {
        p.project.goal
    } else {
        p.today
    });
    let gone = if p.project.missing {
        dim("  (missing)")
    } else {
        String::new()
    };
    println!(
        "  {}  {}{streak}{gone}",
        goal_part(p.today as f64, p.project.goal as f64, unit),
        bold(&p.project.name)
    );
}

/// "last commit Oct 11 · 128 commits in all · ~/patrak/Projects/patrak", dimmed.
fn print_project_details(p: &ProjectStatus, today: NaiveDate) {
    let mut parts = Vec::new();
    if let Some(last) = p.last_commit.filter(|&d| d < today) {
        parts.push(format!("last commit {}", last.format("%b %-d")));
    }
    if p.total > 0 {
        parts.push(format!("{} in all", projects::count(p.total)));
    }
    if !p.project.has_goal() {
        parts.push("no daily goal".into());
    }
    parts.push(p.project.path.display().to_string());
    println!("    {}", dim(&parts.join("  ·  ")));
}

pub fn projects() -> Result<()> {
    let (session, snap) = session()?;
    if snap.projects.is_empty() {
        println!(
            "No projects yet. Put git repositories in {}.",
            session.cfg.project_dirs.join(", ")
        );
    }
    for p in &snap.projects {
        print_project(p);
        print_project_details(p, snap.today);
    }
    Ok(())
}

/// Shows a project, or sets its daily goal in commits (0 turns it off).
pub fn project(query: String, goal: Option<u32>) -> Result<()> {
    let (session, snap) = session()?;
    let found = one(snap.find_projects(&query), "project", &query, |p| {
        p.project.name.clone()
    })?;
    let id = found.project.id;
    if let Some(goal) = goal {
        projects::set_goal(&session.conn, id, goal.into())?;
    }
    let snap = session.snapshot()?;
    let status = snap
        .projects
        .iter()
        .find(|p| p.project.id == id)
        .context("project vanished")?;
    print_project(status);
    print_project_details(status, snap.today);
    Ok(())
}

pub fn habits() -> Result<()> {
    let (_, snap) = session()?;
    if snap.habits.is_empty() {
        println!("No habits yet. Add them on the Habits page: patrak gui --page habits");
    }
    for h in &snap.habits {
        print_habit(h);
    }
    Ok(())
}

pub fn habit(query: String, amount: Option<String>) -> Result<()> {
    let (session, snap) = session()?;
    let found = one(snap.find_habits(&query), "habit", &query, |h| {
        h.habit.name.clone()
    })?;
    let id = found.habit.id;
    let amount = parse_amount(amount.as_deref(), found.today, &found.habit)?;
    habits::set_amount(&session.conn, id, snap.today, amount)?;
    let snap = session.snapshot()?;
    let status = snap
        .habits
        .iter()
        .find(|h| h.habit.id == id)
        .context("habit vanished")?;
    print_habit(status);
    Ok(())
}

/// Today's new amount: `3` sets it, `+2` and `-1` change it. With no amount, a count goes up
/// by one and a minutes habit is completed.
fn parse_amount(arg: Option<&str>, current: i64, habit: &Habit) -> Result<i64> {
    let Some(arg) = arg.map(str::trim) else {
        return Ok(habit.quick_amount(current));
    };
    let number = |s: &str| {
        s.parse::<i64>()
            .with_context(|| format!("{arg:?} isn't an amount like 3, +2 or -1"))
    };
    let amount = if let Some(n) = arg.strip_prefix('+') {
        current + number(n)?
    } else if let Some(n) = arg.strip_prefix('-') {
        current - number(n)?
    } else {
        number(arg)?
    };
    Ok(amount.max(0))
}

pub fn status(query: Option<String>) -> Result<()> {
    let (_, snap) = session()?;
    let Some(query) = query else {
        print_overview(&snap);
        return Ok(());
    };
    let course = one(snap.find_courses(&query), "course", &query, |c| {
        c.name.clone()
    })?;
    print_course(course, snap.today);
    Ok(())
}

fn print_overview(snap: &Snapshot) {
    // Names go last so right-to-left and wide titles can't break the alignment.
    println!("{}", bold("Courses"));
    if snap.courses.is_empty() {
        println!("  {}", dim("none"));
    }
    for c in &snap.courses {
        let eta = c
            .eta
            .map(|d| format!("  ETA {}", d.format("%b %d")))
            .unwrap_or_default();
        let left = if c.remaining_secs > 0.0 {
            format!("  {} left", fmt_duration(c.remaining_secs))
        } else {
            String::new()
        };
        println!(
            "  {:>3.0}% {}  {:>3}/{:<3}{}{}  {}",
            c.fraction * 100.0,
            bar(c.fraction, 12),
            c.done,
            c.lectures.len(),
            left,
            dim(&eta),
            bold(&format!(
                "{}{}",
                c.name,
                if c.missing { " (missing)" } else { "" }
            ))
        );
        if let Some(summary) = c.plan_summary(snap.today) {
            println!("        {}", dim(&summary));
        }
        if let Some(source) = c.source.as_ref().filter(|s| !s.new_titles.is_empty()) {
            println!(
                "        {}",
                green(&format!(
                    "{} new episodes · patrak update",
                    source.new_titles.len()
                ))
            );
        }
    }
    println!("\n{}", bold("Books"));
    if snap.books.is_empty() {
        println!("  {}", dim("none"));
    }
    for b in &snap.books {
        let f = b.fraction().unwrap_or(0.0);
        println!(
            "  {:>3.0}% {}  {}  {}",
            f * 100.0,
            bar(f, 12),
            dim(&item_status(b)),
            bold(&format!(
                "{}{}",
                b.title,
                if b.missing { " (missing)" } else { "" }
            ))
        );
    }
}

fn print_course(c: &Course, today: NaiveDate) {
    println!(
        "{}  {}/{} lectures · {:.0}% · {} left",
        bold(&c.name),
        c.done,
        c.lectures.len(),
        c.fraction * 100.0,
        fmt_duration(c.remaining_secs)
    );
    if let Some(summary) = c.plan_summary(today) {
        println!("{}", dim(&summary));
    }
    if let Some(source) = &c.source {
        let new = match source.new_titles.len() {
            0 => "no new episodes at last check".to_owned(),
            n => format!("{n} new episodes · patrak update"),
        };
        println!("{}", dim(&format!("{}  ·  {new}", source.url)));
    }
    for (i, l) in c.lectures.iter().enumerate() {
        let arrow = if Some(i) == c.next {
            "→"
        } else if l.missing {
            "✗"
        } else {
            " "
        };
        println!(
            "{arrow} {:>3}  {:<26} {}",
            i + 1,
            item_status(l),
            if l.is_done() {
                dim(&l.title)
            } else {
                l.title.clone()
            }
        );
    }
    if let [lecture] = c.lectures.as_slice() {
        let current = lecture.current_chapter().map(|(i, _)| i);
        for (i, ch) in lecture.chapters.iter().enumerate() {
            let mark = if Some(i) == current {
                "▶"
            } else if lecture.position > ch.start || lecture.is_done() {
                "✓"
            } else {
                "○"
            };
            println!("      {mark} {:>8}  {}", fmt_clock(ch.start), ch.title);
        }
    }
}

pub fn next(query: Option<String>) -> Result<()> {
    let (_, snap) = session()?;
    let (course, start) = pick_course(&snap, query.as_deref())?;
    let lecture = &course.lectures[start];
    println!("{}  {}", bold(&lecture.title), dim(&item_status(lecture)));
    println!("{}", dim(&course.name));
    if let Some((i, ch)) = lecture.current_chapter() {
        println!("chapter {}/{}: {}", i + 1, lecture.chapters.len(), ch.title);
    }
    Ok(())
}

pub fn play(query: Option<String>) -> Result<()> {
    let (mut session, snap) = session()?;
    let (course, start) = pick_course(&snap, query.as_deref())?;
    anyhow::ensure!(
        !course.lectures[start].missing,
        "{} is missing from disk",
        course.lectures[start].title
    );
    println!("▶ {} — {}", course.name, course.lectures[start].title);
    run(launch::play_args(course, start))?;
    after_launch(&mut session)
}

pub fn read(query: Option<String>) -> Result<()> {
    let (mut session, snap) = session()?;
    let book = match query {
        Some(q) => one(snap.find_books(&q), "book", &q, |b| b.title.clone())?,
        None => snap
            .last_book()
            .or(snap.books.iter().find(|b| !b.missing))
            .context("no books found in book_dirs")?,
    };
    anyhow::ensure!(!book.missing, "{} is missing from disk", book.title);
    println!("📖 {}", book.title);
    run(launch::read_args(book))?;
    after_launch(&mut session)
}

pub fn note(query: Option<String>) -> Result<()> {
    let (_, snap) = session()?;
    let item = match query {
        Some(q) => pick_item(&snap, &q)?,
        None => snap
            .current_course()
            .and_then(Course::next_lecture)
            .context("no current lecture; name one: patrak note <lecture>")?,
    };
    let path = launch::ensure_note(item)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    run(vec![editor.into(), path.into_os_string()])?;
    Ok(())
}

pub fn mark(query: String, done: bool) -> Result<()> {
    let (session, snap) = session()?;
    let item = pick_item(&snap, &query)?;
    set_done(&session.conn, item.id, done)?;
    println!(
        "{} {}",
        if done { "✓ done:" } else { "· not done:" },
        item.title
    );
    Ok(())
}

pub fn stats() -> Result<()> {
    let (_, snap) = session()?;
    const WEEKS: i64 = 26;
    let start = snap.today
        - Duration::days(snap.today.weekday().num_days_from_monday() as i64)
        - Duration::weeks(WEEKS - 1);
    println!("{}", bold("Last 26 weeks"));
    for (row, name) in ["Mon", "", "Wed", "", "Fri", "", "Sun"].iter().enumerate() {
        let mut line = format!("  {name:<4}");
        for week in 0..WEEKS {
            let day = start + Duration::weeks(week) + Duration::days(row as i64);
            if day > snap.today {
                line.push(' ');
                continue;
            }
            let shade = if snap.is_holiday(day) {
                "135"
            } else {
                ["238", "22", "28", "34", "40"][snap.heat_level(day) as usize]
            };
            line.push_str(&paint(&format!("38;5;{shade}"), "■"));
        }
        println!("{line}");
    }
    println!(
        "  {}  {} {}",
        dim("less ■ ■ ■ ■ ■ more   (full square = daily goals met)"),
        paint("38;5;135", "■"),
        dim("holiday")
    );

    println!("\n{}", bold("Weekly totals"));
    let weeks = snap.weekly(8);
    let max = weeks
        .iter()
        .map(|w| w.1.minutes)
        .fold(snap.goals.minutes * 7.0, f64::max)
        .max(1.0);
    for (monday, week) in weeks {
        let pages = if week.pages > 0 {
            dim(&format!("  {} pages", week.pages))
        } else {
            String::new()
        };
        let commits = if week.commits > 0 {
            dim(&format!("  {}", projects::count(week.commits)))
        } else {
            String::new()
        };
        println!(
            "  {}  {} {:>5.0} min{pages}{commits}",
            monday.format("%b %d"),
            bar(week.minutes / max, 30),
            week.minutes
        );
    }

    let total: Day = snap.days.values().copied().sum();
    let days = snap.days.values().filter(|d| !d.is_empty()).count();
    println!(
        "\nstreak {}  ·  best {}  ·  {} studied, {} pages read, {} over {days} days",
        bold(&snap.streak.to_string()),
        snap.best_streak,
        fmt_duration(total.minutes * 60.0),
        total.pages,
        projects::count(total.commits)
    );
    Ok(())
}

pub fn sync(verbose: bool) -> Result<()> {
    let mut session = Session::open()?;
    let report = session.sync()?;
    if verbose {
        for line in &report.changes {
            println!("{line}");
        }
    }
    println!(
        "synced: {} new files, {} newly finished",
        report.new_items,
        report.completed.len()
    );
    Ok(())
}

pub fn remind() -> Result<()> {
    let (_, snap) = session()?;
    let (studied, goals) = (snap.studied_today(), snap.goals);
    if snap.holiday_today() || goals.met(studied) {
        return Ok(());
    }
    let mut short = Vec::new();
    if studied.minutes < goals.minutes {
        short.push(format!(
            "{:.0} of {:.0} minutes",
            studied.minutes, goals.minutes
        ));
    }
    if (studied.pages as f64) < goals.pages {
        short.push(format!("{} of {:.0} pages", studied.pages, goals.pages));
    }
    for h in snap.habits.iter().filter(|h| !h.done()) {
        short.push(format!(
            "{} {}",
            h.habit.name,
            h.habit.progress_text(h.today)
        ));
    }
    for p in snap
        .projects
        .iter()
        .filter(|p| p.project.has_goal() && !p.project.missing && !p.done())
    {
        short.push(format!(
            "{} {}",
            p.project.name,
            p.project.progress_text(p.today)
        ));
    }
    let mut body = if short.is_empty() {
        "Nothing studied yet today.".to_owned()
    } else {
        format!("{} so far.", short.join(" and "))
    };
    if let Some(l) = snap.current_course().and_then(Course::next_lecture) {
        body.push_str(&format!("\nNext: {}", l.title));
    }
    let handle = notify_rust::Notification::new()
        .appname("patrak")
        .summary("Time to learn")
        .body(&body)
        .icon("accessories-dictionary-symbolic")
        .action("default", "Open patrak")
        .show()?;
    handle.wait_for_action(|action| {
        if action == "default"
            && let Ok(exe) = std::env::current_exe()
        {
            let _ = Command::new(exe).spawn();
        }
    });
    Ok(())
}

/// Makes today a holiday (`on`), a study day again (`off`), or toggles it.
pub fn holiday(state: Option<String>) -> Result<()> {
    let (session, snap) = session()?;
    let on = match state.as_deref() {
        Some("on") => true,
        Some("off") => false,
        _ => !snap.holiday_today(),
    };
    db::set_holiday(&session.conn, snap.today, on)?;
    print_today(&session.snapshot()?);
    Ok(())
}

pub fn setup(force: bool) -> Result<()> {
    let session = Session::open()?;
    let cfg = if session.paths.config.exists() && !force {
        session.cfg.clone()
    } else {
        Config::default()
    };
    println!("{}", setup::write_config(&session.paths, &cfg, force)?);
    for dir in setup::create_library_dirs(&cfg)? {
        println!("library {}", dir.display());
    }
    println!("{}", setup::install_mpv_script(&session.paths, force)?);
    println!("{}", setup::install_desktop_entry(force)?);
    match setup::install_reminder(&cfg) {
        Ok(units) => {
            for u in units {
                println!("{u}");
            }
            println!("reminder at {} every day", cfg.reminder_time);
        }
        Err(e) => eprintln!("reminder timer not installed: {e:#}"),
    }
    println!(
        "\nPut course folders in {}, books in {} and git repositories in {}.",
        cfg.video_dirs.join(", "),
        cfg.book_dirs.join(", "),
        cfg.project_dirs.join(", ")
    );
    Ok(())
}

pub fn deadline(query: String, when: String) -> Result<()> {
    let (session, snap) = session()?;
    let course = one(snap.find_courses(&query), "course", &query, |c| {
        c.name.clone()
    })?;
    let date = parse_when(&when, snap.today)?;
    db::set_deadline(&session.conn, &course.key, date)?;
    let snap = session.snapshot()?;
    let course = snap
        .courses
        .iter()
        .find(|c| c.key == course.key)
        .context("course vanished")?;
    match course.plan_summary(snap.today) {
        Some(summary) => println!("{}\n{}", bold(&course.name), summary),
        None => println!("{}: no deadline", course.name),
    }
    Ok(())
}

/// `2026-11-15`, `+10` / `+10d` (days), `+3w` (weeks), `+2m` (months of 30 days), or `clear`.
fn parse_when(when: &str, today: NaiveDate) -> Result<Option<NaiveDate>> {
    let when = when.trim().to_lowercase();
    if matches!(when.as_str(), "clear" | "none" | "off") {
        return Ok(None);
    }
    if let Some(rel) = when.strip_prefix('+') {
        let (n, unit) = rel.split_at(rel.find(|c: char| !c.is_ascii_digit()).unwrap_or(rel.len()));
        let n: i64 = n.parse().with_context(|| format!("can't read {when:?}"))?;
        let days = match unit {
            "" | "d" => n,
            "w" => n * 7,
            "m" => n * 30,
            _ => bail!("unknown unit in {when:?}; use d, w or m"),
        };
        return Ok(Some(today + Duration::days(days)));
    }
    NaiveDate::parse_from_str(&when, "%Y-%m-%d")
        .map(Some)
        .with_context(|| format!("{when:?} isn't a date like 2026-11-15, +3w or clear"))
}

pub fn add(url: String, name: Option<String>) -> Result<()> {
    let mut session = Session::open()?;
    println!("{}", dim(&format!("Looking up {url} …")));
    let plan = download::plan(&session.cfg, &url, name.as_deref())?;
    println!("Downloading {}", bold(&plan.describe()));
    download::prepare(&plan)?;
    if !run(download::download_args(&session.cfg, &plan, false))? {
        bail!("download stopped; run `patrak update` to resume it");
    }
    download::finish(&plan)?;
    after_launch(&mut session)
}

pub fn update(query: Option<String>, check_only: bool) -> Result<()> {
    let mut session = Session::open()?;
    let dirs: Vec<PathBuf> = download::source_dirs(&session.cfg)
        .into_iter()
        .filter(|d| {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            query
                .as_deref()
                .is_none_or(|q| name.contains(&q.to_lowercase()))
        })
        .collect();
    if dirs.is_empty() {
        bail!("no downloaded courses match; add one with `patrak add <playlist-url>`");
    }
    let mut downloaded = false;
    for dir in dirs {
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let new =
            download::check(&session.cfg, &dir).with_context(|| format!("checking {name}"))?;
        if new.is_empty() {
            println!("{}  {}", bold(&name), dim("up to date"));
            continue;
        }
        println!("{}  {}", bold(&name), green(&format!("{} new", new.len())));
        for title in &new {
            println!("  + {title}");
        }
        if check_only {
            continue;
        }
        let plan = download::update_plan(&dir).context("course has no source")?;
        if run(download::download_args(&session.cfg, &plan, false))? {
            download::finish(&plan)?;
            downloaded = true;
        }
    }
    if downloaded {
        after_launch(&mut session)?;
    }
    Ok(())
}

pub fn review() -> Result<()> {
    let (session, snap) = session()?;
    let mut queue: VecDeque<Card> = review::due(&session.conn, snap.today)?.into();
    if queue.is_empty() {
        match snap.cards_total {
            0 => println!(
                "No review cards yet. In a lecture's notes (patrak note), write\n\n  Q: a question\n  A: its answer\n"
            ),
            n => println!(
                "Nothing due. {n} cards, {} reviewed today.",
                snap.reviewed_today
            ),
        }
        return Ok(());
    }
    let total = queue.len();
    let mut finished = 0;
    let keys = "[1] again  [2] hard  [3] good  [4] easy  [q] stop";
    while let Some(card) = queue.pop_front() {
        println!(
            "\n{}  {}",
            dim(&format!("{}/{total}", finished + 1)),
            dim(&card.source())
        );
        println!("{}", bold(&card.question));
        let shown = Instant::now();
        if prompt(&dim("Enter shows the answer, q stops"))?.is_none_or(|l| l == "q") {
            break;
        }
        println!("{}\n", card.answer);
        let when: Vec<String> = Grade::ALL
            .iter()
            .map(|g| format!("{} {}", g.label(), card.preview(*g)))
            .collect();
        println!("{}", dim(&when.join(" · ")));
        let grade = loop {
            match prompt(keys)? {
                None => return Ok(()),
                Some(l) if l == "q" => return Ok(()),
                Some(l) => match l.chars().next().and_then(Grade::from_key) {
                    Some(g) => break g,
                    None => continue,
                },
            }
        };
        let next = review::grade(
            &session.conn,
            &card,
            grade,
            snap.today,
            shown.elapsed().as_secs_f64(),
        )?;
        if grade == Grade::Again {
            queue.push_back(next);
        } else {
            finished += 1;
        }
    }
    println!("\nReviewed {finished} of {total} cards.");
    Ok(())
}

/// Prints `text` and reads a trimmed line; `None` at end of input.
fn prompt(text: &str) -> Result<Option<String>> {
    print!("{text} › ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    Ok((std::io::stdin().read_line(&mut line)? > 0).then(|| line.trim().to_lowercase()))
}

/// The course to play and the lecture index to start from.
fn pick_course<'a>(snap: &'a Snapshot, query: Option<&str>) -> Result<(&'a Course, usize)> {
    let Some(q) = query else {
        let course = snap.current_course().context("nothing left to watch")?;
        return Ok((course, course.next.unwrap_or(0)));
    };
    let courses = snap.find_courses(q);
    if !courses.is_empty() {
        let course = one(courses, "course", q, |c| c.name.clone())?;
        return Ok((course, course.next.unwrap_or(0)));
    }
    let lecture = one(snap.find_lectures(q), "course or lecture", q, |l| {
        format!("{} ({})", l.title, l.course)
    })?;
    let course = snap
        .course_of(lecture)
        .context("lecture without a course")?;
    let start = course
        .lectures
        .iter()
        .position(|l| l.id == lecture.id)
        .unwrap_or(0);
    Ok((course, start))
}

fn pick_item<'a>(snap: &'a Snapshot, query: &str) -> Result<&'a Item> {
    let mut found = snap.find_lectures(query);
    found.extend(snap.find_books(query));
    one(found, "lecture or book", query, |i| {
        format!("{} ({})", i.title, i.course)
    })
}

fn one<'a, T>(
    found: Vec<&'a T>,
    what: &str,
    query: &str,
    label: impl Fn(&T) -> String,
) -> Result<&'a T> {
    match found.as_slice() {
        [single] => Ok(single),
        [] => bail!("no {what} matches {query:?}"),
        many => {
            let list: Vec<String> = many.iter().map(|t| format!("  {}", label(t))).collect();
            bail!(
                "{query:?} matches more than one {what}:\n{}",
                list.join("\n")
            )
        }
    }
}

/// Runs a program in the foreground; returns whether it succeeded.
fn run(args: Vec<OsString>) -> Result<bool> {
    let (program, rest) = args.split_first().context("empty command")?;
    let status = Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("starting {}", program.to_string_lossy()))?;
    if !status.success() {
        eprintln!("{} exited with {status}", program.to_string_lossy());
    }
    Ok(status.success())
}

fn after_launch(session: &mut Session) -> Result<()> {
    let report = session.sync()?;
    for title in &report.completed {
        println!("{} {title}", green("✓ finished"));
    }
    println!();
    print_today(&session.snapshot()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use patrak::habits::Unit;

    #[test]
    fn reads_deadlines() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();
        let d = |s| Some(NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap());
        assert_eq!(parse_when("2026-11-15", today).unwrap(), d("2026-11-15"));
        assert_eq!(parse_when("+10", today).unwrap(), d("2026-10-21"));
        assert_eq!(parse_when("+3w", today).unwrap(), d("2026-11-01"));
        assert_eq!(parse_when("+1m", today).unwrap(), d("2026-11-10"));
        assert_eq!(parse_when("clear", today).unwrap(), None);
        assert!(parse_when("+3y", today).is_err());
        assert!(parse_when("next friday", today).is_err());
    }

    #[test]
    fn reads_habit_amounts() {
        let habit = |unit| Habit {
            id: 1,
            name: "Run".into(),
            unit,
            goal: 30,
            created: NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        };
        let (count, minutes) = (habit(Unit::Count), habit(Unit::Minutes));
        assert_eq!(parse_amount(None, 2, &count).unwrap(), 3);
        assert_eq!(parse_amount(None, 10, &minutes).unwrap(), 30);
        assert_eq!(parse_amount(None, 45, &minutes).unwrap(), 45);
        assert_eq!(parse_amount(Some("+15"), 10, &minutes).unwrap(), 25);
        assert_eq!(parse_amount(Some("-1"), 2, &count).unwrap(), 1);
        assert_eq!(parse_amount(Some("-5"), 2, &count).unwrap(), 0);
        assert_eq!(parse_amount(Some("4"), 2, &count).unwrap(), 4);
        assert!(parse_amount(Some("lots"), 2, &count).is_err());
    }
}
