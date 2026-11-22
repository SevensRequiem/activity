use std::sync::Arc;

use activity_core::dose::DAY;
use activity_core::store::{self, ReminderSpec};
use async_channel::Sender;
use parking_lot::Mutex;
use zbus::fdo;

use crate::daemon::{self, Event, MAX_CAPTION, Resolution, Shared};
use crate::notify::NotificationsProxy;

pub const NAME: &str = "dev.requiem.Activity";
pub const PATH: &str = "/dev/requiem/Activity";
const MAX_JSON: usize = 16 * 1024;

pub struct Service {
    pub shared: Arc<Mutex<Shared>>,
    pub tx: Sender<Event>,
}

fn to_fdo(e: &store::Error) -> fdo::Error {
    match e {
        store::Error::Sql(_) | store::Error::Corrupt(_) => {
            tracing::warn!("store: {e}");
            fdo::Error::Failed("storage error".into())
        }
        _ => fdo::Error::InvalidArgs(e.to_string()),
    }
}

fn parse_spec(json: &str) -> fdo::Result<ReminderSpec> {
    if json.len() > MAX_JSON {
        return Err(fdo::Error::InvalidArgs(format!(
            "reminder JSON over {MAX_JSON} bytes"
        )));
    }
    serde_json::from_str(json).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))
}

/// Ids travel as strings because QML can't easily send a typed int64.
fn parse_id(id: &str) -> fdo::Result<i64> {
    id.trim()
        .parse()
        .map_err(|_| fdo::Error::InvalidArgs(format!("bad id {id:?}")))
}

fn parse_days(days: &str) -> fdo::Result<u32> {
    days.trim()
        .parse::<u32>()
        .ok()
        .filter(|d| (1..=90).contains(d))
        .ok_or_else(|| fdo::Error::InvalidArgs("days must be 1 to 90".into()))
}

impl Service {
    async fn changed(&self) {
        // Only fails when the daemon is shutting down.
        let _ = self.tx.send(Event::Changed).await;
    }

    async fn resolve(&self, conn: &zbus::Connection, id: i64, r: Resolution) -> fdo::Result<()> {
        let shown = self
            .shared
            .lock()
            .resolve_reminder(id, r, daemon::now())
            .map_err(|e| to_fdo(&e))?;
        if let Some(n) = shown
            && let Ok(proxy) = NotificationsProxy::new(conn).await
        {
            let _ = proxy.close_notification(n).await;
        }
        self.changed().await;
        Ok(())
    }
}

/// Payloads are JSON strings so the QML side needs no D-Bus type plumbing.
#[zbus::interface(name = "dev.requiem.Activity")]
impl Service {
    fn list_reminders(&self) -> fdo::Result<String> {
        self.shared
            .lock()
            .reminders_json(daemon::now())
            .map_err(|e| to_fdo(&e))
    }

    async fn add_reminder(&self, json: &str) -> fdo::Result<i64> {
        let spec = parse_spec(json)?;
        let id = self
            .shared
            .lock()
            .store
            .add_reminder(&spec)
            .map_err(|e| to_fdo(&e))?;
        self.changed().await;
        Ok(id)
    }

    async fn update_reminder(&self, id: &str, json: &str) -> fdo::Result<()> {
        let spec = parse_spec(json)?;
        self.shared
            .lock()
            .store
            .update_reminder(parse_id(id)?, &spec)
            .map_err(|e| to_fdo(&e))?;
        self.changed().await;
        Ok(())
    }

    async fn delete_reminder(&self, id: &str) -> fdo::Result<()> {
        self.shared
            .lock()
            .store
            .delete_reminder(parse_id(id)?)
            .map_err(|e| to_fdo(&e))?;
        self.changed().await;
        Ok(())
    }

    async fn ack(&self, #[zbus(connection)] conn: &zbus::Connection, id: &str) -> fdo::Result<()> {
        self.resolve(conn, parse_id(id)?, Resolution::Ack).await
    }

    async fn skip(&self, #[zbus(connection)] conn: &zbus::Connection, id: &str) -> fdo::Result<()> {
        self.resolve(conn, parse_id(id)?, Resolution::Skip).await
    }

    async fn snooze(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        id: &str,
        minutes: &str,
    ) -> fdo::Result<()> {
        let minutes = minutes
            .parse::<u32>()
            .ok()
            .filter(|m| (1..=1440).contains(m))
            .ok_or_else(|| fdo::Error::InvalidArgs("snooze must be 1 to 1440 minutes".into()))?;
        self.resolve(conn, parse_id(id)?, Resolution::Snooze(minutes))
            .await
    }

    /// Logs a dose of a medicine reminder. `at` is unix seconds, or empty for
    /// now; up to two days back. Returns the warnings as JSON.
    async fn take_dose(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        id: &str,
        at: &str,
    ) -> fdo::Result<String> {
        let now = daemon::now();
        let taken_at = if at.trim().is_empty() {
            now
        } else {
            at.trim()
                .parse::<i64>()
                .ok()
                .filter(|t| (now - 2 * DAY..=now + 60).contains(t))
                .ok_or_else(|| {
                    fdo::Error::InvalidArgs("dose time must be within the last two days".into())
                })?
                .min(now)
        };
        let (warnings, shown) = self
            .shared
            .lock()
            .take_dose(parse_id(id)?, taken_at, now)
            .map_err(|e| to_fdo(&e))?;
        if let Some(n) = shown
            && let Ok(proxy) = NotificationsProxy::new(conn).await
        {
            let _ = proxy.close_notification(n).await;
        }
        self.changed().await;
        serde_json::to_string(&warnings).map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn delete_dose(&self, dose_id: &str) -> fdo::Result<()> {
        self.shared
            .lock()
            .delete_dose(parse_id(dose_id)?, daemon::now())
            .map_err(|e| to_fdo(&e))?;
        self.changed().await;
        Ok(())
    }

    /// Doses from the last `days` days (1 to 90); `id` empty means every medicine.
    fn doses(&self, id: &str, days: &str) -> fdo::Result<String> {
        let id = if id.trim().is_empty() {
            None
        } else {
            Some(parse_id(id)?)
        };
        self.shared
            .lock()
            .doses_json(id, parse_days(days)?, daemon::now())
            .map_err(|e| to_fdo(&e))
    }

    fn current_state(&self) -> String {
        self.shared.lock().state_json()
    }

    fn timeline(&self, date: &str) -> fdo::Result<String> {
        self.shared
            .lock()
            .timeline_json(date, daemon::now())
            .map_err(fdo::Error::InvalidArgs)
    }

    /// Usage over the last `days` local days (1 to 90) and the same number
    /// of days before them.
    fn stats(&self, days: &str) -> fdo::Result<String> {
        self.shared
            .lock()
            .stats_json(parse_days(days)?, daemon::now())
            .map_err(|e| {
                tracing::warn!("stats: {e:#}");
                fdo::Error::Failed("stats unavailable".into())
            })
    }

    /// Called by the `KWin` script on focus, caption and fullscreen changes.
    async fn window_activated(&self, caption: &str, class: &str, fullscreen: bool) {
        let _ = self
            .tx
            .send(Event::Window {
                class: class.chars().take(MAX_CAPTION).collect(),
                caption: caption.chars().take(MAX_CAPTION).collect(),
                fullscreen,
            })
            .await;
    }
}
