# patrak

A daily learning tracker. It watches folders of video courses, books and git projects, picks up your
progress from **mpv**, **zathura** and your commits automatically, and shows how much you learned today,
your streak, and what to watch or read next. It has a GTK4/libadwaita window and a CLI.

## Install

```sh
sudo dnf install gtk4-devel libadwaita-devel   # build dependencies for the GUI
cargo install --path .                         # installs ~/.cargo/bin/patrak
patrak setup
```

`patrak setup` does the following:
- writes `~/.config/patrak/config.toml`;
- creates the library folders (Videos, Books and Projects);
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
~/patrak/Projects/patrak/.git                       ← each git repository is a project
```

- Lectures are ordered naturally, so `Episode 2` comes before `Episode 10`.
- Hidden files and yt-dlp `.part` leftovers are ignored.
- A long lecture with chapters shows which chapter you're on.
- Notes go to `<course>/notes/<lecture>.md`.

You can add other folders, such as `~/Videos/Youtube` or `~/Programming`, in Preferences → Library or in
`config.toml` (`video_dirs`, `book_dirs`, `project_dirs`).

## Where progress comes from

| Source | What it gives |
| --- | --- |
| mpv script (`assets/patrak.lua`) | Real watch time (paused time doesn't count), position, and "finished" when a lecture plays to its end |
| mpv `watch_later` files | Saved positions, including history from before you installed patrak |
| zathura `bookmarks.sqlite` | The current page of each book; pages read count toward the pages goal |
| git, in each project | Your commits, counted toward each project's goal |

The library is synced every time the GUI refreshes and every time a CLI command runs. While mpv is playing,
the window updates about once a minute. Study done before `day_starts_at` (04:00 by default) counts for the
previous day.

## Daily goals

There are two daily goals: minutes of video (`daily_goal_minutes`, 60 by default) and book pages
(`daily_goal_pages`, 20 by default).

- Change them with the pencil button next to **Goals** on the Today page, or in Preferences.
- Card review time counts toward the minutes goal.
- Each [habit](#habits), and each [project](#projects) with a goal, is a goal too. The Today page
  lists every goal in one place, with how many are done.
- A day counts toward your streak, and shows green in the heatmap, only when every goal is met.
- Set a goal to 0 to turn it off, for example to track only videos.
- On a [holiday](#holidays) every goal is off.

## Habits

Track anything else you do every day on the **Habits** page, for example running 30 minutes or
eating three times. Use **New Habit** to give one a name, a unit (times or minutes) and a daily goal.

- Set today's amount with − and +, or type it in. On the Today page, **+1** adds one time and
  **Mark Done** completes a minutes habit.
- Each habit shows its own streak.
- Habits are part of the daily goal. A day keeps your streak only when every habit is done too, and
  the evening reminder lists the ones still missing.
- A habit counts from the day you add it, so adding one doesn't break past streaks.
- Deleting a habit deletes its history.

From the terminal:

```sh
patrak habits            # today's habits
patrak habit eat         # +1 for a count; a minutes habit is marked as done
patrak habit run 45      # set today's amount; +15 or -1 change it instead
```

## Projects

Each git repository in `~/patrak/Projects` is a project. A link to a repository works too, and so
does a project folder that is a repository itself. The **Projects** page lists them with today's
commits.

- A project's progress is the number of commits you make in it each day. Each project has its own
  daily goal: 1 commit to start with, which you can change with the pencil button or with
  `patrak project <name> <n>`.
- A goal of 0 keeps the project tracked without making it part of the daily goal.
- Project goals are part of the daily goal, like habits. A day keeps your streak only when every
  project with a goal reaches it.
- A project counts from the day patrak finds it, so adding one doesn't break past streaks. Its
  older commits still show in the heatmap and stats.
- Only your own commits count. That means commits whose author email is the repository's
  `user.email`, on any branch, tag or remote branch. Merge commits don't count.
- A commit is counted on the day it was written. It still counts after you squash it, reset past
  it or delete its branch. Rebased, amended and cherry-picked copies keep the original author time,
  so they aren't counted twice.

```sh
patrak projects          # every project: today's commits, streak, last commit
patrak project patrak 3  # set a project's daily goal; 0 turns it off
```

## Holidays

There's no fixed schedule of days off. To make today a holiday, press the sun button in the header
bar (or Ctrl+H), or run `patrak holiday`. Press it again to make today a study day again.

- Every goal is off for the day: minutes, pages, habits and project commits. Anything you still
  do is recorded as usual.
- A holiday neither breaks your streak nor adds to it. With Monday met, Tuesday a holiday and
  Wednesday met, the streak is 2 days. The same goes for each habit's and project's streak.
- Holidays show purple in the heatmap, and the evening reminder stays quiet.
- Only today can be made a holiday.

```sh
patrak holiday           # toggle today
patrak holiday on        # or off
```

## Deadlines

Give a course a deadline in its **Plan** row (Courses page) or with `patrak deadline <course> <when>`.
`<when>` is a date like `2026-11-15`, a relative time like `+3w`, `+10d` or `+2m`, or `clear`.

- patrak works out how much you need to watch each day to finish on time.
- It compares that with your recent pace (averaged over up to 14 days) and marks the course
  *on track*, *behind* or *overdue*.
- **Deadlines** on the Today page lists each course's share for today.
- `patrak play` with no course name, and **Up Next** on the Today page, pick the course furthest
  behind on today's share.

## Courses from YouTube

```sh
patrak add "https://www.youtube.com/playlist?list=…"    # or the + button in the window
patrak update                                           # fetch new episodes of every added course
patrak update --check                                   # only list what's new
```

- A playlist becomes a course folder in your first video folder, with files named
  `001 - Title.mp4`, so lectures play in playlist order.
- If the folder already exists (made by plain yt-dlp, say), new files are named
  `Title.mp4` so the videos already there are recognised.
- Each folder gets two hidden files: `.patrak-source.json` and `.patrak-archive.txt`, the record
  of downloaded videos. Moving the folder keeps the link.
- The window checks each playlist for new episodes about once a day.

yt-dlp options come from `yt_dlp` and `yt_dlp_args` in `config.toml`. If YouTube refuses downloads
(HTTP 403), update yt-dlp or add browser cookies:

```toml
yt_dlp = "/usr/bin/yt-dlp"
yt_dlp_args = ["-S", "res:1080", "--cookies-from-browser", "firefox"]
```

## Notes

Every lecture and book has a Markdown note at `<course folder>/notes/<file name>.md`, created when
you first open it. Open a note with the pencil button, or with `patrak note [query]` to use `$EDITOR`.

- The notes window saves as you type.
- The clock button inserts the current position: `[12:30]` for a video, `[p. 42]` for a book.
- In the window, stamps show as links. Click one to play the lecture from that moment (`mpv --start`)
  or open the book at that page (`zathura --page`).
- Stamps you type yourself work too, in `mm:ss`, `h:mm:ss` or `p. N` form.

## Review cards

Write questions in a lecture's notes (`patrak note`, or the notes button):

```markdown
Q: What does Raft guarantee about committed entries?
A: Every future leader has them.
```

- The answer is everything after the question up to a blank line, the next `Q:` or a heading.
  The `A:` prefix is optional.
- Cards are scheduled like Anki (Again / Hard / Good / Easy).
- Review them in the window's **Review** dialog (Space shows the answer, keys 1–4 grade it) or
  with `patrak review`.
- Review time counts toward the minutes goal.

## CLI

```
patrak                   open the window (patrak gui --page stats opens a specific page)
patrak today             minutes and pages vs goals, streak, next lecture
patrak status [course]   all courses and books, or one course's lectures and chapters
patrak play [query]      play a course in mpv from the next lecture; mpv moves on to the next one by itself
patrak read [book]       open a book in zathura
patrak note [query]      edit notes in $EDITOR
patrak done/reset <q>    mark a lecture or book done or not done
patrak deadline <c> <w>  set or clear a course deadline
patrak add <url>         download a playlist as a course (--name to choose the folder)
patrak update [course]   download new episodes (--check to only list them)
patrak review            review the cards due today
patrak habits            today's habits against their goals
patrak habit <h> [n]     log a habit: n sets today's amount, +n or -n changes it
patrak projects          git projects: today's commits against each goal
patrak project <p> [n]   show a project, or set its daily goal in commits
patrak holiday [on|off]  make today a holiday (goals off, streak kept), or toggle it
patrak stats             heatmap, streaks, weekly totals
patrak sync -v           rescan and show what changed
```

Queries are case-insensitive substrings of course, lecture, book, habit or project names.

For testing on a scratch library, set `PATRAK_CONFIG` and `PATRAK_DATA_DIR` to use a separate config file and database.
