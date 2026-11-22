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
    /// Whether to hold the reminder while you're gaming, watching or focused.
    #[arg(long, value_enum, default_value = "busy")]
    hold: HoldArg,
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
        let schedule = match (self.at, self.daily, self.every) {
            (Some(at), None, None) => json!({ "kind": "once", "at": at }),
            (None, Some(times), None) => {
                json!({ "kind": "daily", "times": split(&times), "days": days })
            }
            (None, None, Some(minutes)) => {
                json!({ "kind": "every", "minutes": minutes, "start": self.from, "end": self.to, "days": days })
            }
            _ => bail!("give exactly one of --at, --daily or --every"),
        };
        Ok(json!({
            "title": self.title,
            "body": self.body,
            "schedule": schedule,
            "policy": {
                "hold": match self.hold { HoldArg::Never => "never", HoldArg::Busy => "when_busy" },
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
    /// Add a reminder. Give exactly one of --at, --daily or --every.
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
        Cmd::List => {
            let text: String = call(&conn, "ListReminders", &())
                .await?
                .body()
                .deserialize()?;
            let list: Vec<Value> = serde_json::from_str(&text)?;
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
            }
        }
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
            let text: String = call(&conn, "CurrentState", &())
                .await?
                .body()
                .deserialize()?;
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
            let text: String = call(&conn, "Timeline", &(date.unwrap_or_default(),))
                .await?
                .body()
                .deserialize()?;
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
        Cmd::Daemon | Cmd::DefaultConfig => unreachable!("handled in main"),
    }
    Ok(())
}
