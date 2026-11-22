mod config;
mod daemon;
mod notify;
mod sensors;
mod service;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde_json::{Value, json};

#[derive(Parser)]
#[command(about = "Activity-aware reminders for KDE Plasma")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum HoldArg {
    Never,
    Busy,
}

#[derive(clap::Args)]
struct AddArgs {
    title: String,
    #[arg(long, default_value = "")]
    body: String,
    /// One-off local time, e.g. 2026-11-23T15:00.
    #[arg(long)]
    at: Option<String>,
    /// Comma-separated local times, e.g. 09:00,14:30.
    #[arg(long)]
    daily: Option<String>,
    /// Interval in minutes, between --from and --to.
    #[arg(long)]
    every: Option<u32>,
    #[arg(long, default_value = "00:00")]
    from: String,
    #[arg(long, default_value = "23:59")]
    to: String,
    /// Comma-separated weekdays (mon,tue,...); default every day.
    #[arg(long)]
    days: Option<String>,
    /// Medicine: each dose due this long after the last one taken, e.g. 6h or 5h30m.
    #[arg(long, value_parser = minutes)]
    doses: Option<u32>,
    /// Medicine: warn when doses are closer than this, e.g. 4h. Defaults to the interval.
    #[arg(long, value_parser = minutes)]
    min_gap: Option<u32>,
    /// Medicine: at most this many doses in any 24 hours; 0 for no cap.
    #[arg(long, default_value_t = 0)]
    max_day: u32,
    /// Hold the reminder while you're gaming, watching or focused. Default
    /// busy, or never for medicine.
    #[arg(long, value_enum)]
    hold: Option<HoldArg>,
    /// While away or asleep, wait instead of pinging the phone. On by default
    /// for medicine.
    #[arg(long)]
    pause_away: bool,
    /// Re-ping every N minutes until acknowledged; 0 pings once.
    #[arg(long, default_value_t = 10)]
    repeat: u32,
    #[arg(long, default_value_t = 3)]
    max_repeats: u32,
    /// From the Nth ping on, also ping the phone; 0 never.
    #[arg(long, default_value_t = 3)]
    escalate: u32,
    /// Ring the phone on the final ping.
    #[arg(long)]
    ring: bool,
    /// Deliver anyway after holding this many minutes.
    #[arg(long, default_value_t = 120)]
    max_defer: u32,
}

impl AddArgs {
    fn spec(self) -> Result<Value> {
        let days: Vec<&str> = self.days.as_deref().map(split).unwrap_or_default();
        let medicine = self.doses.is_some();
        let schedule = match (self.at, self.daily, self.every, self.doses) {
            (Some(at), None, None, None) => json!({ "kind": "once", "at": at }),
            (None, None, None, Some(every)) => json!({
                "kind": "doses",
                "every_min": every,
                "min_gap_min": self.min_gap.unwrap_or(every),
                "max_per_day": self.max_day,
            }),
            (None, Some(times), None, None) => {
                json!({ "kind": "daily", "times": split(&times), "days": days })
            }
            (None, None, Some(minutes), None) => {
                json!({ "kind": "every", "minutes": minutes, "start": self.from, "end": self.to, "days": days })
            }
            _ => bail!("give exactly one of --at, --daily, --every or --doses"),
        };
        Ok(json!({
            "title": self.title,
            "body": self.body,
            "schedule": schedule,
            "policy": {
                "hold": if self.hold.map_or(medicine, |h| matches!(h, HoldArg::Never)) { "never" } else { "when_busy" },
                "pause_when_away": self.pause_away || medicine,
                "keep_open": medicine,
                "repeat_every_min": self.repeat,
                "max_repeats": self.max_repeats,
                "escalate_after": self.escalate,
                "ring_phone": self.ring,
                "max_defer_min": self.max_defer,
            },
        }))
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the background daemon (normally started by systemd).
    Daemon,
    /// Add a reminder. Give exactly one of --at, --daily, --every or --doses.
    Add(AddArgs),
    /// List reminders with their next time.
    List,
    /// Delete a reminder.
    Rm { id: i64 },
    /// Acknowledge a reminder's current ping.
    Ack { id: i64 },
    /// Skip a reminder's current ping.
    Skip { id: i64 },
    /// Push a reminder's current ping back.
    Snooze {
        id: i64,
        #[arg(default_value_t = 10)]
        minutes: u32,
    },
    /// Log a medicine dose, now or backdated with --ago or --at.
    Took {
        id: i64,
        /// How long ago, e.g. 20m or 1h30m.
        #[arg(long, value_parser = minutes, conflicts_with = "at")]
        ago: Option<u32>,
        /// Local time taken, HH:MM (the most recent one) or YYYY-MM-DDTHH:MM.
        #[arg(long)]
        at: Option<String>,
    },
    /// Show logged doses with the gap since the previous one.
    Doses {
        /// Only this medicine; default all.
        id: Option<i64>,
        #[arg(long, default_value_t = 3)]
        days: u32,
    },
    /// Delete a logged dose by its dose id (shown by `doses`).
    RmDose { dose_id: i64 },
    /// Show what the daemon thinks you're doing.
    State,
    /// Show a day's activity, YYYY-MM-DD, default today.
    Timeline { date: Option<String> },
    /// Print the default config.toml.
    DefaultConfig,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Daemon => daemon::run(config::load()?),
        Cmd::DefaultConfig => {
            print!("{}", toml::to_string_pretty(&config::Config::default())?);
            Ok(())
        }
        cmd => async_io::block_on(client(cmd)),
    }
}

