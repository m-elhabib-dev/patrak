//! Review cards written in lecture notes, scheduled with spaced repetition:
//!
//! ```text
//! Q: What does Raft guarantee about committed entries?
//! A: Every future leader has them.
//! ```
//!
//! The answer runs until a blank line, the next `Q:` or a heading. The `A:` prefix is optional.

use anyhow::Result;
use chrono::{Duration, NaiveDate};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};

const MIN_EASE: f64 = 1.3;
/// A card left on screen longer than this is assumed to be forgotten, not studied.
const MAX_SECONDS_PER_CARD: f64 = 120.0;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCard {
    pub question: String,
    pub answer: String,
}

pub fn parse(text: &str) -> Vec<ParsedCard> {
    let mut cards = Vec::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    let mut in_comment = false;
    let flush = |current: &mut Option<(String, Vec<&str>)>, cards: &mut Vec<ParsedCard>| {
        if let Some((question, answer)) = current.take() {
            let answer = answer.join("\n").trim().to_owned();
            if !answer.is_empty() {
                cards.push(ParsedCard { question, answer });
            }
        }
    };

    for line in text.lines() {
        let t = line.trim();
        if in_comment {
            in_comment = !t.contains("-->");
            continue;
        }
        if t.starts_with("<!--") {
            in_comment = !t.contains("-->");
            continue;
        }
        if let Some(q) = question_of(t) {
            flush(&mut current, &mut cards);
            current = Some((q.to_owned(), Vec::new()));
            continue;
        }
        let Some((_, answer)) = current.as_mut() else {
            continue;
        };
        let heading = t.starts_with("# ") || t.starts_with("##");
        if heading || (t.is_empty() && !answer.is_empty()) {
            flush(&mut current, &mut cards);
            continue;
        }
        if t.is_empty() {
            continue;
        }
        let bare = t.trim_start_matches(['-', '*', ' ']);
        answer.push(match bare.strip_prefix("A:") {
            Some(a) => a.trim(),
            None => line.trim_end(),
        });
    }
    flush(&mut current, &mut cards);
    cards
}

