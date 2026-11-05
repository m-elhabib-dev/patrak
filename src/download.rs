//! Courses downloaded from YouTube playlists with yt-dlp.
//!
//! A downloaded course folder holds two hidden files, so the link survives moving the folder:
//! `.patrak-source.json` (the playlist URL and file name template) and `.patrak-archive.txt`
//! (yt-dlp's record of downloaded videos, so updates only fetch new episodes).
//!
//! Every download is also listed in `pending-downloads.json` in the data directory until it
//! finishes, so a stopped one, even a single video with no folder of its own, keeps its link.

use crate::config::{self, Config, Paths};
use crate::db::{self, DownloadRecord, DownloadStatus};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SOURCE_FILE: &str = ".patrak-source.json";
pub const ARCHIVE_FILE: &str = ".patrak-archive.txt";
const PENDING_FILE: &str = "pending-downloads.json";
/// How many downloads the history lists.
const HISTORY_LIMIT: i64 = 200;
/// File names for a new course: numbered, so lectures sort in playlist order.
pub const NUMBERED_TEMPLATE: &str = "%(playlist_index)03d - %(title)s.%(ext)s";
/// File names matching an existing folder downloaded without numbers (yt-download's layout),
/// so yt-dlp recognises the videos already there.
pub const TITLE_TEMPLATE: &str = "%(title)s.%(ext)s";
/// Machine-readable progress lines for the GUI.
const PROGRESS_TEMPLATE: &str = "download:[patrak] %(progress.downloaded_bytes)s|%(progress.total_bytes,progress.total_bytes_estimate)s|%(info.playlist_index)s|%(info.playlist_count)s|%(info.title)s";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub template: String,
    /// When the playlist was last checked for new episodes (unix seconds).
    #[serde(default)]
    pub checked_at: Option<i64>,
    /// Titles of episodes not downloaded yet, as of `checked_at`.
    #[serde(default)]
    pub new_titles: Vec<String>,
}

impl Source {
    pub fn load(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(SOURCE_FILE), serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", dir.join(SOURCE_FILE).display()))
    }
}

/// Where a URL will be downloaded and how.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub url: String,
    /// The course folder, or the video folder itself for a single video.
    pub dir: PathBuf,
    pub title: String,
    pub videos: usize,
    /// `None` for a single video, which has nothing to update later.
    pub source: Option<Source>,
    /// How far it had got when last saved, to show until it's resumed.
    #[serde(default)]
    pub progress: Option<Progress>,
}

impl Plan {
    /// Whether both download the same URL to the same place.
    pub fn same_download(&self, other: &Plan) -> bool {
        self.url == other.url && self.dir == other.dir
    }

    pub fn template(&self) -> &str {
        self.source.as_ref().map_or(TITLE_TEMPLATE, |s| &s.template)
    }

    pub fn describe(&self) -> String {
        match &self.source {
            Some(_) => format!(
                "{} ({} video{}) into {}",
                self.title,
                self.videos,
                if self.videos == 1 { "" } else { "s" },
                self.dir.display()
            ),
            None => format!("{} into {}", self.title, self.dir.display()),
        }
    }
}

#[derive(Deserialize)]
struct FlatInfo {
    #[serde(rename = "_type", default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    entries: Vec<serde_json::Value>,
}

/// Looks the URL up (no download) and decides where it goes: a playlist becomes a course
/// folder in the first video folder, a single video a loose one-lecture course.
pub fn plan(cfg: &Config, url: &str, name: Option<&str>) -> Result<Plan> {
    let root = cfg
        .video_roots()
        .into_iter()
        .next()
        .context("no video_dirs configured")?;
    let out = yt_dlp(cfg, &["--flat-playlist", "-J", url])?;
    let info: FlatInfo = serde_json::from_str(&out).context("reading yt-dlp's playlist info")?;
    let title = info.title.unwrap_or_else(|| url.to_owned());

    if info.kind.as_deref() != Some("playlist") {
        return Ok(Plan {
            url: url.into(),
            dir: root,
            title,
            videos: 1,
            source: None,
            progress: None,
        });
    }
    let folder = match name {
        Some(n) => sanitize(n),
        None => folder_name(cfg, url).unwrap_or_else(|| sanitize(&title)),
    };
    let dir = root.join(folder);
    let template = match Source::load(&dir) {
        Some(existing) => existing.template,
        None if has_files(&dir) => TITLE_TEMPLATE.into(),
        None => NUMBERED_TEMPLATE.into(),
    };
    let source = Source {
        url: url.into(),
        template,
        // Not known to be complete until `finish`, so Download New works after a stop.
        checked_at: None,
        new_titles: Vec::new(),
    };
    Ok(Plan {
        url: url.into(),
        dir,
        title,
        videos: info.entries.len(),
        source: Some(source),
        progress: None,
    })
}

