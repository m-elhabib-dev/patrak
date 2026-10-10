use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration};
use patrak::Session;
use patrak::db::{Item, Kind};
use patrak::progress::{Course, Snapshot, fmt_clock, fmt_duration, item_status};
use patrak::{config::Config, launch, setup, sync::set_done};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::Command;
use std::sync::OnceLock;

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

fn print_today(snap: &Snapshot) {
    let mins = snap.today_minutes();
    println!(
        "{}  {}  {} / {} min   streak {} (best {})",
        bold(&snap.today.format("%a %d %b").to_string()),
        bar(mins / snap.goal, 24),
        bold(&format!("{mins:.0}")),
        snap.goal,
        bold(&snap.streak.to_string()),
        snap.best_streak,
    );

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

pub fn status(query: Option<String>) -> Result<()> {
    let (_, snap) = session()?;
    let Some(query) = query else {
        print_overview(&snap);
        return Ok(());
    };
    let course = one(snap.find_courses(&query), "course", &query, |c| {
        c.name.clone()
    })?;
    print_course(course);
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
            bold(&c.name)
        );
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
            bold(&b.title)
        );
    }
}

fn print_course(c: &Course) {
    println!(
        "{}  {}/{} lectures · {:.0}% · {} left",
        bold(&c.name),
        c.done,
        c.lectures.len(),
        c.fraction * 100.0,
        fmt_duration(c.remaining_secs)
    );
    for (i, l) in c.lectures.iter().enumerate() {
        let arrow = if Some(i) == c.next { "→" } else { " " };
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
                "·"
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
            .or(snap.books.first())
            .context("no books found in book_dirs")?,
    };
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
    run(vec![editor.into(), path.into_os_string()])
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
            let shade = ["238", "22", "28", "34", "40"][snap.heat_level(day) as usize];
            line.push_str(&paint(&format!("38;5;{shade}"), "■"));
        }
        println!("{line}");
    }
    println!(
        "  {}",
        dim("less ■ ■ ■ ■ ■ more   (full square = daily goal met)")
    );

    println!("\n{}", bold("Weekly minutes"));
    let weeks = snap.weekly_minutes(8);
    let max = weeks.iter().map(|w| w.1).fold(snap.goal * 7.0, f64::max);
    for (monday, mins) in weeks {
        println!(
            "  {}  {} {mins:>5.0}",
            monday.format("%b %d"),
            bar(mins / max, 30)
        );
    }

    let total: f64 = snap.minutes_by_day.values().sum();
    let days = snap.minutes_by_day.values().filter(|&&m| m > 0.0).count();
    println!(
        "\nstreak {}  ·  best {}  ·  {} studied over {days} days",
        bold(&snap.streak.to_string()),
        snap.best_streak,
        fmt_duration(total * 60.0)
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
    let mins = snap.today_minutes();
    if mins >= snap.goal {
        return Ok(());
    }
    let mut body = format!("{mins:.0} of {} minutes so far.", snap.goal);
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
        "\nPut course folders in {} and books in {}.",
        cfg.video_dirs.join(", "),
        cfg.book_dirs.join(", ")
    );
    Ok(())
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

fn run(args: Vec<OsString>) -> Result<()> {
    let (program, rest) = args.split_first().context("empty command")?;
    let status = Command::new(program)
        .args(rest)
        .status()
        .with_context(|| format!("starting {}", program.to_string_lossy()))?;
    if !status.success() {
        eprintln!("{} exited with {status}", program.to_string_lossy());
    }
    Ok(())
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