/// `Q: …`, also as `## Q: …`, `- Q: …` or `> Q: …`.
fn question_of(line: &str) -> Option<&str> {
    line.trim_start_matches(['#', '-', '*', '>', ' '])
        .strip_prefix("Q:")
        .map(str::trim)
        .filter(|q| !q.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Again,
    Hard,
    Good,
    Easy,
}

impl Grade {
    pub const ALL: [Grade; 4] = [Grade::Again, Grade::Hard, Grade::Good, Grade::Easy];

    pub fn label(self) -> &'static str {
        match self {
            Grade::Again => "Again",
            Grade::Hard => "Hard",
            Grade::Good => "Good",
            Grade::Easy => "Easy",
        }
    }

    /// Keys 1–4, as in Anki.
    pub fn from_key(key: char) -> Option<Grade> {
        Some(match key {
            '1' => Grade::Again,
            '2' => Grade::Hard,
            '3' => Grade::Good,
            '4' => Grade::Easy,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub id: i64,
    pub note: PathBuf,
    pub question: String,
    pub answer: String,
    pub due: NaiveDate,
    /// Days until the next review; 0 for new or forgotten cards.
    pub interval: f64,
    pub ease: f64,
    pub reps: i64,
    pub lapses: i64,
}

impl Card {
    /// Where the card was written: "<course> · <lecture>".
    pub fn source(&self) -> String {
        let lecture = self
            .note
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let course = self
            .note
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if course.is_empty() {
            lecture
        } else {
            format!("{course} · {lecture}")
        }
    }

    /// The card's state after answering with `grade`.
    pub fn after(&self, grade: Grade) -> Card {
        let mut next = self.clone();
        let fresh = self.interval < 1.0;
        match grade {
            Grade::Again => {
                next.interval = 0.0;
                next.ease = (self.ease - 0.2).max(MIN_EASE);
                next.reps = 0;
                next.lapses += 1;
            }
            Grade::Hard => {
                next.interval = if fresh {
                    1.0
                } else {
                    (self.interval * 1.2).max(self.interval + 1.0)
                };
                next.ease = (self.ease - 0.15).max(MIN_EASE);
                next.reps += 1;
            }
            Grade::Good => {
                next.interval = if fresh {
                    1.0
                } else {
                    (self.interval * self.ease).max(self.interval + 1.0)
                };
                next.reps += 1;
            }
            Grade::Easy => {
                next.interval = if fresh {
                    4.0
                } else {
                    (self.interval * self.ease * 1.3).max(self.interval + 1.0)
                };
                next.ease = self.ease + 0.15;
                next.reps += 1;
            }
        }
        next
    }

    /// When the card comes back after `grade`, e.g. "now", "1d", "3w".
    pub fn preview(&self, grade: Grade) -> String {
        let days = self.after(grade).interval.round() as i64;
        match days {
            0 => "now".into(),
            d if d < 14 => format!("{d}d"),
            d if d < 60 => format!("{}w", d / 7),
            d if d < 365 => format!("{}mo", d / 30),
            d => format!("{:.1}y", d as f64 / 365.0),
        }
    }
}

/// Reads the cards from `notes` and stores new ones (due today) or updated answers. Cards whose
/// question disappeared are hidden but keep their schedule in case they come back.
pub fn sync_cards(conn: &Connection, notes: &[PathBuf], today: NaiveDate) -> Result<usize> {
    conn.execute("UPDATE cards SET missing = 1", [])?;
    let mut added = 0;
    for note in notes {
        let Ok(text) = std::fs::read_to_string(note) else {
            continue;
        };
        let Some(note) = note.to_str() else { continue };
        for card in parse(&text) {
            added += conn.execute(
                "INSERT INTO cards (note, question, answer, due) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (note, question) DO NOTHING",
                params![note, card.question, card.answer, today.to_string()],
            )?;
            conn.execute(
                "UPDATE cards SET answer = ?3, missing = 0 WHERE note = ?1 AND question = ?2",
                params![note, card.question, card.answer],
            )?;
        }
    }
    Ok(added)
}

/// Cards due on or before `today`, oldest first.
pub fn due(conn: &Connection, today: NaiveDate) -> Result<Vec<Card>> {
    let mut stmt = conn.prepare(
        "SELECT id, note, question, answer, due, interval, ease, reps, lapses FROM cards
         WHERE missing = 0 AND due <= ?1 ORDER BY due, id",
    )?;
    let rows = stmt.query_map([today.to_string()], |r| {
        let note: String = r.get(1)?;
        let due: String = r.get(4)?;
        Ok(Card {
            id: r.get(0)?,
            note: note.into(),
            question: r.get(2)?,
            answer: r.get(3)?,
            due: NaiveDate::parse_from_str(&due, "%Y-%m-%d").unwrap_or(today),
            interval: r.get(5)?,
            ease: r.get(6)?,
            reps: r.get(7)?,
            lapses: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// (due today, total) among cards still present in notes.
pub fn counts(conn: &Connection, today: NaiveDate) -> Result<(usize, usize)> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(due <= ?1), 0), COUNT(*) FROM cards WHERE missing = 0",
        [today.to_string()],
        |r| Ok((r.get::<_, i64>(0)? as usize, r.get::<_, i64>(1)? as usize)),
    )?)
}

/// Records an answer: reschedules the card and adds the time to today's review total.
/// Returns the card's new state.
pub fn grade(
    conn: &Connection,
    card: &Card,
    grade: Grade,
    today: NaiveDate,
    seconds: f64,
) -> Result<Card> {
    let mut next = card.after(grade);
    next.due = today + Duration::days(next.interval.round() as i64);
    conn.execute(
        "UPDATE cards SET due = ?2, interval = ?3, ease = ?4, reps = ?5, lapses = ?6, last_review = ?7 WHERE id = ?1",
        params![next.id, next.due.to_string(), next.interval, next.ease, next.reps, next.lapses, crate::config::now()],
    )?;
    conn.execute(
        "INSERT INTO review_daily (date, seconds, cards) VALUES (?1, ?2, 1)
         ON CONFLICT (date) DO UPDATE SET seconds = seconds + ?2, cards = cards + 1",
        params![today.to_string(), seconds.clamp(0.0, MAX_SECONDS_PER_CARD)],
    )?;
    Ok(next)
}

/// (date, seconds, cards) for every day with reviews.
pub fn daily(conn: &Connection) -> Result<Vec<(NaiveDate, f64, i64)>> {
    let mut stmt = conn.prepare("SELECT date, seconds, cards FROM review_daily ORDER BY date")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (date, seconds, cards) = row?;
        if let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
            out.push((date, seconds, cards));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cards_from_notes() {
        let note = "# Episode 3 | Raft\n\n<!-- Q: lines in comments are ignored\nA: really -->\n\
            Some notes [12:30]\n\n\
            Q: What does Raft guarantee about committed entries?\n\
            A: Every future leader has them.\n\n\
            ## Q: Two phases of leader election?\n\
            - request votes\n- win a majority\n\
            ## Next topic\n\
            Q: question without an answer\n\n\
            - Q: inline bullet?\n  A: yes\n";
        let cards = parse(note);
        assert_eq!(cards.len(), 3);
        assert_eq!(
            cards[0].question,
            "What does Raft guarantee about committed entries?"
        );
        assert_eq!(cards[0].answer, "Every future leader has them.");
        assert_eq!(cards[1].answer, "- request votes\n- win a majority");
        assert_eq!(
            cards[2],
            ParsedCard {
                question: "inline bullet?".into(),
                answer: "yes".into()
            }
        );
    }

    fn card(interval: f64, ease: f64) -> Card {
        Card {
            id: 1,
            note: "/c/Raft/notes/Episode 3.md".into(),
            question: "q".into(),
            answer: "a".into(),
            due: NaiveDate::from_ymd_opt(2026, 10, 11).unwrap(),
            interval,
            ease,
            reps: 0,
            lapses: 0,
        }
    }

    #[test]
    fn schedules_like_anki() {
        let new = card(0.0, 2.5);
        assert_eq!(new.after(Grade::Again).interval, 0.0);
        assert_eq!(new.after(Grade::Good).interval, 1.0);
        assert_eq!(new.after(Grade::Easy).interval, 4.0);
        let learned = card(4.0, 2.5);
        assert_eq!(learned.after(Grade::Good).interval, 10.0);
        assert_eq!(learned.after(Grade::Hard).interval, 5.0);
        assert_eq!(learned.after(Grade::Again).lapses, 1);
        assert!((learned.after(Grade::Again).ease - 2.3).abs() < 1e-9);
        assert_eq!(card(1.0, 1.3).after(Grade::Hard).ease, MIN_EASE);
        assert_eq!(learned.preview(Grade::Good), "10d");
        assert_eq!(card(30.0, 2.5).preview(Grade::Good), "2mo");
        assert_eq!(new.source(), "Raft · Episode 3");
    }

    #[test]
    fn stores_and_reviews_cards() {
        let tmp = tempfile::tempdir().unwrap();
        let note = tmp.path().join("Episode 1.md");
        std::fs::write(&note, "Q: one?\nA: 1\n\nQ: two?\nA: 2\n").unwrap();
        let conn = crate::db::open_in_memory().unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 10, 11).unwrap();

        assert_eq!(
            sync_cards(&conn, std::slice::from_ref(&note), today).unwrap(),
            2
        );
        assert_eq!(
            sync_cards(&conn, std::slice::from_ref(&note), today).unwrap(),
            0
        );
        assert_eq!(counts(&conn, today).unwrap(), (2, 2));

        let first = due(&conn, today).unwrap().remove(0);
        let next = grade(&conn, &first, Grade::Good, today, 30.0).unwrap();
        assert_eq!(next.due, today + Duration::days(1));
        assert_eq!(counts(&conn, today).unwrap(), (1, 2));
        assert_eq!(daily(&conn).unwrap(), [(today, 30.0, 1)]);

        // Editing the answer keeps the schedule; deleting the question hides the card.
        std::fs::write(&note, "Q: one?\nA: one\n").unwrap();
        sync_cards(&conn, &[note], today).unwrap();
        assert_eq!(counts(&conn, today).unwrap(), (0, 1));
        assert!(due(&conn, today + Duration::days(1)).unwrap()[0].answer == "one");
    }
}
