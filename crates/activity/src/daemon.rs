use std::sync::Arc;
use std::time::Duration;

use activity_core::classify::{Classifier, Snapshot, State, Verdict};
use activity_core::dose::{self, DAY, Rules, Warning};
use activity_core::policy::{Delivery, Phase, step};
use activity_core::store::{self, Occurrence, Store};
use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use async_executor::LocalExecutor;
use futures_lite::{FutureExt, StreamExt};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use parking_lot::Mutex;
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::notify::{self, NotificationsProxy};
use crate::sensors::{self, IDLE_PROBE_SECS};
use crate::service::{self, Service};

pub const MAX_CAPTION: usize = 300;

#[derive(Debug)]
pub enum Event {
    Window {
        class: String,
        caption: String,
        fullscreen: bool,
    },
    Idle(bool),
    Locked(bool),
    Media(bool),
    Action {
        notification: u32,
        key: String,
    },
    /// Reminders were edited; re-plan now.
    Changed,
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    Ack,
    Skip,
    Snooze(u32),
}

/// One delivery decided under the lock and sent after it's released.
struct Outgoing {
    occurrence: i64,
    replaces: Option<u32>,
    title: String,
    body: String,
    delivery: Delivery,
}

/// Shared between the main loop and the D-Bus service. Lock it only for
/// synchronous work; never hold it across an await.
#[derive(Debug)]
pub struct Shared {
    pub store: Store,
    classifier: Classifier,
    snapshot: Snapshot,
    verdict: Verdict,
    idle_since: Option<i64>,
    tz: TimeZone,
    grace: i64,
}