async fn call<B>(conn: &zbus::Connection, method: &str, args: &B) -> Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    conn.call_method(
        Some(service::NAME),
        service::PATH,
        Some(service::NAME),
        method,
        args,
    )
    .await
    .with_context(|| format!("{method} (is the daemon running?)"))
}

async fn text<B>(conn: &zbus::Connection, method: &str, args: &B) -> Result<String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    Ok(call(conn, method, args).await?.body().deserialize()?)
}

fn local(secs: i64) -> String {
    Timestamp::from_second(secs).map_or_else(
        |_| "?".into(),
        |t| {
            t.to_zoned(TimeZone::system())
                .strftime("%a %Y-%m-%d %H:%M")
                .to_string()
        },
    )
}

/// Parses a duration like 6h, 90m or 5h30m into whole minutes.
fn minutes(s: &str) -> Result<u32, String> {
    let d: jiff::SignedDuration = s.parse().map_err(|e| format!("{e}"))?;
    u32::try_from(d.as_secs() / 60)
        .ok()
        .filter(|m| *m > 0)
        .ok_or_else(|| "must be at least a minute".into())
}

/// A local HH:MM means its most recent occurrence; a full date-time is taken as is.
fn taken_at(s: &str) -> Result<i64> {
    let tz = TimeZone::system();
    if let Ok(dt) = s.parse::<jiff::civil::DateTime>() {
        return Ok(dt.to_zoned(tz)?.timestamp().as_second());
    }
    let time: jiff::civil::Time = s.parse().context("expected HH:MM or YYYY-MM-DDTHH:MM")?;
    let now = Timestamp::now().to_zoned(tz.clone());
    let mut at = now.date().to_datetime(time).to_zoned(tz)?;
    if at > now {
        at = at.yesterday()?;
    }
    Ok(at.timestamp().as_second())
}

fn print_list(text: &str) -> Result<()> {
    let list: Vec<Value> = serde_json::from_str(text)?;
    if list.is_empty() {
        println!("no reminders");
    }
    for item in list {
        let r = &item["reminder"];
        let next = item["next"].as_i64().map_or_else(|| "never".into(), local);
        let phase = item["phase"]["phase"].as_str().unwrap_or("scheduled");
        let off = if r["enabled"].as_bool() == Some(false) {
            " (disabled)"
        } else {
            ""
        };
        println!(
            "{:>4}  {:<30}  {}  [{phase}]  {}{off}",
            r["id"],
            r["title"].as_str().unwrap_or(""),
            next,
            r["schedule"]
        );
        let d = &item["dose"];
        if !d.is_null() {
            let at = |k: &str| d[k].as_i64().map_or_else(|| "—".into(), local);
            println!(
                "      last dose {}  ·  safe from {}  ·  {} in 24h (max {})",
                at("last"),
                at("earliest"),
                d["in_24h"],
                d["max"]
            );
        }
    }
    Ok(())
}

fn print_warnings(text: &str) -> Result<()> {
    for w in serde_json::from_str::<Vec<Value>>(text)? {
        match w["kind"].as_str() {
            Some("too_soon") => println!(
                "warning: only {} min from another dose (minimum {} min)",
                w["gap_min"], w["min_gap_min"]
            ),
            Some("over_max") => println!(
                "warning: {} doses in 24 hours (maximum {})",
                w["count"], w["max"]
            ),
            _ => println!("warning: {w}"),
        }
    }
    Ok(())
}

