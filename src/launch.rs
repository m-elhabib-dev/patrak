//! Commands for opening lectures and books, shared by the CLI and the GUI.

use crate::config::Config;
use crate::db::Item;
use crate::library::NOTES_DIR;
use crate::progress::{Course, fmt_clock};
use std::ffi::OsString;
use std::path::PathBuf;

/// `mpv` with the whole course as a playlist, starting at lecture `start`, so mpv moves on to the
/// next lecture by itself. mpv resumes the first file from its saved position.
pub fn play_args(course: &Course, start: usize) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["mpv".into(), format!("--playlist-start={start}").into()];
    args.extend(
        course
            .lectures
            .iter()
            .map(|l| l.path.clone().into_os_string()),
    );
    args
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
    format!("# {}\n\n_{course}_\n\n", item.title)
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
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, note_template(item))?;
    }
    Ok(path)
}