/// The playlist title as yt-dlp would write it in a file name, so a folder made earlier by
/// yt-dlp itself is recognised.
fn folder_name(cfg: &Config, url: &str) -> Option<String> {
    let out = yt_dlp(
        cfg,
        &[
            "--flat-playlist",
            "-I",
            "1",
            "-o",
            "%(playlist_title)s",
            "--print",
            "filename",
            url,
        ],
    )
    .ok()?;
    let name = out.lines().next()?.trim();
    (!name.is_empty() && name != "NA").then(|| name.to_owned())
}

/// Like yt-dlp: no path separators in folder names.
fn sanitize(name: &str) -> String {
    name.trim().replace('/', "⧸").replace('\0', "")
}

fn has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// The yt-dlp command that downloads `plan`. With `machine_progress` it prints progress lines
/// that [`parse_progress`] understands.
pub fn download_args(cfg: &Config, plan: &Plan, machine_progress: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![cfg.yt_dlp.clone().into()];
    args.extend(cfg.yt_dlp_args.iter().map(OsString::from));
    args.extend([
        "--embed-chapters".into(),
        "-o".into(),
        plan.dir.join(plan.template()).into_os_string(),
    ]);
    if plan.source.is_some() {
        args.extend([
            "--download-archive".into(),
            plan.dir.join(ARCHIVE_FILE).into_os_string(),
        ]);
    }
    if machine_progress {
        args.extend([
            "--newline".into(),
            "--progress-template".into(),
            PROGRESS_TEMPLATE.into(),
        ]);
    }
    args.push(plan.url.clone().into());
    args
}

/// Course folders created by `patrak add`, including ones whose first download failed and
/// so have no lectures yet.
pub fn source_dirs(cfg: &Config) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = cfg
        .video_roots()
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.join(SOURCE_FILE).is_file())
        .collect();
    dirs.sort();
    dirs
}

/// A plan for fetching new episodes of an already downloaded course.
pub fn update_plan(dir: &Path) -> Option<Plan> {
    let source = Source::load(dir)?;
    let title = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(Plan {
        url: source.url.clone(),
        dir: dir.to_path_buf(),
        title,
        videos: source.new_titles.len(),
        source: Some(source),
        progress: None,
    })
}

/// Records the link before downloading, so an interrupted download can be resumed.
pub fn prepare(paths: &Paths, plan: &Plan) -> Result<()> {
    std::fs::create_dir_all(&plan.dir)?;
    if let Some(source) = &plan.source {
        source.save(&plan.dir)?;
    }
    let mut plans = pending(paths);
    // Resuming doesn't lose how far it got, in case it's stopped again before making progress.
    let progress = plans
        .iter()
        .find(|p| p.same_download(plan))
        .and_then(|p| p.progress.clone());
    plans.retain(|p| !p.same_download(plan));
    plans.push(Plan {
        progress,
        ..plan.clone()
    });
    save_pending(paths, &plans)?;
    record(paths, |conn| {
        let course = plan.source.is_some();
        db::download_started(
            conn,
            &plan.url,
            &plan.dir,
            &plan.title,
            course,
            config::now(),
        )
    });
    Ok(())
}

/// Called after a successful download: nothing is new any more.
pub fn finish(paths: &Paths, plan: &Plan) -> Result<()> {
    if let Some(mut source) = Source::load(&plan.dir) {
        source.checked_at = Some(config::now());
        source.new_titles.clear();
        source.save(&plan.dir)?;
    }
    record_end(paths, plan, DownloadStatus::Finished, None);
    forget(paths, plan)
}

/// Called when yt-dlp was stopped or failed. It stays pending, to be resumed.
pub fn stopped(paths: &Paths, plan: &Plan) {
    record_end(paths, plan, DownloadStatus::Stopped, None);
}

