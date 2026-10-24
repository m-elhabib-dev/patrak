use patrak::config::{Config, Paths};
use patrak::db;
use patrak::progress;
use patrak::sources::watch_later;
use patrak::sync::{set_done, sync};
use rusqlite::Connection;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    cfg: Config,
    paths: Paths,
    conn: Connection,
}

impl Fixture {
    fn new(with_script: bool) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        for f in [
            "Videos/Rust/01 Intro.mp4",
            "Videos/Rust/02 Ownership.mp4",
            "Videos/Rust/03 Traits.mp4",
            "Books/book.pdf",
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, b"not really media").unwrap();
        }
        fs::create_dir_all(root.join("watch_later")).unwrap();
        let zathura = Connection::open(root.join("zathura.sqlite")).unwrap();
        zathura
            .execute_batch(
                "CREATE TABLE fileinfo (file TEXT PRIMARY KEY, page INTEGER, time TIMESTAMP);",
            )
            .unwrap();
        let events_log = root.join("events.jsonl");
        if with_script {
            fs::write(&events_log, "").unwrap();
        }
        let cfg = Config {
            video_dirs: vec![root.join("Videos").to_string_lossy().into()],
            book_dirs: vec![root.join("Books").to_string_lossy().into()],
            project_dirs: vec![root.join("Projects").to_string_lossy().into()],
            mpv_watch_later_dir: root.join("watch_later").to_string_lossy().into(),
            zathura_db: root.join("zathura.sqlite").to_string_lossy().into(),
            day_starts_at: "00:00".into(),
            ..Config::default()
        };
        let paths = Paths {
            config: root.join("config.toml"),
            data_dir: root.clone(),
            events_log,
        };
        Self {
            conn: db::open_in_memory().unwrap(),
            _tmp: tmp,
            root,
            cfg,
            paths,
        }
    }

    fn video(&self, name: &str) -> PathBuf {
        self.root.join("Videos/Rust").join(name)
    }

    fn resume(&self, video: &Path, start: f64, mtime: SystemTime) {
        let file = self
            .root
            .join("watch_later")
            .join(watch_later::file_name(video.to_str().unwrap()));
        fs::write(&file, format!("start={start}\n")).unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    }

    fn event(&self, json: serde_json::Value) {
        let mut f = fs::File::options()
            .append(true)
            .open(&self.paths.events_log)
            .unwrap();
        writeln!(f, "{json}").unwrap();
    }

    fn zathura_page(&self, page0: i64, utc: &str) {
        let book = self.root.join("Books/book.pdf");
        Connection::open(self.root.join("zathura.sqlite"))
            .unwrap()
            .execute(
                "REPLACE INTO fileinfo (file, page, time) VALUES (?1, ?2, ?3)",
                rusqlite::params![book.to_str().unwrap(), page0, utc],
            )
            .unwrap();
    }

    fn sync(&mut self) -> patrak::sync::Report {
        sync(&mut self.conn, &self.cfg, &self.paths).unwrap()
    }

    fn snapshot(&self) -> progress::Snapshot {
        progress::build(&self.conn, &self.cfg).unwrap()
    }
}