/// Oldest first, with the gap since the previous dose of the same medicine.
fn print_doses(text: &str) -> Result<()> {
    let mut list: Vec<Value> = serde_json::from_str(text)?;
    list.reverse();
    let mut prev: Vec<(i64, i64)> = Vec::new();
    for d in list {
        let rid = d["reminder_id"].as_i64().unwrap_or(0);
        let taken = d["taken_at"].as_i64().unwrap_or(0);
        let gap = prev
            .iter()
            .find(|(r, _)| *r == rid)
            .map_or_else(String::new, |(_, t)| {
                let m = (taken - t) / 60;
                format!("+{}h{:02}m", m / 60, m % 60)
            });
        prev.retain(|(r, _)| *r != rid);
        prev.push((rid, taken));
        let backdated = if d["logged_at"].as_i64().unwrap_or(taken) - taken > 120 {
            " (backdated)"
        } else {
            ""
        };
        println!(
            "{:>5}  {}  {:>8}  {}{backdated}",
            d["id"],
            local(taken),
            gap,
            d["title"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

fn split(list: &str) -> Vec<&str> {
    list.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

async fn client(cmd: Cmd) -> Result<()> {
    let conn = zbus::Connection::session().await?;
    match cmd {
        Cmd::Add(args) => {
            let spec = args.spec()?;
            let id: i64 = call(&conn, "AddReminder", &(spec.to_string(),))
                .await?
                .body()
                .deserialize()?;
            println!("added reminder {id}");
        }
        Cmd::List => print_list(&text(&conn, "ListReminders", &()).await?)?,
        Cmd::Rm { id } => {
            call(&conn, "DeleteReminder", &(id.to_string(),)).await?;
        }
        Cmd::Ack { id } => {
            call(&conn, "Ack", &(id.to_string(),)).await?;
        }
        Cmd::Skip { id } => {
            call(&conn, "Skip", &(id.to_string(),)).await?;
        }
        Cmd::Snooze { id, minutes } => {
            call(&conn, "Snooze", &(id.to_string(), minutes.to_string())).await?;
        }
        Cmd::State => {
            let text = text(&conn, "CurrentState", &()).await?;
            let v: Value = serde_json::from_str(&text)?;
            let s = &v["snapshot"];
            println!(
                "{}  (rule: {})\nwindow: {} — {}\nfullscreen: {}  media: {}  idle: {}s  locked: {}",
                v["verdict"]["state"].as_str().unwrap_or("?"),
                v["verdict"]["rule"].as_str().unwrap_or("none"),
                s["class"].as_str().unwrap_or(""),
                s["caption"].as_str().unwrap_or(""),
                s["fullscreen"],
                s["media_playing"],
                s["input_idle_secs"],
                s["locked"],
            );
        }
        Cmd::Timeline { date } => {
            let text = text(&conn, "Timeline", &(date.unwrap_or_default(),)).await?;
            let spans: Vec<Value> = serde_json::from_str(&text)?;
            for s in spans {
                let start = s["start"].as_i64().unwrap_or(0);
                let end = s["end"].as_i64().unwrap_or_else(daemon::now);
                println!(
                    "{}  {:>4} min  {:<9} {}",
                    local(start),
                    (end - start) / 60,
                    s["state"].as_str().unwrap_or("?"),
                    s["class"].as_str().unwrap_or("")
                );
            }
        }
        Cmd::Took { id, ago, at } => {
            let taken = match (ago, at) {
                (Some(m), _) => (daemon::now() - i64::from(m) * 60).to_string(),
                (None, Some(at)) => taken_at(&at)?.to_string(),
                (None, None) => String::new(),
            };
            let text = text(&conn, "TakeDose", &(id.to_string(), taken)).await?;
            println!("logged");
            print_warnings(&text)?;
        }
        Cmd::Doses { id, days } => {
            let id = id.map(|i| i.to_string()).unwrap_or_default();
            let text = text(&conn, "Doses", &(id, days.to_string())).await?;
            print_doses(&text)?;
        }
        Cmd::RmDose { dose_id } => {
            call(&conn, "DeleteDose", &(dose_id.to_string(),)).await?;
        }
        Cmd::Daemon | Cmd::DefaultConfig => unreachable!("handled in main"),
    }
    Ok(())
}
