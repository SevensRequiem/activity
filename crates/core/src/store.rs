use std::path::Path;

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::classify::State;
use crate::policy::{Phase, Policy};
use crate::schedule::Schedule;

pub const MAX_TITLE: usize = 200;
pub const MAX_BODY: usize = 2000;

/// A reminder as clients send it. Validated before it reaches the database.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReminderSpec {
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub schedule: Schedule,
    #[serde(default)]
    pub policy: Policy,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

fn enabled_default() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reminder {
    pub id: i64,
    #[serde(flatten)]
    pub spec: ReminderSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Occurrence {
    pub id: i64,
    pub reminder_id: i64,
    pub due: i64,
    pub phase: Phase,
    pub notification: Option<u32>,
}

/// One dose as actually taken. `logged_at` differs from `taken_at` when backdated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Dose {
    pub id: i64,
    pub reminder_id: i64,
    pub taken_at: i64,
    pub logged_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Span {
    pub start: i64,
    pub end: Option<i64>,
    pub state: State,
    pub class: String,
    pub caption: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("title must be 1 to {MAX_TITLE} characters")]
    Title,
    #[error("body must be at most {MAX_BODY} characters")]
    Body,
    #[error(transparent)]
    Schedule(#[from] crate::schedule::Error),
    #[error(transparent)]
    Policy(#[from] crate::policy::Error),
    #[error("no reminder with id {0}")]
    NotFound(i64),
    #[error("no dose with id {0}")]
    NoDose(i64),
    #[error("reminder {0} is not a medicine reminder")]
    NotDoses(i64),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error("stored row is corrupt: {0}")]
    Corrupt(#[from] serde_json::Error),
}

impl ReminderSpec {
    pub fn validate(&self) -> Result<(), Error> {
        let title = self.title.trim().chars().count();
        if title == 0 || title > MAX_TITLE {
            return Err(Error::Title);
        }
        if self.body.chars().count() > MAX_BODY {
            return Err(Error::Body);
        }
        self.schedule.validate()?;
        self.policy.validate()?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct Store {
    db: Connection,
}

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS reminders (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    schedule TEXT NOT NULL,
    policy TEXT NOT NULL,
    enabled INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS occurrences (
    id INTEGER PRIMARY KEY,
    reminder_id INTEGER NOT NULL REFERENCES reminders(id) ON DELETE CASCADE,
    due INTEGER NOT NULL,
    phase TEXT NOT NULL,
    open INTEGER NOT NULL,
    notification INTEGER
);
CREATE INDEX IF NOT EXISTS occurrences_open ON occurrences(open, reminder_id);
CREATE TABLE IF NOT EXISTS activity_log (
    id INTEGER PRIMARY KEY,
    start INTEGER NOT NULL,
    end INTEGER,
    state TEXT NOT NULL,
    class TEXT NOT NULL,
    caption TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS activity_log_start ON activity_log(start);
CREATE TABLE IF NOT EXISTS doses (
    id INTEGER PRIMARY KEY,
    reminder_id INTEGER NOT NULL REFERENCES reminders(id) ON DELETE CASCADE,
    taken_at INTEGER NOT NULL,
    logged_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS doses_taken ON doses(reminder_id, taken_at);
";

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self, Error> {
        db.execute_batch(SCHEMA)?;
        Ok(Self { db })
    }

    pub fn add_reminder(&self, spec: &ReminderSpec) -> Result<i64, Error> {
        spec.validate()?;
        self.db.execute(
            "INSERT INTO reminders (title, body, schedule, policy, enabled) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                spec.title.trim(),
                spec.body,
                serde_json::to_string(&spec.schedule)?,
                serde_json::to_string(&spec.policy)?,
                spec.enabled
            ],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Replaces the reminder and drops its open occurrence so the new schedule takes effect.
    pub fn update_reminder(&self, id: i64, spec: &ReminderSpec) -> Result<(), Error> {
        spec.validate()?;
        let n = self.db.execute(
            "UPDATE reminders SET title = ?2, body = ?3, schedule = ?4, policy = ?5, enabled = ?6 WHERE id = ?1",
            params![
                id,
                spec.title.trim(),
                spec.body,
                serde_json::to_string(&spec.schedule)?,
                serde_json::to_string(&spec.policy)?,
                spec.enabled
            ],
        )?;
        if n == 0 {
            return Err(Error::NotFound(id));
        }
        self.db.execute(
            "DELETE FROM occurrences WHERE reminder_id = ?1 AND open = 1",
            [id],
        )?;
        Ok(())
    }

    pub fn delete_reminder(&self, id: i64) -> Result<(), Error> {
        if self
            .db
            .execute("DELETE FROM reminders WHERE id = ?1", [id])?
            == 0
        {
            return Err(Error::NotFound(id));
        }
        Ok(())
    }

    pub fn reminders(&self) -> Result<Vec<Reminder>, Error> {
        let mut stmt = self.db.prepare(
            "SELECT id, title, body, schedule, policy, enabled FROM reminders ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, title, body, schedule, policy, enabled) = row?;
            Ok(Reminder {
                id,
                spec: ReminderSpec {
                    title,
                    body,
                    schedule: serde_json::from_str(&schedule)?,
                    policy: serde_json::from_str(&policy)?,
                    enabled,
                },
            })
        })
        .collect()
    }

    pub fn open_occurrences(&self) -> Result<Vec<Occurrence>, Error> {
        self.occurrences("open = 1", [])
    }

    pub fn open_occurrence_for(&self, reminder_id: i64) -> Result<Option<Occurrence>, Error> {
        Ok(self
            .occurrences("open = 1 AND reminder_id = ?1", [reminder_id])?
            .pop())
    }

    pub fn find_by_notification(&self, notification: u32) -> Result<Option<Occurrence>, Error> {
        Ok(self
            .occurrences("open = 1 AND notification = ?1", [i64::from(notification)])?
            .pop())
    }

    fn occurrences<P: rusqlite::Params>(
        &self,
        filter: &str,
        args: P,
    ) -> Result<Vec<Occurrence>, Error> {
        let mut stmt = self.db.prepare(&format!(
            "SELECT id, reminder_id, due, phase, notification FROM occurrences WHERE {filter} ORDER BY due"
        ))?;
        let rows = stmt.query_map(args, |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<u32>>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (id, reminder_id, due, phase, notification) = row?;
            Ok(Occurrence {
                id,
                reminder_id,
                due,
                phase: serde_json::from_str(&phase)?,
                notification,
            })
        })
        .collect()
    }

    pub fn last_due(&self, reminder_id: i64) -> Result<Option<i64>, Error> {
        Ok(self.db.query_row(
            "SELECT MAX(due) FROM occurrences WHERE reminder_id = ?1",
            [reminder_id],
            |r| r.get(0),
        )?)
    }

    /// Due time of the latest dose occurrence after `since` that closed without
    /// being taken (skipped or expired), which restarts dose spacing. Acked ones
    /// don't count: the dose log has the real time.
    pub fn last_missed_due(&self, reminder_id: i64, since: i64) -> Result<Option<i64>, Error> {
        Ok(self.db.query_row(
            "SELECT MAX(due) FROM occurrences WHERE reminder_id = ?1 AND open = 0 AND phase != ?2 AND due > ?3",
            params![reminder_id, serde_json::to_string(&Phase::Acked)?, since],
            |r| r.get(0),
        )?)
    }

    pub fn log_dose(&self, reminder_id: i64, taken_at: i64, logged_at: i64) -> Result<i64, Error> {
        self.db.execute(
            "INSERT INTO doses (reminder_id, taken_at, logged_at) VALUES (?1, ?2, ?3)",
            [reminder_id, taken_at, logged_at],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    /// Returns the reminder the dose belonged to.
    pub fn delete_dose(&self, id: i64) -> Result<i64, Error> {
        self.db
            .query_row(
                "DELETE FROM doses WHERE id = ?1 RETURNING reminder_id",
                [id],
                |r| r.get(0),
            )
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Error::NoDose(id),
                e => e.into(),
            })
    }

    /// Doses taken at or after `since`, oldest first; all reminders when `reminder_id` is `None`.
    pub fn doses(&self, reminder_id: Option<i64>, since: i64) -> Result<Vec<Dose>, Error> {
        let mut stmt = self.db.prepare(
            "SELECT id, reminder_id, taken_at, logged_at FROM doses
             WHERE taken_at >= ?2 AND (?1 IS NULL OR reminder_id = ?1) ORDER BY taken_at",
        )?;
        let rows = stmt.query_map(params![reminder_id, since], |r| {
            Ok(Dose {
                id: r.get(0)?,
                reminder_id: r.get(1)?,
                taken_at: r.get(2)?,
                logged_at: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn last_dose(&self, reminder_id: i64) -> Result<Option<i64>, Error> {
        Ok(self.db.query_row(
            "SELECT MAX(taken_at) FROM doses WHERE reminder_id = ?1",
            [reminder_id],
            |r| r.get(0),
        )?)
    }

    /// Touches only the notification column, so it can't overwrite a phase
    /// change that landed while the notification was being sent.
    pub fn set_notification(&self, occurrence: i64, notification: u32) -> Result<(), Error> {
        self.db.execute(
            "UPDATE occurrences SET notification = ?2 WHERE id = ?1",
            params![occurrence, notification],
        )?;
        Ok(())
    }

    pub fn insert_occurrence(&self, reminder_id: i64, due: i64) -> Result<i64, Error> {
        self.db.execute(
            "INSERT INTO occurrences (reminder_id, due, phase, open) VALUES (?1, ?2, ?3, 1)",
            params![reminder_id, due, serde_json::to_string(&Phase::Pending)?],
        )?;
        Ok(self.db.last_insert_rowid())
    }

    pub fn save_occurrence(&self, o: &Occurrence) -> Result<(), Error> {
        self.db.execute(
            "UPDATE occurrences SET due = ?2, phase = ?3, open = ?4, notification = ?5 WHERE id = ?1",
            params![
                o.id,
                o.due,
                serde_json::to_string(&o.phase)?,
                o.phase.is_open(),
                o.notification
            ],
        )?;
        Ok(())
    }

    /// Closes the running span and opens a new one.
    pub fn log_state(
        &self,
        now: i64,
        state: State,
        class: &str,
        caption: &str,
    ) -> Result<(), Error> {
        self.db
            .execute("UPDATE activity_log SET end = ?1 WHERE end IS NULL", [now])?;
        self.db.execute(
            "INSERT INTO activity_log (start, state, class, caption) VALUES (?1, ?2, ?3, ?4)",
            params![now, serde_json::to_string(&state)?, class, caption],
        )?;
        Ok(())
    }

    pub fn timeline(&self, from: i64, to: i64) -> Result<Vec<Span>, Error> {
        let mut stmt = self.db.prepare(
            "SELECT start, end, state, class, caption FROM activity_log
             WHERE start < ?2 AND (end IS NULL OR end > ?1) ORDER BY start",
        )?;
        let rows = stmt.query_map([from, to], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (start, end, state, class, caption) = row?;
            Ok(Span {
                start,
                end,
                state: serde_json::from_str(&state)?,
                class,
                caption,
            })
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(title: &str) -> ReminderSpec {
        serde_json::from_str(&format!(
            r#"{{"title":"{title}","schedule":{{"kind":"daily","times":["09:00"]}}}}"#
        ))
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn store() -> Store {
        Store::open_in_memory().unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn reminder_round_trip_and_cascade() -> Result<(), Error> {
        let s = store();
        let id = s.add_reminder(&spec("stretch"))?;
        let all = s.reminders()?;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].spec.policy, Policy::default());
        assert!(all[0].spec.enabled);

        let occ = s.insert_occurrence(id, 100)?;
        let mut o = s
            .open_occurrence_for(id)?
            .unwrap_or_else(|| panic!("missing occurrence"));
        assert_eq!(o.id, occ);
        o.phase = Phase::Delivered {
            count: 1,
            last: 100,
        };
        o.notification = Some(7);
        s.save_occurrence(&o)?;
        assert_eq!(s.find_by_notification(7)?.map(|o| o.phase), Some(o.phase));

        o.phase = Phase::Acked;
        s.save_occurrence(&o)?;
        assert_eq!(s.open_occurrences()?, Vec::new());

        s.insert_occurrence(id, 200)?;
        s.delete_reminder(id)?;
        assert_eq!(s.open_occurrences()?, Vec::new());
        assert!(matches!(s.delete_reminder(id), Err(Error::NotFound(_))));
        Ok(())
    }

    #[test]
    fn update_drops_open_occurrence() -> Result<(), Error> {
        let s = store();
        let id = s.add_reminder(&spec("water"))?;
        s.insert_occurrence(id, 100)?;
        s.update_reminder(id, &spec("more water"))?;
        assert_eq!(s.open_occurrences()?, Vec::new());
        assert_eq!(s.reminders()?[0].spec.title, "more water");
        assert!(matches!(
            s.update_reminder(999, &spec("x")),
            Err(Error::NotFound(999))
        ));
        Ok(())
    }

    #[test]
    fn title_bounds() {
        let s = store();
        let cases = [
            ("empty", String::new(), false),
            ("whitespace", "   ".into(), false),
            ("at limit", "a".repeat(MAX_TITLE), true),
            ("over limit", "a".repeat(MAX_TITLE + 1), false),
        ];
        for (name, title, ok) in cases {
            let mut sp = spec("x");
            sp.title = title;
            assert_eq!(s.add_reminder(&sp).is_ok(), ok, "case {name}");
        }
    }

    #[test]
    fn unknown_fields_rejected() {
        let bad = r#"{"title":"x","schedule":{"kind":"daily","times":["09:00"]},"evil":1}"#;
        assert!(serde_json::from_str::<ReminderSpec>(bad).is_err());
    }

    #[test]
    fn dose_log() -> Result<(), Error> {
        let s = store();
        let a = s.add_reminder(&spec("meds"))?;
        let b = s.add_reminder(&spec("other"))?;
        let d1 = s.log_dose(a, 100, 100)?;
        s.log_dose(a, 50, 300)?;
        s.log_dose(b, 200, 200)?;
        let taken: Vec<i64> = s.doses(Some(a), 0)?.iter().map(|d| d.taken_at).collect();
        assert_eq!(taken, [50, 100]);
        assert_eq!(s.doses(None, 0)?.len(), 3);
        assert_eq!(s.doses(None, 101)?.len(), 1);
        assert_eq!(s.last_dose(a)?, Some(100));

        let skipped = s.insert_occurrence(a, 400)?;
        s.save_occurrence(&Occurrence {
            id: skipped,
            reminder_id: a,
            due: 400,
            phase: Phase::Skipped,
            notification: None,
        })?;
        assert_eq!(s.last_missed_due(a, 100)?, Some(400));
        assert_eq!(s.last_missed_due(a, 400)?, None);

        s.delete_dose(d1)?;
        assert_eq!(s.last_dose(a)?, Some(50));
        assert!(matches!(s.delete_dose(d1), Err(Error::NoDose(_))));
        s.delete_reminder(a)?;
        assert_eq!(s.doses(None, 0)?.len(), 1);
        Ok(())
    }

    #[test]
    fn timeline_spans() -> Result<(), Error> {
        let s = store();
        s.log_state(100, State::Browsing, "firefox", "a")?;
        s.log_state(200, State::Gaming, "steam_app_1", "b")?;
        let spans = s.timeline(150, 1000)?;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].end, Some(200));
        assert_eq!(spans[1].end, None);
        assert_eq!(s.timeline(0, 100)?.len(), 0);
        Ok(())
    }
}
