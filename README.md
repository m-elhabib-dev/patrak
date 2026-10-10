# patrak

A daily learning tracker. It watches folders of video courses and books, picks up your progress from
**mpv** and **zathura** automatically, and shows how much you learned today, your streak, and what to
watch or read next. It has a GTK4/libadwaita window and a CLI.

## Install

```sh
sudo dnf install gtk4-devel libadwaita-devel   # build dependencies for the GUI
cargo install --path .                         # installs ~/.cargo/bin/patrak
patrak setup
```

`patrak setup` does the following:
- writes `~/.config/patrak/config.toml`;
- creates the library folders;
- installs the mpv script (`~/.config/mpv/scripts/patrak.lua`) and a launcher entry;
- enables a daily reminder timer (`systemctl --user list-timers | grep patrak`).

It never overwrites existing files unless you pass `--force`. The GUI's **Set Up** banner runs the same steps.

To build without the GUI, use `cargo build --no-default-features`.

## Library layout

```
~/patrak/Videos/rust-from-scratch/01 - Intro.mp4    ← each folder is a course
~/patrak/Videos/rust-from-scratch/02 - Ownership.mp4
~/patrak/Videos/One Long Lecture.mkv                ← a loose video is a one-lecture course
~/patrak/Books/Rust/The Rust Book.pdf               ← every pdf/epub/djvu is a book
```

- Lectures are ordered naturally, so `Episode 2` comes before `Episode 10`.
- Hidden files and yt-dlp `.part` leftovers are ignored.
- A long lecture with chapters shows which chapter you're on.
- Notes go to `<course>/notes/<lecture>.md`.

You can add other folders, such as `~/Videos/Youtube`, in Preferences → Library or in `config.toml`.

## Where progress comes from

| Source | What it gives |
| --- | --- |
| mpv script (`assets/patrak.lua`) | Real watch time (paused time doesn't count), position, and "finished" when a lecture plays to its end |
| mpv `watch_later` files | Saved positions, including history from before you installed patrak |
| zathura `bookmarks.sqlite` | The current page of each book; pages read count toward the goal (`minutes_per_page`) |

The library is synced every time the GUI refreshes and every time a CLI command runs. While mpv is playing,
the window updates about once a minute. Study done before `day_starts_at` (04:00 by default) counts for the
previous day.

## CLI

```
patrak                   open the window (patrak gui --page stats opens a specific page)
patrak today             minutes vs goal, streak, next lecture
patrak status [course]   all courses and books, or one course's lectures and chapters
patrak play [query]      play a course in mpv from the next lecture; mpv moves on to the next one by itself
patrak read [book]       open a book in zathura
patrak note [query]      edit notes in $EDITOR
patrak done/reset <q>    mark a lecture or book done or not done
patrak stats             heatmap, streaks, weekly totals
patrak sync -v           rescan and show what changed
```

Queries are case-insensitive substrings of course, lecture or book names.

For testing on a scratch library, set `PATRAK_CONFIG` and `PATRAK_DATA_DIR` to use a separate config file and database.
