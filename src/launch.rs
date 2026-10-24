//! Commands for opening lectures and books, shared by the CLI and the GUI.

use crate::config::Config;
use crate::db::Item;
use crate::library::NOTES_DIR;
use crate::progress::{Course, fmt_clock};
use std::ffi::OsString;
use std::path::PathBuf;

/// `mpv` with the whole course as a playlist, starting at lecture `start`, so mpv moves on to the
/// next lecture by itself. mpv resumes the first file from its saved position. Lectures whose
/// file is gone are left out.
pub fn play_args(course: &Course, start: usize) -> Vec<OsString> {
    let start = course.lectures[..start.min(course.lectures.len())]
        .iter()
        .filter(|l| !l.missing)
        .count();
    let mut args: Vec<OsString> = vec!["mpv".into(), format!("--playlist-start={start}").into()];
    args.extend(
        course
            .lectures
            .iter()
            .filter(|l| !l.missing)
            .map(|l| l.path.clone().into_os_string()),
    );
    args
}

/// `zathura` opened at `page`.
pub fn read_at_args(book: &Item, page: i64) -> Vec<OsString> {
    vec![
        "zathura".into(),
        format!("--page={page}").into(),
        book.path.clone().into_os_string(),
    ]
}

/// `mpv` playing one file from `secs`.
pub fn play_at_args(item: &Item, secs: f64) -> Vec<OsString> {
    vec![
        "mpv".into(),
        format!("--start={secs}").into(),
        item.path.clone().into_os_string(),
    ]
}

pub fn read_args(book: &Item) -> Vec<OsString> {
    vec!["zathura".into(), book.path.clone().into_os_string()]
}

pub fn note_path(item: &Item) -> PathBuf {
    let stem = item
        .path
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    let mut name = stem;
    name.push(".md");
    item.course_dir.join(NOTES_DIR).join(name)
}

/// Header written into a new note.
pub fn note_template(item: &Item) -> String {
    let course = if item.course.is_empty() {
        "Books"
    } else {
        &item.course
    };
    format!(
        "# {}\n\n_{course}_\n\n<!-- Review cards: a line starting with Q: is a question, the lines \
         after it (optionally starting with A:) up to a blank line are the answer. -->\n\n",
        item.title
    )
}

/// `[mm:ss]` for the item's saved position, or `[p. N]` for a book.
pub fn position_stamp(item: &Item) -> String {
    match item.kind {
        crate::db::Kind::Video => format!("[{}] ", fmt_clock(item.position)),
        crate::db::Kind::Book => format!("[p. {}] ", item.position.max(1.0) as i64),
    }
}

/// `$EDITOR` (vim if unset) running in the configured terminal.
pub fn editor_in_terminal_args(cfg: &Config, file: &std::path::Path) -> Vec<OsString> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    vec![
        cfg.terminal.clone().into(),
        "-e".into(),
        editor.into(),
        file.as_os_str().to_owned(),
    ]
}

pub fn ensure_note(item: &Item) -> std::io::Result<PathBuf> {
    let path = note_path(item);
    if !path.exists() {
        // Don't bring back the folder of a course that was deleted or moved away.
        if !item.course_dir.is_dir() {
            return Err(std::io::ErrorKind::NotFound.into());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, note_template(item))?;
    }
    Ok(path)
}

/// A position written in a note by [`position_stamp`]: `[12:30]`, `[1:02:03]` or `[p. 42]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stamp {
    Time(f64),
    Page(i64),
}

impl Stamp {
    /// Whether the stamp points into this kind of item: times into videos, pages into books.
    pub fn fits(self, kind: crate::db::Kind) -> bool {
        matches!(
            (self, kind),
            (Stamp::Time(_), crate::db::Kind::Video) | (Stamp::Page(_), crate::db::Kind::Book)
        )
    }
}

/// Every stamp in `text` as `(start, end, stamp)`, in character offsets (what GTK text
/// iterators count), with `end` exclusive.
pub fn find_stamps(text: &str) -> Vec<(usize, usize, Stamp)> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(len) = chars[i + 1..].iter().take(12).position(|&c| c == ']')
            && let Some(stamp) = parse_stamp(&chars[i + 1..i + 1 + len].iter().collect::<String>())
        {
            found.push((i, i + len + 2, stamp));
            i += len + 2;
            continue;
        }
        i += 1;
    }
    found
}

fn parse_stamp(inner: &str) -> Option<Stamp> {
    let inner = inner.trim();
    if let Some(page) = inner
        .strip_prefix("p.")
        .or_else(|| inner.strip_prefix("p "))
    {
        return page.trim().parse().ok().filter(|&p| p > 0).map(Stamp::Page);
    }
    let parts: Vec<&str> = inner.split(':').collect();
    let digits = |p: &&str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    // Minutes and seconds after the first field are always two digits, as fmt_clock writes them.
    if !(2..=3).contains(&parts.len())
        || !parts.iter().all(digits)
        || parts[1..].iter().any(|p| p.len() != 2)
    {
        return None;
    }
    let nums: Vec<u64> = parts
        .iter()
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if nums[1..].iter().any(|&n| n >= 60) {
        return None;
    }
    Some(Stamp::Time(
        nums.iter().fold(0, |acc, n| acc * 60 + n) as f64
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_stamps_in_notes() {
        let note = "## Raft\n[12:30] elections ⏱ [1:02:03] logs [p. 42] [p.7]\n- [ ] todo [x] [1:2] [99:99] [link](url)";
        let found = find_stamps(note);
        assert_eq!(
            found.iter().map(|f| f.2).collect::<Vec<_>>(),
            [
                Stamp::Time(750.0),
                Stamp::Time(3723.0),
                Stamp::Page(42),
                Stamp::Page(7)
            ]
        );
        // Offsets count characters, not bytes, so text after "⏱" still lines up.
        let chars: Vec<char> = note.chars().collect();
        let (start, end, _) = found[1];
        assert_eq!(chars[start..end].iter().collect::<String>(), "[1:02:03]");
        assert!(Stamp::Time(1.0).fits(crate::db::Kind::Video));
        assert!(!Stamp::Page(1).fits(crate::db::Kind::Video));
    }
}