fn utc_now_offset(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

#[test]
fn first_sync_records_a_baseline_then_counts_new_progress() {
    let mut fx = Fixture::new(true);
    let now = chrono::Utc::now().timestamp();
    fx.resume(
        &fx.video("02 Ownership.mp4"),
        100.0,
        SystemTime::now() - Duration::from_secs(3600),
    );
    fx.zathura_page(9, &utc_now_offset(-3600));

    let report = fx.sync();
    assert_eq!(report.new_items, 4);
    assert!(report.completed.is_empty());
    let snap = fx.snapshot();
    assert!(
        snap.studied_today().is_empty(),
        "existing progress is a baseline, not today's work"
    );
    let course = &snap.courses[0];
    assert_eq!(course.name, "Rust");
    assert_eq!(course.lectures[1].position, 100.0);
    assert_eq!(course.next_lecture().unwrap().title, "02 Ownership");
    assert_eq!(snap.books[0].position, 10.0);

    // Watch lecture 1 to the end with the mpv script running, and read 20 pages.
    let intro = fx.video("01 Intro.mp4");
    fx.event(serde_json::json!({"ts": now, "path": intro, "pos": 120.0, "duration": 600.0, "watched": 120.0, "eof": false}));
    fx.event(serde_json::json!({"ts": now + 10, "path": intro, "pos": 599.0, "duration": 600.0, "watched": 480.0, "eof": true}));
    fx.zathura_page(29, &utc_now_offset(0));

    let report = fx.sync();
    assert_eq!(report.completed, ["01 Intro"]);
    let snap = fx.snapshot();
    // 600 s of video and 20 pages.
    assert_eq!(snap.studied_today().minutes, 10.0);
    assert_eq!(snap.studied_today().pages, 20);
    let course = &snap.courses[0];
    assert!(course.lectures[0].is_done());
    assert_eq!(course.lectures[0].duration, Some(600.0));
    assert_eq!(course.next_lecture().unwrap().title, "02 Ownership");
    assert_eq!(snap.books[0].position, 30.0);

    // The same log lines are not counted twice.
    fx.sync();
    let today = fx.snapshot().studied_today();
    assert_eq!((today.minutes, today.pages), (10.0, 20));

    // Clearing "done" sticks even though the saved position is at the end.
    let id = course.lectures[0].id;
    set_done(&fx.conn, id, false).unwrap();
    fx.sync();
    assert!(!fx.snapshot().item(id).unwrap().is_done());
}

#[test]
fn without_the_script_watch_later_moves_count_as_watch_time() {
    let mut fx = Fixture::new(false);
    let video = fx.video("03 Traits.mp4");
    fx.sync();

    fx.resume(&video, 300.0, SystemTime::now());
    fx.sync();
    assert_eq!(fx.snapshot().studied_today().minutes, 5.0);

    // Moving backwards (rewatching) updates the position without adding time.
    fx.resume(&video, 60.0, SystemTime::now() + Duration::from_secs(5));
    fx.sync();
    let snap = fx.snapshot();
    assert_eq!(snap.studied_today().minutes, 5.0);
    assert_eq!(snap.courses[0].lectures[2].position, 60.0);
}

#[test]
fn deleted_files_keep_their_progress_but_cannot_be_played() {
    let mut fx = Fixture::new(true);
    fx.sync();
    let intro = fx.video("01 Intro.mp4");
    let id = fx.snapshot().courses[0].lectures[0].id;
    set_done(&fx.conn, id, true).unwrap();
    fs::remove_file(&intro).unwrap();
    fs::remove_file(fx.video("03 Traits.mp4")).unwrap();
    fx.sync();

    let snap = fx.snapshot();
    let course = &snap.courses[0];
    assert_eq!(course.lectures.len(), 3);
    assert!(course.lectures[0].missing && course.lectures[0].is_done());
    assert_eq!(course.done, 1);
    assert!(!course.missing);
    // Only the lecture still on disk can be next, and only it goes to mpv.
    assert_eq!(course.next_lecture().unwrap().title, "02 Ownership");
    let args = patrak::launch::play_args(course, 1);
    assert_eq!(args.len(), 3);
    assert_eq!(args[1], "--playlist-start=0");

    // The whole course gone: still listed, nothing to continue.
    fs::remove_dir_all(fx.root.join("Videos/Rust")).unwrap();
    fx.sync();
    let snap = fx.snapshot();
    assert!(snap.courses[0].missing && snap.courses[0].next.is_none());
    assert_eq!(snap.courses[0].done, 1);
    assert!(snap.current_course().is_none());
}

#[test]
fn moved_files_keep_their_progress() {
    let mut fx = Fixture::new(true);
    fx.sync();
    let before = fx.snapshot();
    let ids: Vec<i64> = before.courses[0].lectures.iter().map(|l| l.id).collect();
    let (intro, own) = (
        before.courses[0].lectures[0].id,
        before.courses[0].lectures[1].id,
    );
    set_done(&fx.conn, intro, true).unwrap();
    db::set_position(&fx.conn, own, 42.0, 1000).unwrap();
    db::add_daily(&fx.conn, chrono::Local::now().date_naive(), intro, 300.0, 0).unwrap();
    db::set_deadline(
        &fx.conn,
        fx.root.join("Videos/Rust").to_str().unwrap(),
        Some(chrono::Local::now().date_naive() + chrono::Duration::days(9)),
    )
    .unwrap();

    // Rename the course folder; every lecture has the same size and mtime, so only their names tell them apart.
    fs::rename(fx.root.join("Videos/Rust"), fx.root.join("Videos/Rust 2")).unwrap();
    let report = fx.sync();
    assert_eq!(report.new_items, 0);
    assert_eq!(
        report
            .changes
            .iter()
            .filter(|c| c.starts_with("moved"))
            .count(),
        3
    );

    let snap = fx.snapshot();
    assert_eq!(snap.courses.len(), 1);
    let course = &snap.courses[0];
    assert_eq!(course.name, "Rust 2");
    assert_eq!(
        course.lectures.iter().map(|l| l.id).collect::<Vec<_>>(),
        ids
    );
    assert!(course.lectures[0].is_done() && !course.lectures[0].missing);
    assert_eq!(course.lectures[1].position, 42.0);
    assert_eq!(snap.studied_today().minutes, 5.0);
    assert!(course.deadline.is_some());

    // A book moved to another shelf too.
    fs::create_dir_all(fx.root.join("Books/shelf")).unwrap();
    fs::rename(
        fx.root.join("Books/book.pdf"),
        fx.root.join("Books/shelf/book.pdf"),
    )
    .unwrap();
    let book = fx.snapshot().books[0].id;
    fx.sync();
    let snap = fx.snapshot();
    assert_eq!((snap.books.len(), snap.books[0].id), (1, book));
}

#[test]
fn files_that_differ_are_not_mistaken_for_moved_ones() {
    let mut fx = Fixture::new(true);
    fx.sync();
    fs::remove_file(fx.video("03 Traits.mp4")).unwrap();
    fs::write(fx.video("04 Other.mp4"), b"a different, longer file").unwrap();
    let report = fx.sync();
    assert_eq!(report.new_items, 1);
    let lectures = &fx.snapshot().courses[0].lectures;
    assert_eq!(lectures.len(), 4);
    assert!(
        lectures
            .iter()
            .find(|l| l.title == "03 Traits")
            .unwrap()
            .missing
    );
}

/// Runs git in `dir` without the user's own config, so signing and hooks can't get in the way.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

#[test]
fn projects_count_the_users_own_commits() {
    let mut fx = Fixture::new(true);
    let repo = fx.root.join("Projects/app");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "Me"]);
    git(&repo, &["config", "user.email", "me@example.com"]);
    // Commits are told apart by author time, so give each its own second.
    let now = chrono::Utc::now().timestamp();
    let next = std::cell::Cell::new(now - 20);
    let commit = |message: &str, extra: &[&str]| {
        let date = format!("--date={} +0000", next.replace(next.get() + 1));
        let mut args = vec!["commit", "-q", "--allow-empty", "-m", message, &date];
        args.extend(extra);
        git(&repo, &args);
    };
    commit("one", &[]);
    commit("theirs", &["--author", "Someone <someone@example.com>"]);
    git(&repo, &["checkout", "-q", "-b", "feature"]);
    commit("two", &[]);
    git(&repo, &["checkout", "-q", "main"]);
    git(&repo, &["merge", "-q", "--no-ff", "-m", "merge", "feature"]);

    let report = fx.sync();
    assert!(report.changes.contains(&"project  app".to_owned()));
    let snap = fx.snapshot();
    let today = snap.studied_today();
    // Not the other author's commit, and not the merge.
    assert_eq!(today.commits, 2);
    assert_eq!((today.projects_due, today.projects_met), (1, 1));
    let app = &snap.projects[0];
    assert_eq!((app.project.name.as_str(), app.today), ("app", 2));
    assert_eq!(app.project.added, snap.today);

    // Throwing commits away doesn't take them back.
    git(&repo, &["reset", "-q", "--hard", "HEAD~1"]);
    git(&repo, &["branch", "-q", "-D", "feature"]);
    fx.sync();
    assert_eq!(fx.snapshot().studied_today().commits, 2);

    commit("three", &[]);
    fx.sync();
    assert_eq!(fx.snapshot().projects[0].today, 3);
    // An amended commit keeps its author time, so it isn't counted twice.
    git(
        &repo,
        &[
            "commit",
            "-q",
            "--amend",
            "--allow-empty",
            "-m",
            "three, amended",
        ],
    );
    fx.sync();
    assert_eq!(fx.snapshot().projects[0].today, 3);

    // Moving the repository keeps the project: same goal, same history, no double counting.
    let id = fx.snapshot().projects[0].project.id;
    db_set_goal(&fx, id, 2);
    let moved = fx.root.join("Projects/renamed");
    fs::rename(&repo, &moved).unwrap();
    let report = fx.sync();
    assert!(report.changes.contains(&"moved    renamed".to_owned()));
    let snap = fx.snapshot();
    assert_eq!(snap.projects.len(), 1);
    let app = &snap.projects[0];
    assert_eq!((app.project.id, app.project.goal, app.today), (id, 2, 3));
    assert_eq!(snap.studied_today().commits, 3);

    // A project that disappears stays listed, with its history, but its goal stops counting.
    fs::remove_dir_all(&moved).unwrap();
    fx.sync();
    let snap = fx.snapshot();
    let gone = &snap.projects[0];
    assert!(gone.project.missing);
    assert_eq!((gone.today, gone.total), (3, 3));
    assert_eq!(snap.studied_today().commits, 3);
    assert_eq!(snap.studied_today().projects_due, 0);
}

fn db_set_goal(fx: &Fixture, id: i64, goal: i64) {
    patrak::projects::set_goal(&fx.conn, id, goal).unwrap();
}
