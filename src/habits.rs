//! Custom daily habits, such as running 30 minutes or eating three times. Each habit has a daily
//! goal and counts toward the day's goal and streak from the day it was added.

use crate::progress;
use anyhow::{Result, bail};
use chrono::NaiveDate;
use rusqlite::{Connection, ErrorCode, params};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Count,
    Minutes,
}

impl Unit {
    pub const ALL: [Unit; 2] = [Unit::Count, Unit::Minutes];

    fn as_str(self) -> &'static str {
        match self {
            Unit::Count => "count",
            Unit::Minutes => "minutes",
        }
    }

    fn parse(s: &str) -> Unit {
        if s == "minutes" {
            Unit::Minutes
        } else {
            Unit::Count
        }
    }

    /// The name shown when choosing how a habit is measured.
    pub fn label(self) -> &'static str {
        match self {
            Unit::Count => "Times",
            Unit::Minutes => "Minutes",
        }
    }

    /// The word for `n` of the unit: "time", "times" or "min".
    pub fn word(self, n: i64) -> &'static str {
        match self {
            Unit::Count if n == 1 => "time",
            Unit::Count => "times",
            Unit::Minutes => "min",
        }
    }

    /// "1 time", "3 times" or "30 min".
    pub fn amount(self, n: i64) -> String {
        format!("{n} {}", self.word(n))
    }

    /// How far one click of a spinner's + or − moves.
    pub fn step(self) -> i64 {
        match self {
            Unit::Count => 1,
            Unit::Minutes => 5,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Habit {
    pub id: i64,
    pub name: String,
    pub unit: Unit,
    pub goal: i64,
    /// The first study day the habit counts on.
    pub created: NaiveDate,
}

impl Habit {
    pub fn met(&self, amount: i64) -> bool {
        amount >= self.goal
    }

    /// 0 to 1: how much of the goal `amount` reaches.
    pub fn share(&self, amount: i64) -> f64 {
        if self.goal <= 0 {
            return 1.0;
        }
        (amount as f64 / self.goal as f64).clamp(0.0, 1.0)
    }

    /// "2 of 3 times" or "20 of 30 min".
    pub fn progress_text(&self, amount: i64) -> String {
        format!("{amount} of {}", self.unit.amount(self.goal))
    }
}

/// A habit as of today.
#[derive(Debug, Clone, PartialEq)]
pub struct HabitStatus {
    pub habit: Habit,
    /// Today's amount.
    pub today: i64,
    pub streak: usize,
    pub best_streak: usize,
}

impl HabitStatus {
    pub fn done(&self) -> bool {
        self.habit.met(self.today)
    }

    /// "5-day streak", "5-day streak, best 12" or "best: 12 days"; `None` before the first
    /// day the goal was met.
    pub fn streak_text(&self) -> Option<String> {
        progress::streak_text(self.streak, self.best_streak)
    }
}

/// Every habit, oldest first.
pub fn list(conn: &Connection) -> Result<Vec<Habit>> {
    let mut stmt = conn.prepare("SELECT id, name, unit, goal, created FROM habits ORDER BY id")?;
    let rows = stmt.query_map([], |r| {
        let unit: String = r.get(2)?;
        let created: String = r.get(4)?;
        Ok(Habit {
            id: r.get(0)?,
            name: r.get(1)?,
            unit: Unit::parse(&unit),
            goal: r.get(3)?,
            created: NaiveDate::parse_from_str(&created, "%Y-%m-%d").unwrap_or(NaiveDate::MIN),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Adds a habit that counts from `today` on; returns its id.
pub fn add(conn: &Connection, name: &str, unit: Unit, goal: i64, today: NaiveDate) -> Result<i64> {
    let name = checked_name(name)?;
    unique_name(
        conn.execute(
            "INSERT INTO habits (name, unit, goal, created) VALUES (?1, ?2, ?3, ?4)",
            params![name, unit.as_str(), goal.max(1), today.to_string()],
        ),
        name,
    )?;
    Ok(conn.last_insert_rowid())
}

/// Renames a habit or changes its goal. The unit stays, so past amounts keep their meaning.
pub fn update(conn: &Connection, id: i64, name: &str, goal: i64) -> Result<()> {
    let name = checked_name(name)?;
    unique_name(
        conn.execute(
            "UPDATE habits SET name = ?2, goal = ?3 WHERE id = ?1",
            params![id, name, goal.max(1)],
        ),
        name,
    )?;
    Ok(())
}

/// Deletes a habit and its history.
pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM habits WHERE id = ?1", [id])?;
    Ok(())
}

/// Sets the amount done on `date`; 0 or less clears it.
pub fn set_amount(conn: &Connection, id: i64, date: NaiveDate, amount: i64) -> Result<()> {
    if amount > 0 {
        conn.execute(
            "INSERT INTO habit_daily (date, habit_id, amount) VALUES (?1, ?2, ?3)
             ON CONFLICT (date, habit_id) DO UPDATE SET amount = ?3",
            params![date.to_string(), id, amount],
        )?;
    } else {
        conn.execute(
            "DELETE FROM habit_daily WHERE date = ?1 AND habit_id = ?2",
            params![date.to_string(), id],
        )?;
    }
    Ok(())
}

/// (date, habit id, amount) for every day something was logged, oldest first.
pub fn daily(conn: &Connection) -> Result<Vec<(NaiveDate, i64, i64)>> {
    let mut stmt =
        conn.prepare("SELECT date, habit_id, amount FROM habit_daily ORDER BY date, habit_id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)))?;
    let mut out = Vec::new();
    for row in rows {
        let (date, id, amount) = row?;
        if let Ok(date) = NaiveDate::parse_from_str(&date, "%Y-%m-%d") {
            out.push((date, id, amount));
        }
    }
    Ok(out)
}

/// Each habit with today's amount and its streaks, from `logs` as returned by [`daily`].
pub fn statuses(
    habits: &[Habit],
    logs: &[(NaiveDate, i64, i64)],
    holidays: &BTreeSet<NaiveDate>,
    today: NaiveDate,
) -> Vec<HabitStatus> {
    habits
        .iter()
        .map(|h| {
            let amounts: BTreeMap<NaiveDate, i64> = logs
                .iter()
                .filter(|l| l.1 == h.id)
                .map(|&(date, _, amount)| (date, amount))
                .collect();
            let met = |d: NaiveDate| d >= h.created && amounts.get(&d).is_some_and(|&a| h.met(a));
            let (streak, best_streak) = progress::streaks_by(
                amounts.keys().copied(),
                met,
                |d| holidays.contains(&d),
                today,
            );
            HabitStatus {
                habit: h.clone(),
                today: amounts.get(&today).copied().unwrap_or(0),
                streak,
                best_streak,
            }
        })
        .collect()
}

fn checked_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a habit needs a name");
    }
    Ok(name)
}

fn unique_name(result: rusqlite::Result<usize>, name: &str) -> Result<usize> {
    match result {
        Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == ErrorCode::ConstraintViolation => {
            bail!("you already have a habit called “{name}”")
        }
        r => Ok(r?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn stores_logs_and_deletes_habits() {
        let conn = crate::db::open_in_memory().unwrap();
        let today = d("2026-10-11");
        let eat = add(&conn, " Eat ", Unit::Count, 3, today).unwrap();
        let run = add(&conn, "Running", Unit::Minutes, 30, today).unwrap();
        assert!(add(&conn, "eat", Unit::Count, 2, today).is_err());
        assert!(add(&conn, "  ", Unit::Count, 2, today).is_err());
        assert!(update(&conn, run, "EAT", 30).is_err());

        let habits = list(&conn).unwrap();
        assert_eq!(habits.len(), 2);
        assert_eq!(habits[0].name, "Eat");
        assert_eq!(habits[1].unit, Unit::Minutes);
        assert_eq!(habits[1].created, today);

        set_amount(&conn, eat, today, 2).unwrap();
        set_amount(&conn, eat, today, 3).unwrap();
        set_amount(&conn, run, today, 20).unwrap();
        assert_eq!(daily(&conn).unwrap(), [(today, eat, 3), (today, run, 20)]);
        set_amount(&conn, run, today, 0).unwrap();
        assert_eq!(daily(&conn).unwrap(), [(today, eat, 3)]);

        update(&conn, eat, "Meals", 4).unwrap();
        assert_eq!(list(&conn).unwrap()[0].goal, 4);
        delete(&conn, eat).unwrap();
        assert!(
            daily(&conn).unwrap().is_empty(),
            "history goes with the habit"
        );
        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn streaks_count_days_the_goal_was_met() {
        let today = d("2026-10-11");
        let habit = Habit {
            id: 1,
            name: "Eat".into(),
            unit: Unit::Count,
            goal: 3,
            created: d("2026-10-05"),
        };
        let day = |n: i64| today - Duration::days(n);
        let logs = [
            // Before the habit existed: doesn't count.
            (day(7), 1, 3),
            (day(5), 1, 3),
            (day(4), 1, 3),
            (day(3), 1, 3),
            (day(2), 1, 1),
            (day(1), 1, 3),
            (today, 1, 2),
        ];
        let s = &statuses(std::slice::from_ref(&habit), &logs, &BTreeSet::new(), today)[0];
        assert_eq!((s.today, s.streak, s.best_streak), (2, 1, 3));
        assert!(!s.done());
        assert_eq!(s.streak_text().as_deref(), Some("1-day streak, best 3"));
        assert_eq!(habit.progress_text(2), "2 of 3 times");
        assert_eq!(habit.share(2), 2.0 / 3.0);
        assert_eq!(habit.share(5), 1.0);
    }
}
