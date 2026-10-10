//! mpv's resume files (`save-position-on-quit`). Each file is named by the uppercase MD5 of the
//! video's absolute path and holds `start=<seconds>`. mpv deletes it when a video plays to the end.

use md5::{Digest, Md5};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, PartialEq)]
pub struct Resume {
    pub start: f64,
    /// When mpv wrote the file (unix seconds).
    pub mtime: i64,
}

pub fn file_name(video_path: &str) -> String {
    Md5::digest(video_path.as_bytes())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// `start=` from a resume file; `None` for redirect entries written for directories.
pub fn parse(content: &str) -> Option<f64> {
    content
        .lines()
        .find_map(|l| l.strip_prefix("start="))
        .and_then(|v| v.trim().parse().ok())
}

/// Looks the video up under its path as stored and under its canonical path, since
/// mpv hashes whatever absolute path it was given.
pub fn lookup(dir: &Path, video: &Path) -> Option<Resume> {
    let mut candidates = vec![video.to_path_buf()];
    if let Ok(canon) = video.canonicalize()
        && canon != video
    {
        candidates.push(canon);
    }
    candidates.iter().find_map(|p| {
        let file = dir.join(file_name(p.to_str()?));
        let content = std::fs::read_to_string(&file).ok()?;
        let mtime = std::fs::metadata(&file)
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(Resume {
            start: parse(&content)?,
            mtime,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_files_like_mpv() {
        // printf '%s' '/home/user/patrak/Videos/rust-from-scratch/01 - Intro.mp4' | md5sum
        assert_eq!(
            file_name("/home/user/patrak/Videos/rust-from-scratch/01 - Intro.mp4"),
            "A2060EC30EA360562B85EE18F10EFF83"
        );
    }

    #[test]
    fn parses_start_and_ignores_redirects() {
        assert_eq!(
            parse("start=3062.382667\nvolume=94.289833\n"),
            Some(3062.382667)
        );
        assert_eq!(parse("volume=64\nstart=12.5"), Some(12.5));
        assert_eq!(parse("# redirect entry\n"), None);
    }

    #[test]
    fn finds_resume_file_by_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("lecture.mp4");
        std::fs::write(&video, b"x").unwrap();
        let wl = tmp.path().join("watch_later");
        std::fs::create_dir(&wl).unwrap();
        std::fs::write(wl.join(file_name(video.to_str().unwrap())), "start=42.0\n").unwrap();
        assert_eq!(lookup(&wl, &video).unwrap().start, 42.0);
        assert!(lookup(&wl, &tmp.path().join("other.mp4")).is_none());
    }
}
