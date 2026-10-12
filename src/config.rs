use anyhow::{Context, Result};
use chrono::{Duration, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const VIDEO_EXTS: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v"];
pub const BOOK_EXTS: &[&str] = &["pdf", "epub", "djvu"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Each subdirectory is a course; each loose video file is a one-lecture course.
    pub video_dirs: Vec<String>,
    /// Every pdf/epub/djvu below these directories is a book.
    pub book_dirs: Vec<String>,
    /// Each git repository inside is a project; a directory that is itself a repository is one too.
    pub project_dirs: Vec<String>,
    /// Minutes of lectures watched (and cards reviewed) a day; 0 turns this goal off.
    pub daily_goal_minutes: u32,
    /// Book pages read a day; 0 turns this goal off.
    pub daily_goal_pages: u32,
    /// A lecture counts as done once position/duration reaches this ratio.
    pub complete_threshold: f64,
    /// Study before this time counts for the previous day.
    pub day_starts_at: String,
    pub reminder_time: String,
    pub terminal: String,
    pub mpv_watch_later_dir: String,
    pub zathura_db: String,
    /// yt-dlp binary used by `patrak add` and `patrak update`.
    pub yt_dlp: String,
    /// Extra yt-dlp options, e.g. `["-S", "res:1080", "--cookies-from-browser", "firefox"]`.
    pub yt_dlp_args: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            video_dirs: vec!["~/patrak/Videos".into()],
            book_dirs: vec!["~/patrak/Books".into()],
            project_dirs: vec!["~/patrak/Projects".into()],
            daily_goal_minutes: 60,
            daily_goal_pages: 20,
            complete_threshold: 0.95,
            day_starts_at: "04:00".into(),
            reminder_time: "21:00".into(),
            terminal: "kitty".into(),
            mpv_watch_later_dir: "~/.local/state/mpv/watch_later".into(),
            zathura_db: "~/.local/share/zathura/bookmarks.sqlite".into(),
            yt_dlp: "yt-dlp".into(),
            yt_dlp_args: vec!["-S".into(), "res:1080".into()],
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn video_roots(&self) -> Vec<PathBuf> {
        self.video_dirs.iter().map(|d| expand(d)).collect()
    }

    pub fn book_roots(&self) -> Vec<PathBuf> {
        self.book_dirs.iter().map(|d| expand(d)).collect()
    }

    pub fn project_roots(&self) -> Vec<PathBuf> {
        self.project_dirs.iter().map(|d| expand(d)).collect()
    }

    pub fn day_start(&self) -> NaiveTime {
        parse_hhmm(&self.day_starts_at).unwrap_or(NaiveTime::MIN)
    }

    /// The study day a unix timestamp belongs to, honouring `day_starts_at`.
    pub fn day_of(&self, ts: i64) -> NaiveDate {
        let offset = self.day_start() - NaiveTime::MIN;
        let local = Local
            .timestamp_opt(ts, 0)
            .earliest()
            .unwrap_or_else(Local::now);
        (local - offset).date_naive()
    }

    pub fn today(&self) -> NaiveDate {
        self.day_of(Local::now().timestamp())
    }
}

pub fn parse_hhmm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Expands a leading `~` to the home directory.
pub fn expand(p: &str) -> PathBuf {
    if p == "~" {
        return dirs::home_dir().unwrap_or_default();
    }
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

/// Where patrak keeps its own files. `PATRAK_CONFIG` and `PATRAK_DATA_DIR`
/// override the defaults, which is handy for trying things out on a scratch library.
#[derive(Debug, Clone)]
pub struct Paths {
    pub config: PathBuf,
    pub data_dir: PathBuf,
    /// Written by the mpv script, which always uses the default data dir.
    pub events_log: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let config = std::env::var_os("PATRAK_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| config_home().join("patrak/config.toml"));
        let data_dir = std::env::var_os("PATRAK_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_data_dir);
        let events_log = default_data_dir().join("mpv-events.jsonl");
        Self {
            config,
            data_dir,
            events_log,
        }
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("patrak.db")
    }
}

pub fn config_home() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| expand("~/.config"))
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| expand("~/.local/share"))
        .join("patrak")
}

/// Seconds since the epoch.
pub fn now() -> i64 {
    Local::now().timestamp()
}

pub fn days_back(day: NaiveDate, n: i64) -> NaiveDate {
    day - Duration::days(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn ts(s: &str) -> i64 {
        let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
        Local.from_local_datetime(&naive).unwrap().timestamp()
    }

    #[test]
    fn late_night_counts_for_previous_day() {
        let cfg = Config::default();
        let d = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(cfg.day_of(ts("2026-10-11 02:30")), d("2026-10-10"));
        assert_eq!(cfg.day_of(ts("2026-10-11 04:00")), d("2026-10-11"));
        assert_eq!(cfg.day_of(ts("2026-10-11 23:59")), d("2026-10-11"));

        let midnight = Config {
            day_starts_at: "00:00".into(),
            ..Config::default()
        };
        assert_eq!(midnight.day_of(ts("2026-10-11 00:30")), d("2026-10-11"));
    }

    #[test]
    fn partial_config_uses_defaults() {
        let cfg: Config = toml::from_str("daily_goal_minutes = 90").unwrap();
        assert_eq!(cfg.daily_goal_minutes, 90);
        assert_eq!(cfg.daily_goal_pages, 20);
        assert_eq!(cfg.day_starts_at, "04:00");
    }

    #[test]
    fn old_config_with_minutes_per_page_still_loads() {
        let cfg: Config =
            toml::from_str("daily_goal_minutes = 45\nminutes_per_page = 2.0").unwrap();
        assert_eq!(cfg.daily_goal_minutes, 45);
    }
}
