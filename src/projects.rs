//! Git repositories in the project folders. Each project has a daily goal of commits that counts
//! toward the day's goal and streak from the day patrak first found the project.
//!
//! Commits are remembered by author time. Rebasing, amending, cherry-picking and squashing keep a
//! commit's author time, so its new copy isn't counted again; and a commit that later disappears
//! (squashed, reset, on a deleted branch) still counts for the day it was made.

use crate::config::Config;
use crate::library::{file_name, is_hidden};
use crate::progress;
use anyhow::Result;
use chrono::NaiveDate;
use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The daily goal a newly found project starts with.
pub const DEFAULT_GOAL: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub path: PathBuf,
    /// Commits a day; 0 means no goal, so the project doesn't affect the streak.
    pub goal: i64,
    /// The first study day the goal counts on.
    pub added: NaiveDate,
}

impl Project {
    pub fn has_goal(&self) -> bool {
        self.goal > 0
    }

    /// Whether `commits` reach the goal; never true without one.
    pub fn met(&self, commits: i64) -> bool {
        self.has_goal() && commits >= self.goal
    }

    /// 0 to 1: how much of the goal `commits` reach.
    pub fn share(&self, commits: i64) -> f64 {
        if !self.has_goal() {
            return 1.0;
        }
        (commits as f64 / self.goal as f64).clamp(0.0, 1.0)
    }

    /// "2 of 3 commits", or "4 commits" without a goal.
    pub fn progress_text(&self, commits: i64) -> String {
        if self.has_goal() {
            format!("{commits} of {}", count(self.goal))
        } else {
            count(commits)
        }
    }
}

/// "commit" or "commits".
pub fn word(n: i64) -> &'static str {
    if n == 1 { "commit" } else { "commits" }
}

/// "1 commit" or "3 commits".
pub fn count(n: i64) -> String {
    format!("{n} {}", word(n))
}

/// A project as of today.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectStatus {
    pub project: Project,
    /// Commits today.
    pub today: i64,
    /// Commits on every day counted.
    pub total: i64,
    pub last_commit: Option<NaiveDate>,
    pub streak: usize,
    pub best_streak: usize,
}

impl ProjectStatus {
    pub fn done(&self) -> bool {
        self.project.met(self.today)
    }

    pub fn streak_text(&self) -> Option<String> {
        progress::streak_text(self.streak, self.best_streak)
    }
}

