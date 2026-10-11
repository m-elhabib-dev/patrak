use patrak::config::{Config, Paths};
use patrak::db;
use patrak::progress;
use patrak::sources::watch_later;
use patrak::sync::{set_done, sync};
use rusqlite::Connection;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
fn deleted_files_drop_out_of_the_library() {
    let mut fx = Fixture::new(true);
    fx.sync();
    fs::remove_file(fx.video("03 Traits.mp4")).unwrap();
    fx.sync();
    assert_eq!(fx.snapshot().courses[0].lectures.len(), 2);
}