/// See [`stopped`].
pub fn failed(paths: &Paths, plan: &Plan, error: &str) {
    record_end(paths, plan, DownloadStatus::Failed, Some(error));
}

/// Called when a URL couldn't be looked up, so there's nothing to resume.
pub fn lookup_failed(paths: &Paths, url: &str, error: &str) {
    record(paths, |conn| {
        db::download_lookup_failed(conn, url, error, config::now())
    });
}

fn record_end(paths: &Paths, plan: &Plan, status: DownloadStatus, error: Option<&str>) {
    record(paths, |conn| {
        db::download_ended(conn, &plan.url, &plan.dir, status, error, config::now())
    });
}

/// The history is a nicety: failing to write it doesn't stop a download.
fn record(paths: &Paths, write: impl FnOnce(&rusqlite::Connection) -> Result<()>) {
    if let Err(e) = db::open(&paths.db()).and_then(|conn| write(&conn)) {
        eprintln!("download history: {e:#}");
    }
}

/// Past downloads, newest first. Unfinished ones are left out while they can be resumed, since
/// they aren't history yet.
pub fn history(paths: &Paths) -> Result<Vec<DownloadRecord>> {
    let pending = pending(paths);
    let conn = db::open(&paths.db())?;
    let mut records = db::download_history(&conn, HISTORY_LIMIT)?;
    records.retain(|r| {
        r.status == DownloadStatus::Finished
            || !pending
                .iter()
                .any(|p| p.url == r.url && r.dir.as_ref() == Some(&p.dir))
    });
    Ok(records)
}

/// Removes downloads from the history. Their files stay.
pub fn remove_from_history(paths: &Paths, ids: &[i64]) -> Result<()> {
    db::delete_downloads(&db::open(&paths.db())?, ids)
}

/// Downloads that were started and haven't finished, oldest first.
pub fn pending(paths: &Paths) -> Vec<Plan> {
    let text = std::fs::read_to_string(paths.data_dir.join(PENDING_FILE)).unwrap_or_default();
    serde_json::from_str::<Vec<Plan>>(&text)
        .unwrap_or_default()
        .into_iter()
        .filter_map(refreshed)
        .collect()
}

/// Saves how far a download has got. Does nothing once it has finished or been forgotten.
pub fn save_progress(paths: &Paths, plan: &Plan, progress: &Progress) -> Result<()> {
    let mut plans = pending(paths);
    let Some(entry) = plans.iter_mut().find(|p| p.same_download(plan)) else {
        return Ok(());
    };
    entry.progress = Some(progress.clone());
    save_pending(paths, &plans)
}

/// Stops offering to resume a download. Its files stay.
pub fn forget(paths: &Paths, plan: &Plan) -> Result<()> {
    let mut plans = pending(paths);
    plans.retain(|p| !p.same_download(plan));
    save_pending(paths, &plans)
}

/// `plan` with its course's current source. `None` if the course folder is gone; if it was
/// moved, its source file went with it and Download New still works there.
pub fn refreshed(mut plan: Plan) -> Option<Plan> {
    if plan.source.is_some() {
        plan.source = Some(Source::load(&plan.dir)?);
    }
    Some(plan)
}

fn save_pending(paths: &Paths, plans: &[Plan]) -> Result<()> {
    std::fs::create_dir_all(&paths.data_dir)?;
    let file = paths.data_dir.join(PENDING_FILE);
    std::fs::write(&file, serde_json::to_string_pretty(plans)?)
        .with_context(|| format!("writing {}", file.display()))
}

/// Asks the playlist which episodes aren't downloaded yet and records the answer.
pub fn check(cfg: &Config, dir: &Path) -> Result<Vec<String>> {
    let mut source = Source::load(dir).context("not a downloaded course")?;
    let out = yt_dlp(
        cfg,
        &[
            "--flat-playlist",
            "--print",
            "%(id)s\t%(title)s",
            &source.url,
        ],
    )?;
    let archive = std::fs::read_to_string(dir.join(ARCHIVE_FILE)).unwrap_or_default();
    source.new_titles = new_entries(&out, &archive);
    source.checked_at = Some(config::now());
    source.save(dir)?;
    Ok(source.new_titles)
}