/// Every project still on disk, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, path, goal, added FROM projects WHERE missing = 0 ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        let path: String = r.get(2)?;
        let added: String = r.get(4)?;
        Ok(Project {
            id: r.get(0)?,
            name: r.get(1)?,
            path: path.into(),
            goal: r.get(3)?,
            added: NaiveDate::parse_from_str(&added, "%Y-%m-%d").unwrap_or(NaiveDate::MIN),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Adds a project whose goal counts from `today` on; returns its id.
pub fn add(conn: &Connection, path: &Path, name: &str, today: NaiveDate) -> Result<i64> {
    conn.execute(
        "INSERT INTO projects (path, name, goal, added) VALUES (?1, ?2, ?3, ?4)",
        params![
            path.to_string_lossy(),
            name,
            DEFAULT_GOAL,
            today.to_string()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Sets a project's daily goal; 0 turns it off.
pub fn set_goal(conn: &Connection, id: i64, goal: i64) -> Result<()> {
    conn.execute(
        "UPDATE projects SET goal = ?2 WHERE id = ?1",
        params![id, goal.max(0)],
    )?;
    Ok(())
}

/// Records a commit by its author time (unix seconds); returns whether it is new.
pub fn record(conn: &Connection, id: i64, ts: i64) -> Result<bool> {
    let added = conn.execute(
        "INSERT OR IGNORE INTO project_commits (project_id, ts) VALUES (?1, ?2)",
        params![id, ts],
    )?;
    Ok(added > 0)
}

/// (study day, project id, commits) for every day with commits, oldest first, including
/// projects that are no longer on disk.
pub fn daily(conn: &Connection, cfg: &Config) -> Result<Vec<(NaiveDate, i64, i64)>> {
    let mut stmt = conn.prepare("SELECT project_id, ts FROM project_commits")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut days: BTreeMap<(NaiveDate, i64), i64> = BTreeMap::new();
    for row in rows {
        let (id, ts) = row?;
        *days.entry((cfg.day_of(ts), id)).or_insert(0) += 1;
    }
    Ok(days
        .into_iter()
        .map(|((date, id), commits)| (date, id, commits))
        .collect())
}

/// Each project with today's commits and its streaks, from `logs` as returned by [`daily`];
/// most recently committed to first.
pub fn statuses(
    projects: &[Project],
    logs: &[(NaiveDate, i64, i64)],
    holidays: &BTreeSet<NaiveDate>,
    today: NaiveDate,
) -> Vec<ProjectStatus> {
    let mut out: Vec<ProjectStatus> = projects
        .iter()
        .map(|p| {
            let counts: BTreeMap<NaiveDate, i64> = logs
                .iter()
                .filter(|l| l.1 == p.id)
                .map(|&(date, _, commits)| (date, commits))
                .collect();
            let met = |d: NaiveDate| d >= p.added && counts.get(&d).is_some_and(|&c| p.met(c));
            let (streak, best_streak) = progress::streaks_by(
                counts.keys().copied(),
                met,
                |d| holidays.contains(&d),
                today,
            );
            ProjectStatus {
                project: p.clone(),
                today: counts.get(&today).copied().unwrap_or(0),
                total: counts.values().sum(),
                last_commit: counts.keys().next_back().copied(),
                streak,
                best_streak,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.last_commit
            .cmp(&a.last_commit)
            .then_with(|| natord::compare(&a.project.name, &b.project.name))
    });
    out
}

/// Finds the projects in the project folders and recounts the commits of each one whose
/// branches or tags moved since the last count. Returns lines for `patrak sync -v`.
pub fn sync(conn: &Connection, cfg: &Config) -> Result<Vec<String>> {
    let today = cfg.today();
    let mut changes = Vec::new();
    conn.execute("UPDATE projects SET missing = 1", [])?;
    for found in find(cfg) {
        let existing: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT id, refs FROM projects WHERE path = ?1",
                [found.path.to_string_lossy()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (id, counted_at) = match existing {
            Some((id, refs)) => {
                conn.execute(
                    "UPDATE projects SET name = ?2, missing = 0 WHERE id = ?1",
                    params![id, found.name],
                )?;
                (id, refs)
            }
            None => {
                changes.push(format!("project  {}", found.name));
                (add(conn, &found.path, &found.name, today)?, None)
            }
        };
        let Some(refs) = refs_fingerprint(&found.path) else {
            continue;
        };
        if counted_at.as_deref() == Some(refs.as_str()) {
            continue;
        }
        let Some(times) = own_commits(&found.path) else {
            continue;
        };
        let mut new = 0;
        for ts in times {
            new += usize::from(record(conn, id, ts)?);
        }
        conn.execute(
            "UPDATE projects SET refs = ?2 WHERE id = ?1",
            params![id, refs],
        )?;
        if new > 0 {
            changes.push(format!("commits  {} in {}", count(new as i64), found.name));
        }
    }
    Ok(changes)
}

/// A git repository found in a project folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    /// Canonical, so a repository reached through two links is one project.
    pub path: PathBuf,
    pub name: String,
}

/// The repositories directly inside each project folder, or the folder itself when it is one.
pub fn find(cfg: &Config) -> Vec<Found> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in cfg.project_roots() {
        let dirs = if is_repo(&root) {
            vec![root]
        } else {
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| !is_hidden(p) && is_repo(p))
                .collect();
            dirs.sort_by(|a, b| natord::compare(&file_name(a), &file_name(b)));
            dirs
        };
        for dir in dirs {
            let path = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if seen.insert(path.clone()) {
                out.push(Found {
                    name: file_name(&dir),
                    path,
                });
            }
        }
    }
    out
}

/// `.git` is a directory in a normal repository and a file in a worktree.
fn is_repo(dir: &Path) -> bool {
    dir.is_dir() && dir.join(".git").exists()
}

/// Output of `git -C dir args…`, or `None` if git is missing or fails.
fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// Changes whenever a branch, remote branch or tag moves, i.e. whenever commits may have.
fn refs_fingerprint(dir: &Path) -> Option<String> {
    let refs = git(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    Some(
        Md5::digest(&refs)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}

/// The email git puts on new commits in `dir`, with the repository's config over the global one.
fn author_email(dir: &Path) -> Option<String> {
    let ident = git(dir, &["var", "GIT_AUTHOR_IDENT"])?;
    let ident = String::from_utf8_lossy(&ident);
    let (_, rest) = ident.split_once('<')?;
    let (email, _) = rest.split_once('>')?;
    Some(email.trim().to_owned()).filter(|e| !e.is_empty())
}

/// Author times of the user's commits on every branch, remote branch and tag, merges left out.
fn own_commits(dir: &Path) -> Option<Vec<i64>> {
    let email = author_email(dir);
    let log = git(
        dir,
        &[
            "log",
            "--branches",
            "--remotes",
            "--tags",
            "--no-merges",
            "--format=%at%x09%ae",
        ],
    )?;
    Some(by_author(&String::from_utf8_lossy(&log), email.as_deref()))
}

/// The times from `<author time>\t<author email>` lines written by `email`, or from every line
/// without one.
fn by_author(log: &str, email: Option<&str>) -> Vec<i64> {
    log.lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(_, author)| email.is_none_or(|e| author.eq_ignore_ascii_case(e)))
        .filter_map(|(ts, _)| ts.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Local, TimeZone};

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn keeps_only_the_users_commits() {
        let log = "100\tme@example.com\n200\tMe@Example.com\n300\tother@example.com\nnot a line\n";
        assert_eq!(by_author(log, Some("me@example.com")), [100, 200]);
        assert_eq!(by_author(log, None), [100, 200, 300]);
    }

    #[test]
    fn counts_each_author_time_once_per_study_day() {
        let conn = crate::db::open_in_memory().unwrap();
        let cfg = Config::default();
        let at = |s: &str| {
            let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
            Local.from_local_datetime(&naive).unwrap().timestamp()
        };
        let today = d("2026-10-11");
        let id = add(&conn, Path::new("/p/app"), "app", today).unwrap();
        assert!(record(&conn, id, at("2026-10-11 10:00")).unwrap());
        // The same commit again, say after a rebase.
        assert!(!record(&conn, id, at("2026-10-11 10:00")).unwrap());
        record(&conn, id, at("2026-10-11 22:00")).unwrap();
        // Before day_starts_at: still the 11th.
        record(&conn, id, at("2026-10-12 02:00")).unwrap();
        record(&conn, id, at("2026-10-12 09:00")).unwrap();
        assert_eq!(
            daily(&conn, &cfg).unwrap(),
            [(today, id, 3), (d("2026-10-12"), id, 1)]
        );

        let projects = list(&conn).unwrap();
        assert_eq!(projects[0].goal, DEFAULT_GOAL);
        assert_eq!(projects[0].added, today);
        set_goal(&conn, id, 0).unwrap();
        let p = &list(&conn).unwrap()[0];
        assert!(!p.has_goal() && !p.met(4));
        assert_eq!(p.progress_text(4), "4 commits");
    }

    #[test]
    fn streaks_count_days_the_goal_was_met() {
        let today = d("2026-10-11");
        let project = Project {
            id: 1,
            name: "app".into(),
            path: "/p/app".into(),
            goal: 2,
            added: d("2026-10-07"),
        };
        let day = |n: i64| today - Duration::days(n);
        let logs = [
            // Before the project was found: counted, but not toward its streak.
            (day(5), 1, 2),
            (day(4), 1, 2),
            (day(3), 1, 1),
            (day(2), 1, 2),
            (day(1), 1, 3),
            (today, 1, 1),
        ];
        let s = &statuses(
            std::slice::from_ref(&project),
            &logs,
            &BTreeSet::new(),
            today,
        )[0];
        assert_eq!((s.today, s.total, s.streak, s.best_streak), (1, 11, 2, 2));
        assert_eq!(s.last_commit, Some(today));
        assert!(!s.done());
        assert_eq!(project.progress_text(1), "1 of 2 commits");
        assert_eq!(project.share(1), 0.5);
    }

    #[test]
    fn finds_repositories_and_folders_that_are_one() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = tmp.path().join("Projects");
        for dir in ["b-app/.git", "a-app/.git", "not-a-repo", ".hidden/.git"] {
            std::fs::create_dir_all(projects.join(dir)).unwrap();
        }
        let single = tmp.path().join("single");
        std::fs::create_dir_all(single.join(".git")).unwrap();
        // The same repository through a link counts once.
        std::os::unix::fs::symlink(projects.join("a-app"), tmp.path().join("link")).unwrap();
        let cfg = Config {
            project_dirs: vec![
                projects.to_string_lossy().into(),
                single.to_string_lossy().into(),
                tmp.path().join("link").to_string_lossy().into(),
            ],
            ..Config::default()
        };
        let names: Vec<String> = find(&cfg).into_iter().map(|f| f.name).collect();
        assert_eq!(names, ["a-app", "b-app", "single"]);
    }
}
