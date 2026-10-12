use crate::config::{BOOK_EXTS, Config, VIDEO_EXTS};
use crate::db::{Chapter, Kind, Scanned};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;
use walkdir::{DirEntry, WalkDir};

/// Directory name that holds lecture notes; never scanned for media.
pub const NOTES_DIR: &str = "notes";

pub fn scan(cfg: &Config) -> Vec<Scanned> {
    let mut out = Vec::new();
    for root in cfg.video_roots() {
        scan_videos(&root, &mut out);
    }
    for root in cfg.book_roots() {
        scan_books(&root, &mut out);
    }
    out
}

fn scan_videos(root: &Path, out: &mut Vec<Scanned>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut entries: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| !is_hidden(p) && file_name(p) != NOTES_DIR)
        .collect();
    entries.sort_by(|a, b| natord::compare(&file_name(a), &file_name(b)));

    for entry in entries {
        if entry.is_dir() {
            let course = file_name(&entry);
            for (ord, file) in media_files(&entry, VIDEO_EXTS).into_iter().enumerate() {
                if let Some(s) = scanned(Kind::Video, &file, &entry, &course, &entry, ord) {
                    out.push(s);
                }
            }
        } else if has_ext(&entry, VIDEO_EXTS) {
            let course = stem(&entry);
            if let Some(s) = scanned(Kind::Video, &entry, &entry, &course, root, 0) {
                out.push(s);
            }
        }
    }
}

fn scan_books(root: &Path, out: &mut Vec<Scanned>) {
    for (ord, file) in media_files(root, BOOK_EXTS).into_iter().enumerate() {
        let parent = file.parent().unwrap_or(root).to_path_buf();
        let shelf = parent
            .strip_prefix(root)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(s) = scanned(Kind::Book, &file, &file, &shelf, &parent, ord) {
            out.push(s);
        }
    }
}

/// Media files below `dir`, in natural order of their path relative to `dir`.
fn media_files(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let keep = |e: &DirEntry| {
        e.depth() == 0
            || !(is_hidden(e.path()) || (e.file_type().is_dir() && e.file_name() == NOTES_DIR))
    };
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_entry(keep)
        .flatten()
        .filter(|e| e.file_type().is_file() && has_ext(e.path(), exts))
        .map(DirEntry::into_path)
        .collect();
    let rel = |p: &Path| {
        p.strip_prefix(dir)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    files.sort_by(|a, b| natord::compare(&rel(a), &rel(b)));
    files
}

fn scanned(
    kind: Kind,
    file: &Path,
    key: &Path,
    course: &str,
    course_dir: &Path,
    ord: usize,
) -> Option<Scanned> {
    let meta = std::fs::metadata(file).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    Some(Scanned {
        kind,
        path: file.to_str()?.to_owned(),
        course_key: key.to_str()?.to_owned(),
        course: course.to_owned(),
        course_dir: course_dir.to_str()?.to_owned(),
        title: stem(file),
        ord: ord as i64,
        size: meta.len() as i64,
        mtime,
    })
}

pub(crate) fn is_hidden(p: &Path) -> bool {
    file_name(p).starts_with('.')
}

/// yt-dlp leftovers such as `x.f137.mp4.part` end in `.part`, so they never match. While yt-dlp
/// is still working it also leaves `x.f137.mp4` (one stream before merging) and `x.temp.mp4`
/// (while embedding chapters), which are skipped too.
fn has_ext(p: &Path, exts: &[&str]) -> bool {
    let inner = p
        .file_stem()
        .map(Path::new)
        .and_then(Path::extension)
        .and_then(|e| e.to_str());
    let intermediate = inner.is_some_and(|e| {
        e == "temp"
            || (e.len() > 1 && e.starts_with('f') && e[1..].bytes().all(|b| b.is_ascii_digit()))
    });
    !intermediate
        && p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

pub(crate) fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Probe {
    pub duration: Option<f64>,
    pub chapters: Vec<Chapter>,
    pub total_pages: Option<i64>,
}

pub fn probe(kind: Kind, path: &Path) -> Probe {
    match kind {
        Kind::Video => probe_video(path),
        Kind::Book => Probe {
            total_pages: probe_pages(path),
            ..Probe::default()
        },
    }
}

#[derive(Deserialize)]
struct FfprobeOut {
    #[serde(default)]
    format: Option<FfFormat>,
    #[serde(default)]
    chapters: Vec<FfChapter>,
}

#[derive(Deserialize)]
struct FfFormat {
    duration: Option<String>,
}

#[derive(Deserialize)]
struct FfChapter {
    start_time: String,
    #[serde(default)]
    tags: Option<FfTags>,
}

#[derive(Deserialize)]
struct FfTags {
    title: Option<String>,
}

fn probe_video(path: &Path) -> Probe {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_chapters",
            "-of",
            "json",
        ])
        .arg(path)
        .output();
    match out {
        Ok(out) if out.status.success() => parse_ffprobe(&out.stdout),
        _ => Probe::default(),
    }
}