/// Titles of `id\ttitle` lines whose id isn't in the archive (`<extractor> <id>` lines).
fn new_entries(listing: &str, archive: &str) -> Vec<String> {
    let have: HashSet<&str> = archive
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    listing
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(id, _)| !have.contains(id))
        .map(|(_, title)| title.to_owned())
        .collect()
}

fn yt_dlp(cfg: &Config, args: &[&str]) -> Result<String> {
    let out = Command::new(&cfg.yt_dlp)
        .args(&cfg.yt_dlp_args)
        .args(args)
        .output()
        .with_context(|| format!("running {} (is yt-dlp installed?)", cfg.yt_dlp))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last = err
            .lines()
            .rfind(|l| l.contains("ERROR"))
            .or(err.lines().next_back())
            .unwrap_or("");
        bail!("yt-dlp failed: {}", last.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// 0..1 for the current video.
    pub fraction: Option<f64>,
    /// Bytes of the current video fetched so far, and its (estimated) size.
    pub downloaded: Option<f64>,
    pub total: Option<f64>,
    pub index: Option<usize>,
    pub count: Option<usize>,
    pub title: String,
}

impl Progress {
    /// 0..1 across the whole playlist.
    pub fn overall(&self) -> Option<f64> {
        let f = self.fraction.unwrap_or(0.0);
        match (self.index, self.count) {
            (Some(i), Some(n)) if n > 0 => Some(((i.saturating_sub(1)) as f64 + f) / n as f64),
            _ => self.fraction,
        }
    }
}

pub fn parse_progress(line: &str) -> Option<Progress> {
    let rest = line.trim().strip_prefix("[patrak] ")?;
    let mut parts = rest.splitn(5, '|');
    let num = |s: Option<&str>| s.and_then(|v| v.trim().parse::<f64>().ok());
    let done = num(parts.next());
    let total = num(parts.next());
    let index = num(parts.next()).map(|v| v as usize);
    let count = num(parts.next()).map(|v| v as usize);
    let title = parts.next().unwrap_or_default().to_owned();
    let fraction = match (done, total) {
        (Some(d), Some(t)) if t > 0.0 => Some((d / t).clamp(0.0, 1.0)),
        _ => None,
    };
    Some(Progress {
        fraction,
        downloaded: done,
        total,
        index,
        count,
        title,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_new_episodes() {
        let listing = "IujMVjKvWP4\tUnderstand RAFT\nZyqAbQkpeUo\tMastering Raft\n64Zp3tzNbpE\tLecture 6: Raft (1)\n";
        let archive = "youtube IujMVjKvWP4\nyoutube 64Zp3tzNbpE\n";
        assert_eq!(new_entries(listing, archive), ["Mastering Raft"]);
        assert_eq!(new_entries(listing, "").len(), 3);
    }

    #[test]
    fn parses_progress_lines() {
        let p = parse_progress("[patrak] 512|1024|3|12|Episode 3 | Raft").unwrap();
        assert_eq!(p.fraction, Some(0.5));
        assert_eq!((p.downloaded, p.total), (Some(512.0), Some(1024.0)));
        assert_eq!((p.index, p.count), (Some(3), Some(12)));
        assert_eq!(p.title, "Episode 3 | Raft");
        assert!((p.overall().unwrap() - 2.5 / 12.0).abs() < 1e-9);

        let single = parse_progress("[patrak] 100|NA|NA|NA|Talk").unwrap();
        assert_eq!((single.fraction, single.overall()), (None, None));
        assert!(parse_progress("[download] Destination: x.mp4").is_none());
    }

    #[test]
    fn remembers_downloads_until_they_finish() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            events_log: tmp.path().join("events.log"),
        };
        let video = Plan {
            url: "https://y/video".into(),
            dir: tmp.path().join("videos"),
            title: "Talk".into(),
            videos: 1,
            source: None,
            progress: None,
        };
        let course = Plan {
            url: "https://y/list".into(),
            dir: tmp.path().join("videos/Rust"),
            title: "Rust".into(),
            videos: 3,
            source: Some(Source {
                url: "https://y/list".into(),
                template: NUMBERED_TEMPLATE.into(),
                checked_at: None,
                new_titles: vec![],
            }),
            progress: None,
        };
        let urls = || -> Vec<String> { pending(&paths).into_iter().map(|p| p.url).collect() };
        assert!(urls().is_empty());

        prepare(&paths, &video).unwrap();
        prepare(&paths, &course).unwrap();
        // Starting one again doesn't list it twice.
        prepare(&paths, &video).unwrap();
        assert_eq!(urls(), ["https://y/list", "https://y/video"]);

        // A course keeps its link in its folder, and a check there isn't overwritten.
        let mut checked = Source::load(&course.dir).unwrap();
        checked.new_titles = vec!["Episode 2".into()];
        checked.save(&course.dir).unwrap();
        let listed = pending(&paths).remove(0);
        assert_eq!(listed.source.unwrap().new_titles, ["Episode 2"]);

        finish(&paths, &video).unwrap();
        assert_eq!(urls(), ["https://y/list"]);

        // A course folder that's gone can't be resumed; a single video needs no folder.
        prepare(&paths, &video).unwrap();
        std::fs::remove_dir_all(&course.dir).unwrap();
        assert_eq!(urls(), ["https://y/video"]);

        // History: the finished video stays listed while it's downloaded again; the course that
        // can't be resumed any more is listed as it ended.
        let titles = || -> Vec<(String, DownloadStatus)> {
            let records = history(&paths).unwrap();
            records.into_iter().map(|r| (r.title, r.status)).collect()
        };
        assert_eq!(titles().len(), 2);
        assert!(titles().contains(&("Talk".to_owned(), DownloadStatus::Finished)));
        assert!(titles().contains(&("Rust".to_owned(), DownloadStatus::Running)));
        // Stopped and still resumable: not history yet, and forgetting it makes it so.
        stopped(&paths, &video);
        assert_eq!(titles().len(), 2);
        forget(&paths, &video).unwrap();
        assert_eq!(titles().len(), 3);
        assert!(titles().contains(&("Talk".to_owned(), DownloadStatus::Stopped)));

        let ids: Vec<i64> = history(&paths).unwrap().iter().map(|r| r.id).collect();
        remove_from_history(&paths, &ids).unwrap();
        assert!(titles().is_empty());
    }

    #[test]
    fn saves_progress_until_the_download_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: tmp.path().join("config.toml"),
            data_dir: tmp.path().join("data"),
            events_log: tmp.path().join("events.log"),
        };
        let video = Plan {
            url: "https://y/video".into(),
            dir: tmp.path().join("videos"),
            title: "Talk".into(),
            videos: 1,
            source: None,
            progress: None,
        };
        let saved = || pending(&paths).pop().and_then(|p| p.progress);
        let progress = parse_progress("[patrak] 512|1024|NA|NA|Talk").unwrap();

        prepare(&paths, &video).unwrap();
        assert_eq!(saved(), None);
        save_progress(&paths, &video, &progress).unwrap();
        assert_eq!(saved().as_ref(), Some(&progress));

        // Resuming keeps it until there's newer progress.
        prepare(&paths, &video).unwrap();
        assert_eq!(saved().as_ref(), Some(&progress));

        // A late save doesn't bring a finished download back.
        finish(&paths, &video).unwrap();
        save_progress(&paths, &video, &progress).unwrap();
        assert!(pending(&paths).is_empty());

        // Lists written before progress was saved still load.
        let old = r#"[{"url":"u","dir":"/v","title":"t","videos":1,"source":null}]"#;
        std::fs::write(paths.data_dir.join(PENDING_FILE), old).unwrap();
        assert_eq!(pending(&paths)[0].progress, None);
    }

    #[test]
    fn builds_download_command() {
        let cfg = Config {
            yt_dlp_args: vec!["-S".into(), "res:720".into()],
            ..Config::default()
        };
        let source = Source {
            url: "https://y/list".into(),
            template: NUMBERED_TEMPLATE.into(),
            checked_at: None,
            new_titles: vec![],
        };
        let plan = Plan {
            url: "https://y/list".into(),
            dir: "/v/Rust".into(),
            title: "Rust".into(),
            videos: 3,
            source: Some(source),
            progress: None,
        };
        let args: Vec<String> = download_args(&cfg, &plan, false)
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "yt-dlp",
                "-S",
                "res:720",
                "--embed-chapters",
                "-o",
                "/v/Rust/%(playlist_index)03d - %(title)s.%(ext)s",
                "--download-archive",
                "/v/Rust/.patrak-archive.txt",
                "https://y/list"
            ]
        );
        assert_eq!(sanitize(" AC/DC "), "AC⧸DC");
    }
}
