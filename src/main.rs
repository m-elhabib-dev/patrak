mod cli;
#[cfg(feature = "gui")]
mod gui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "patrak",
    version,
    about = "Track daily learning: lectures watched in mpv, books read in zathura"
)]
struct Args {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the window (the default when no command is given)
    Gui {
        /// Page to open: today, courses, books or stats
        #[arg(long, value_parser = ["today", "courses", "books", "stats"])]
        page: Option<String>,
    },
    /// Today's learning time against the goal, and what's next
    Today,
    /// Progress of every course and book, or the lectures of one course
    Status { query: Option<String> },
    /// The next lecture to watch
    Next { query: Option<String> },
    /// Play a course in mpv from its next lecture (or from a named lecture)
    Play { query: Option<String> },
    /// Open a book in zathura (the last one read by default)
    Read { query: Option<String> },
    /// Edit the notes for a lecture or book in $EDITOR
    Note { query: Option<String> },
    /// Mark a lecture or book as done
    Done { query: String },
    /// Mark a lecture or book as not done and rewind it
    Reset { query: String },
    /// Set a course's deadline: 2026-11-15, +3w, +10d, +2m, or "clear"
    Deadline { course: String, when: String },
    /// Download a YouTube playlist (or one video) as a course with yt-dlp
    Add {
        url: String,
        /// Course folder name (default: the playlist title)
        #[arg(long)]
        name: Option<String>,
    },
    /// Download new episodes of courses added with `patrak add`
    Update {
        /// Only courses whose name contains this
        query: Option<String>,
        /// List new episodes without downloading
        #[arg(long)]
        check: bool,
    },
    /// Review the cards due today (Q:/A: lines in your notes)
    Review,
    /// Heatmap, streaks and weekly totals
    Stats,
    /// Rescan the library and pull progress from mpv and zathura
    Sync {
        #[arg(short, long)]
        verbose: bool,
    },
    /// Send a desktop notification if today's goal isn't met yet (run by the timer)
    Remind,
    /// Write the config and library folders; install the mpv script, launcher and reminder timer
    Setup {
        /// Overwrite files that already exist
        #[arg(long)]
        force: bool,
    },
}

fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        None => gui(None),
        Some(Cmd::Gui { page }) => gui(page),
        Some(Cmd::Today) => cli::today(),
        Some(Cmd::Status { query }) => cli::status(query),
        Some(Cmd::Next { query }) => cli::next(query),
        Some(Cmd::Play { query }) => cli::play(query),
        Some(Cmd::Read { query }) => cli::read(query),
        Some(Cmd::Note { query }) => cli::note(query),
        Some(Cmd::Done { query }) => cli::mark(query, true),
        Some(Cmd::Reset { query }) => cli::mark(query, false),
        Some(Cmd::Deadline { course, when }) => cli::deadline(course, when),
        Some(Cmd::Add { url, name }) => cli::add(url, name),
        Some(Cmd::Update { query, check }) => cli::update(query, check),
        Some(Cmd::Review) => cli::review(),
        Some(Cmd::Stats) => cli::stats(),
        Some(Cmd::Sync { verbose }) => cli::sync(verbose),
        Some(Cmd::Remind) => cli::remind(),
        Some(Cmd::Setup { force }) => cli::setup(force),
    }
}

#[cfg(feature = "gui")]
fn gui(page: Option<String>) -> anyhow::Result<()> {
    gui::run(page);
    Ok(())
}

#[cfg(not(feature = "gui"))]
fn gui(_page: Option<String>) -> anyhow::Result<()> {
    cli::today()
}