fn parse_ffprobe(json: &[u8]) -> Probe {
    let Ok(parsed) = serde_json::from_slice::<FfprobeOut>(json) else {
        return Probe::default();
    };
    let duration = parsed
        .format
        .and_then(|f| f.duration)
        .and_then(|d| d.parse().ok());
    let chapters = parsed
        .chapters
        .into_iter()
        .enumerate()
        .filter_map(|(i, c)| {
            Some(Chapter {
                start: c.start_time.parse().ok()?,
                title: c
                    .tags
                    .and_then(|t| t.title)
                    .unwrap_or_else(|| format!("Chapter {}", i + 1)),
            })
        })
        .collect();
    Probe {
        duration,
        chapters,
        total_pages: None,
    }
}

fn probe_pages(path: &Path) -> Option<i64> {
    if !has_ext(path, &["pdf"]) {
        return None;
    }
    let out = Command::new("pdfinfo").arg(path).output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|n| n.trim().parse().ok())
}

/// Probes several files at once; ffprobe start-up dominates the cost of a first scan.
pub fn probe_all(jobs: Vec<(i64, Kind, PathBuf)>) -> Vec<(i64, Probe)> {
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(8);
    let chunk = jobs.len().div_ceil(workers).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = jobs
            .chunks(chunk)
            .map(|part| {
                s.spawn(move || {
                    part.iter()
                        .map(|(id, kind, path)| (*id, probe(*kind, path)))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn scans_courses_in_natural_order_and_skips_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let videos = tmp.path().join("Videos");
        let course = videos.join("Distributed Systems بالعربي");
        for name in [
            "Episode 10 ｜ Paxos.mp4",
            "Episode 2 ｜ State Machine Replication.mp4",
            "Episode 1 ｜ Why distributed systems？.mp4",
            "Episode 3 ｜ Raft.f137.mp4.part",
            "Episode 4 ｜ Clocks.f137.mp4",
            "Episode 4 ｜ Clocks.temp.mp4",
            ".hidden.mp4",
            "notes/Episode 1.md",
            "cover.jpg",
        ] {
            touch(&course.join(name));
        }
        touch(&videos.join("One Long Lecture.mkv"));
        touch(&tmp.path().join("Books/Rust/The Book.pdf"));
        touch(&tmp.path().join("Books/SICP.epub"));

        let cfg = Config {
            video_dirs: vec![videos.to_string_lossy().into()],
            book_dirs: vec![tmp.path().join("Books").to_string_lossy().into()],
            ..Config::default()
        };
        let found = scan(&cfg);
        let titles: Vec<_> = found.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Episode 1 ｜ Why distributed systems？",
                "Episode 2 ｜ State Machine Replication",
                "Episode 10 ｜ Paxos",
                "One Long Lecture",
                "The Book",
                "SICP",
            ]
        );
        assert_eq!(found[0].course, "Distributed Systems بالعربي");
        assert_eq!(found[2].ord, 2);
        assert_eq!(found[3].course, "One Long Lecture");
        assert_eq!(found[3].course_dir, videos.to_string_lossy());
        assert_eq!(found[4].course, "Rust");
        assert_eq!(found[5].course, "");
    }

    #[test]
    fn parses_ffprobe_chapters() {
        let json = br#"{"chapters":[
            {"start_time":"0.000000","end_time":"60.0","tags":{"title":"Intro"}},
            {"start_time":"60.000000","end_time":"120.0"}],
            "format":{"duration":"120.500000"}}"#;
        let p = parse_ffprobe(json);
        assert_eq!(p.duration, Some(120.5));
        assert_eq!(p.chapters[0].title, "Intro");
        assert_eq!(p.chapters[1].title, "Chapter 2");
        assert_eq!(p.chapters[1].start, 60.0);
    }
}