pub fn now() -> i64 {
    Timestamp::now().as_second()
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

impl Shared {
    fn apply(&mut self, event: Event, now: i64) -> Result<()> {
        match event {
            Event::Window {
                class,
                caption,
                fullscreen,
            } => {
                self.snapshot.class = truncate(&class, MAX_CAPTION);
                self.snapshot.caption = truncate(&caption, MAX_CAPTION);
                self.snapshot.fullscreen = fullscreen;
            }
            Event::Idle(true) => self.idle_since = Some(now - i64::from(IDLE_PROBE_SECS)),
            Event::Idle(false) => self.idle_since = None,
            Event::Locked(locked) => self.snapshot.locked = locked,
            Event::Media(playing) => self.snapshot.media_playing = playing,
            Event::Action { notification, key } => {
                let resolution = match key.as_str() {
                    notify::ACTION_DONE => Resolution::Ack,
                    notify::ACTION_SKIP => Resolution::Skip,
                    notify::ACTION_SNOOZE => Resolution::Snooze(notify::SNOOZE_MIN),
                    _ => return Ok(()),
                };
                if let Some(mut o) = self.store.find_by_notification(notification)? {
                    self.close(&mut o, resolution, now)?;
                }
            }
            Event::Changed | Event::Quit => {}
        }
        Ok(())
    }

    /// Resolves a reminder's open occurrence from the CLI or GUI. Returns the
    /// notification to close, if one is showing.
    pub fn resolve_reminder(
        &mut self,
        reminder_id: i64,
        r: Resolution,
        now: i64,
    ) -> Result<Option<u32>, store::Error> {
        let mut o = self
            .store
            .open_occurrence_for(reminder_id)?
            .ok_or(store::Error::NotFound(reminder_id))?;
        let shown = o.notification;
        self.close(&mut o, r, now)?;
        Ok(shown)
    }

    fn classify(&mut self, now: i64) -> Result<()> {
        self.snapshot.input_idle_secs = self
            .idle_since
            .map_or(0, |t| u64::try_from(now - t).unwrap_or(0));
        let verdict = self.classifier.classify(&self.snapshot, now);
        if verdict != self.verdict {
            if verdict.state != self.verdict.state {
                tracing::info!(state = ?verdict.state, rule = ?verdict.rule, "state changed");
            }
            self.store.log_state(
                now,
                verdict.state,
                &self.snapshot.class,
                &self.snapshot.caption,
            )?;
            self.verdict = verdict;
        }
        Ok(())
    }

    /// Creates missing occurrences and steps every open one. Returns what to
    /// send and when the loop next needs to run.
    fn plan(&mut self, now: i64) -> Result<(Vec<Outgoing>, i64)> {
        self.classify(now)?;
        let state = self.verdict.state;
        let reminders = self.store.reminders()?;
        let mut wake = now + 3600;

        for r in reminders.iter().filter(|r| r.spec.enabled) {
            if self.store.open_occurrence_for(r.id)?.is_some() {
                continue;
            }
            let next = if let Some(rules) = r.spec.schedule.doses() {
                Some(self.next_dose(r.id, rules, now)?)
            } else {
                // A new reminder starts from now; an existing one continues after its
                // last occurrence, catching up on any missed within the grace window.
                let after = self
                    .store
                    .last_due(r.id)?
                    .map_or(now, |last| last.max(now - self.grace));
                let after = Timestamp::from_second(after).context("timestamp out of range")?;
                r.spec
                    .schedule
                    .next_after(after, &self.tz)
                    .map(Timestamp::as_second)
            };
            if let Some(next) = next {
                self.store.insert_occurrence(r.id, next)?;
            }
        }

        let mut out = Vec::new();
        let mut closed = false;
        for mut o in self.store.open_occurrences()? {
            let Some(r) = reminders.iter().find(|r| r.id == o.reminder_id) else {
                continue;
            };
            if !r.spec.enabled {
                o.phase = Phase::Skipped;
                self.store.save_occurrence(&o)?;
                continue;
            }
            let s = step(o.due, o.phase, &r.spec.policy, state, now, self.grace);
            if s.phase != o.phase {
                o.phase = s.phase;
                closed |= !o.phase.is_open();
                self.store.save_occurrence(&o)?;
            }
            if let Some(delivery) = s.deliver {
                out.push(Outgoing {
                    occurrence: o.id,
                    replaces: o.notification,
                    title: r.spec.title.clone(),
                    body: r.spec.body.clone(),
                    delivery,
                });
            }
            if let Some(w) = s.wake {
                wake = wake.min(w);
            }
        }
        if closed {
            // Schedule the follow-up occurrence on the next pass.
            wake = now;
        }
        Ok((out, wake))
    }

    pub fn reminders_json(&self, now: i64) -> Result<String, store::Error> {
        let open = self.store.open_occurrences()?;
        let after = Timestamp::from_second(now).unwrap_or(Timestamp::UNIX_EPOCH);
        let mut list = Vec::new();
        for r in self.store.reminders()? {
            let occ = open.iter().find(|o| o.reminder_id == r.id);
            let (next, dose) = if let Some(rules) = r.spec.schedule.doses() {
                let doses = self.dose_history(r.id, now)?;
                let next = match occ {
                    Some(o) => o.due,
                    None => self.next_dose(r.id, rules, now)?,
                };
                let dose = json!({
                    "last": doses.last(),
                    "earliest": dose::earliest_safe(rules, &doses),
                    "in_24h": doses.iter().filter(|t| **t > now - DAY).count(),
                    "max": rules.max_per_day,
                });
                (Some(next), dose)
            } else {
                let next = occ.map(|o| o.due).or_else(|| {
                    r.spec
                        .schedule
                        .next_after(after, &self.tz)
                        .map(Timestamp::as_second)
                });
                (next, Value::Null)
            };
            list.push(
                json!({ "reminder": r, "next": next, "phase": occ.map(|o| o.phase), "dose": dose }),
            );
        }
        Ok(serde_json::to_string(&list)?)
    }

    /// Doses taken sorted ascending: two days of them, which covers the 24h cap
    /// window behind any due time, or the last one ever when that's older.
    fn dose_history(&self, id: i64, now: i64) -> Result<Vec<i64>, store::Error> {
        let mut taken: Vec<i64> = self
            .store
            .doses(Some(id), now - 2 * DAY)?
            .iter()
            .map(|d| d.taken_at)
            .collect();
        if taken.is_empty() {
            taken.extend(self.store.last_dose(id)?);
        }
        Ok(taken)
    }

    fn next_dose(&self, id: i64, rules: Rules, now: i64) -> Result<i64, store::Error> {
        let doses = self.dose_history(id, now)?;
        let missed = self
            .store
            .last_missed_due(id, doses.last().copied().unwrap_or(i64::MIN))?;
        Ok(dose::next_due(rules, &doses, missed, now))
    }

    fn dose_rules(&self, id: i64) -> Result<Rules, store::Error> {
        self.store
            .reminders()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or(store::Error::NotFound(id))?
            .spec
            .schedule
            .doses()
            .ok_or(store::Error::NotDoses(id))
    }

    /// Resolves an occurrence. Acknowledging a medicine reminder logs the dose now.
    fn close(&mut self, o: &mut Occurrence, r: Resolution, now: i64) -> Result<(), store::Error> {
        resolve(o, r, now);
        self.store.save_occurrence(o)?;
        let is_dose = match self.dose_rules(o.reminder_id) {
            Ok(_) => true,
            Err(store::Error::NotDoses(_) | store::Error::NotFound(_)) => false,
            Err(e) => return Err(e),
        };
        if matches!(r, Resolution::Ack) && is_dose {
            self.store.log_dose(o.reminder_id, now, now)?;
        }
        Ok(())
    }

    /// Logs a dose taken at `taken_at` (now or backdated). A dose newer than every
    /// logged one satisfies the open occurrence; an older one only re-times it.
    /// Returns the warnings and the notification to close, if one is showing.
    pub fn take_dose(
        &mut self,
        id: i64,
        taken_at: i64,
        now: i64,
    ) -> Result<(Vec<Warning>, Option<u32>), store::Error> {
        let rules = self.dose_rules(id)?;
        let others: Vec<i64> = self
            .store
            .doses(Some(id), taken_at - DAY)?
            .iter()
            .map(|d| d.taken_at)
            .collect();
        let warnings = dose::warnings(rules, &others, taken_at);
        let newest = self.store.last_dose(id)?.is_none_or(|l| taken_at >= l);
        self.store.log_dose(id, taken_at, now)?;
        let mut shown = None;
        if newest && let Some(mut o) = self.store.open_occurrence_for(id)? {
            shown = o.notification;
            o.phase = Phase::Acked;
            self.store.save_occurrence(&o)?;
        } else {
            self.retime_dose(id, rules, now)?;
        }
        Ok((warnings, shown))
    }

    pub fn delete_dose(&mut self, dose_id: i64, now: i64) -> Result<(), store::Error> {
        let id = self.store.delete_dose(dose_id)?;
        if let Ok(rules) = self.dose_rules(id) {
            self.retime_dose(id, rules, now)?;
        }
        Ok(())
    }

    /// Recomputes a pending dose's due time after the history changed under it.
    fn retime_dose(&mut self, id: i64, rules: Rules, now: i64) -> Result<(), store::Error> {
        if let Some(mut o) = self.store.open_occurrence_for(id)?
            && o.phase == Phase::Pending
        {
            o.due = self.next_dose(id, rules, now)?;
            self.store.save_occurrence(&o)?;
        }
        Ok(())
    }

    /// Doses from the last `days` days, newest first, with the reminder title.
    pub fn doses_json(&self, id: Option<i64>, days: u32, now: i64) -> Result<String, store::Error> {
        let titles = self.store.reminders()?;
        let list: Vec<_> = self
            .store
            .doses(id, now - i64::from(days) * DAY)?
            .into_iter()
            .rev()
            .map(|d| {
                let title = titles
                    .iter()
                    .find(|r| r.id == d.reminder_id)
                    .map_or("", |r| r.spec.title.as_str());
                json!({ "id": d.id, "reminder_id": d.reminder_id, "title": title,
                        "taken_at": d.taken_at, "logged_at": d.logged_at })
            })
            .collect();
        Ok(serde_json::to_string(&list)?)
    }

    pub fn state_json(&self) -> String {
        json!({ "verdict": self.verdict, "snapshot": self.snapshot }).to_string()
    }

    /// Spans for one local day, `date` as YYYY-MM-DD or empty for today.
    pub fn timeline_json(&self, date: &str) -> Result<String, String> {
        let day = if date.is_empty() {
            Timestamp::now().to_zoned(self.tz.clone()).date()
        } else {
            date.parse::<jiff::civil::Date>()
                .map_err(|e| e.to_string())?
        };
        let start = day.to_zoned(self.tz.clone()).map_err(|e| e.to_string())?;
        let end = start.tomorrow().map_err(|e| e.to_string())?;
        let spans = self
            .store
            .timeline(start.timestamp().as_second(), end.timestamp().as_second())
            .map_err(|e| e.to_string())?;
        serde_json::to_string(&spans).map_err(|e| e.to_string())
    }
}

fn resolve(o: &mut Occurrence, r: Resolution, now: i64) {
    match r {
        Resolution::Ack => o.phase = Phase::Acked,
        Resolution::Skip => o.phase = Phase::Skipped,
        Resolution::Snooze(min) => {
            o.phase = Phase::Pending;
            o.due = now + i64::from(min) * 60;
        }
    }
}

pub fn run(cfg: Config) -> Result<()> {
    let dir = config::data_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let shared = Arc::new(Mutex::new(Shared {
        store: Store::open(dir.join("activity.db")).context("opening database")?,
        classifier: Classifier::new(&cfg.classifier)?,
        snapshot: Snapshot::default(),
        verdict: Verdict {
            state: State::Active,
            rule: None,
        },
        idle_since: None,
        tz: TimeZone::system(),
        grace: i64::from(cfg.grace_secs),
    }));
    let (tx, rx) = async_channel::bounded(256);
    let ex = LocalExecutor::new();
    async_io::block_on(ex.run(main_loop(&ex, cfg, shared, tx, rx)))
}

async fn main_loop(
    ex: &LocalExecutor<'_>,
    cfg: Config,
    shared: Arc<Mutex<Shared>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
) -> Result<()> {
    let conn = zbus::connection::Builder::session()?
        .name(service::NAME)?
        .serve_at(
            service::PATH,
            Service {
                shared: shared.clone(),
                tx: tx.clone(),
            },
        )?
        .build()
        .await
        .context("claiming dev.requiem.Activity on the session bus (already running?)")?;
    let notifier = NotificationsProxy::new(&conn).await?;

    let spawn = |name: &'static str, fut: futures_lite::future::Boxed<Result<()>>| {
        ex.spawn(async move {
            if let Err(e) = fut.await {
                tracing::warn!(sensor = name, "stopped: {e:#}");
            }
        })
        .detach();
    };
    spawn(
        "lock",
        sensors::watch_lock(conn.clone(), tx.clone()).boxed(),
    );
    spawn(
        "media",
        sensors::watch_media(conn.clone(), tx.clone()).boxed(),
    );
    spawn(
        "notification actions",
        notify::watch_actions(conn.clone(), tx.clone()).boxed(),
    );
    let signals_tx = tx.clone();
    spawn(
        "signals",
        async move {
            let mut signals = async_signal::Signals::new([
                async_signal::Signal::Term,
                async_signal::Signal::Int,
            ])?;
            signals.next().await;
            signals_tx.send(Event::Quit).await?;
            Ok(())
        }
        .boxed(),
    );
    let idle_tx = tx.clone();
    std::thread::Builder::new()
        .name("idle".into())
        .spawn(move || {
            if let Err(e) = sensors::run_idle(idle_tx) {
                tracing::warn!("idle sensor stopped: {e:#}");
            }
        })?;
    if let Err(e) = sensors::load_kwin_script(&conn).await {
        tracing::warn!("window tracking unavailable: {e:#}");
    }
    tracing::info!("running");

    let mut wake = now();
    loop {
        // Capped so suspend, which pauses monotonic timers, can't make us oversleep.
        let secs = (wake - now()).clamp(1, 30);
        let first = async { rx.recv().await.ok() }
            .or(async {
                async_io::Timer::after(Duration::from_secs(secs.unsigned_abs())).await;
                None
            })
            .await;
        let mut quit = false;
        {
            let mut s = shared.lock();
            for event in first
                .into_iter()
                .chain(std::iter::from_fn(|| rx.try_recv().ok()))
            {
                quit |= matches!(event, Event::Quit);
                s.apply(event, now())?;
            }
        }
        if quit {
            break;
        }
        let (outgoing, next) = shared.lock().plan(now())?;
        wake = next;
        for o in outgoing {
            deliver(&conn, &notifier, &cfg, &shared, o).await;
        }
    }
    sensors::unload_kwin_script(&conn).await;
    tracing::info!("stopped");
    Ok(())
}

async fn deliver(
    conn: &zbus::Connection,
    notifier: &NotificationsProxy<'_>,
    cfg: &Config,
    shared: &Mutex<Shared>,
    o: Outgoing,
) {
    let d = o.delivery;
    tracing::info!(title = %o.title, count = d.count, phone = d.phone, ring = d.ring, "delivering");
    match notify::desktop(notifier, o.replaces, &o.title, &o.body, d.count, d.phone).await {
        Ok(id) => {
            if let Err(e) = shared.lock().store.set_notification(o.occurrence, id) {
                tracing::warn!("saving notification id: {e}");
            }
        }
        Err(e) => tracing::warn!("{e:#}"),
    }
    if d.phone {
        let message = if o.body.is_empty() {
            o.title.clone()
        } else {
            format!("{}: {}", o.title, o.body)
        };
        match notify::to_phone(conn, cfg.kdeconnect_device.as_deref(), &message, d.ring).await {
            Ok(true) => {}
            Ok(false) => tracing::info!("no reachable KDE Connect device"),
            Err(e) => tracing::warn!("{e:#}"),
        }
    }
}
