//! Courses downloaded from YouTube playlists with yt-dlp.
//!
//! A downloaded course folder holds two hidden files, so the link survives moving the folder:
//! `.patrak-source.json` (the playlist URL and file name template) and `.patrak-archive.txt`
//! (yt-dlp's record of downloaded videos, so updates only fetch new episodes).

use crate::config::{self, Config};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const SOURCE_FILE: &str = ".patrak-source.json";
pub const ARCHIVE_FILE: &str = ".patrak-archive.txt";
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
#[derive(Debug, Clone)]
pub struct Plan {
    pub url: String,
    /// The course folder, or the video folder itself for a single video.
    pub dir: PathBuf,
    pub title: String,
    pub videos: usize,
    /// `None` for a single video, which has nothing to update later.
    pub source: Option<Source>,
}

impl Plan {
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
        checked_at: Some(config::now()),
        new_titles: Vec::new(),
    };
    Ok(Plan {
        url: url.into(),
        dir,
        title,
        videos: info.entries.len(),
        source: Some(source),
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
    })
}

/// Saves the source file before downloading, so an interrupted download can be resumed with
/// `patrak update`.
pub fn prepare(plan: &Plan) -> Result<()> {
    std::fs::create_dir_all(&plan.dir)?;
    if let Some(source) = &plan.source {
        source.save(&plan.dir)?;
    }
    Ok(())
}

/// Called after a successful download: nothing is new any more.
pub fn finish(plan: &Plan) -> Result<()> {
    if let Some(mut source) = Source::load(&plan.dir) {
        source.checked_at = Some(config::now());
        source.new_titles.clear();
        source.save(&plan.dir)?;
    }
    Ok(())
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

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    /// 0..1 for the current video.
    pub fraction: Option<f64>,
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
        assert_eq!((p.index, p.count), (Some(3), Some(12)));
        assert_eq!(p.title, "Episode 3 | Raft");
        assert!((p.overall().unwrap() - 2.5 / 12.0).abs() < 1e-9);

        let single = parse_progress("[patrak] 100|NA|NA|NA|Talk").unwrap();
        assert_eq!((single.fraction, single.overall()), (None, None));
        assert!(parse_progress("[download] Destination: x.mp4").is_none());
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
