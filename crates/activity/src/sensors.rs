use std::collections::HashMap;
use std::io::BufReader;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use activity_core::media::{Key, Sources, Streams, Voice};
use anyhow::{Context, Result};
use async_channel::Sender;
use futures_lite::StreamExt;
use serde_json::Value;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection as WlConnection, Dispatch, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1, ext_idle_notifier_v1,
};
use zbus::zvariant::OwnedValue;

use crate::daemon::Event;

/// Idle notifications fire after this much inactivity; the daemon backdates the
/// idle start by it, so it only needs to be below the configured idle threshold.
pub const IDLE_PROBE_SECS: u32 = 60;
const MAX_URL: usize = 2048;

const KWIN_SCRIPT: &str = include_str!("../../../kwin/activity-tracker.js");
const KWIN_PLUGIN: &str = "dev.requiem.activity-tracker";

/// `KWin` is the only thing on Wayland that knows the active window, so we load
/// a script into it that reports focus and caption changes back over D-Bus.
pub async fn load_kwin_script(conn: &zbus::Connection) -> Result<()> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let path = std::path::Path::new(&dir).join("activity-tracker.js");
    std::fs::write(&path, KWIN_SCRIPT).with_context(|| format!("writing {}", path.display()))?;
    let path = path.to_str().context("runtime dir is not UTF-8")?;
    unload_kwin_script(conn).await;
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(path, KWIN_PLUGIN),
    )
    .await
    .context("loading KWin script")?;
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "start",
        &(),
    )
    .await
    .context("starting KWin scripts")?;
    Ok(())
}

pub async fn unload_kwin_script(conn: &zbus::Connection) {
    // Fails harmlessly when nothing is loaded.
    let _ = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "unloadScript",
            &(KWIN_PLUGIN,),
        )
        .await;
}

#[zbus::proxy(
    interface = "org.freedesktop.ScreenSaver",
    default_service = "org.freedesktop.ScreenSaver",
    default_path = "/ScreenSaver"
)]
trait ScreenSaver {
    fn get_active(&self) -> zbus::Result<bool>;
    #[zbus(signal)]
    fn active_changed(&self, active: bool) -> zbus::Result<()>;
}

pub async fn watch_lock(conn: zbus::Connection, tx: Sender<Event>) -> Result<()> {
    let proxy = ScreenSaverProxy::new(&conn).await?;
    let mut changes = proxy.receive_active_changed().await?;
    if let Ok(active) = proxy.get_active().await {
        tx.send(Event::Locked(active)).await?;
    }
    while let Some(signal) = changes.next().await {
        tx.send(Event::Locked(signal.args()?.active)).await?;
    }
    Ok(())
}

#[zbus::proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_path = "/org/mpris/MediaPlayer2"
)]
trait Player {
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn metadata(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

/// Polls rather than subscribes: players come and go (each Firefox tab with
/// media can register one), and a 5 s delay is fine for both reminders and
/// usage stats. The URL only picks the site and never leaves this function.
pub async fn watch_media(
    conn: zbus::Connection,
    tx: Sender<Event>,
    sources: Sources,
) -> Result<()> {
    let bus = zbus::fdo::DBusProxy::new(&conn).await?;
    let mut last = None;
    loop {
        let mut playing: Vec<Key> = Vec::new();
        for name in bus.list_names().await? {
            let Some(suffix) = name.strip_prefix("org.mpris.MediaPlayer2.") else {
                continue;
            };
            let Ok(builder) = PlayerProxy::builder(&conn).destination(name.to_string()) else {
                continue;
            };
            let Ok(player) = builder
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build()
                .await
            else {
                continue;
            };
            if !player.playback_status().await.is_ok_and(|s| s == "Playing") {
                continue;
            }
            let url: Option<String> = player.metadata().await.ok().and_then(|m| {
                let url = String::try_from(&**m.get("xesam:url")?).ok()?;
                Some(url.chars().take(MAX_URL).collect())
            });
            // "firefox.instance_1_74" names the same player as "firefox".
            let player: Vec<&str> = suffix
                .split('.')
                .filter(|s| !s.starts_with("instance"))
                .collect();
            playing.push(sources.classify(&player.join("."), url.as_deref()));
        }
        playing.sort();
        playing.dedup();
        if last.as_ref() != Some(&playing) {
            tx.send(Event::Media(playing.clone())).await?;
            last = Some(playing);
        }
        async_io::Timer::after(Duration::from_secs(5)).await;
    }
}

/// Kills `pw-dump` when the watcher returns or fails. At process exit the
/// unit's cgroup takes it down instead.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Calls are seen through `PipeWire`: the voice app runs as its own app user
/// with a private D-Bus, but shares this user's `PipeWire` socket, so its
/// microphone stream shows up here. Blocking; runs on its own thread.
pub fn run_voice(tx: &Sender<Event>, voice: &Voice) {
    let mut backoff = 1;
    loop {
        let started = Instant::now();
        let Err(e) = watch_pipewire(tx, voice) else {
            return;
        };
        if e.downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            tracing::warn!("pw-dump not found (install pipewire-utils); calls won't be tracked");
            return;
        }
        if tx.is_closed() {
            return;
        }
        tracing::warn!("call sensor: {e:#}");
        if tx.send_blocking(Event::Voice(None)).is_err() {
            return;
        }
        backoff = if started.elapsed() > Duration::from_secs(60) {
            1
        } else {
            (backoff * 2).min(60)
        };
        std::thread::sleep(Duration::from_secs(backoff));
    }
}

/// Returns Ok only once the daemon has gone away.
fn watch_pipewire(tx: &Sender<Event>, voice: &Voice) -> Result<()> {
    let mut child = Reaper(
        Command::new("pw-dump")
            .args(["--monitor", "--no-colors"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .context("starting pw-dump")?,
    );
    let out = child.0.stdout.take().context("pw-dump has no stdout")?;
    let mut streams = Streams::default();
    let mut last = None;
    for batch in
        serde_json::Deserializer::from_reader(BufReader::new(out)).into_iter::<Vec<Value>>()
    {
        for obj in batch.context("reading pw-dump output")? {
            streams.apply(&obj);
        }
        let call = streams.call(voice).map(str::to_owned);
        if last.as_ref() != Some(&call) {
            if tx.send_blocking(Event::Voice(call.clone())).is_err() {
                return Ok(());
            }
            last = Some(call);
        }
    }
    anyhow::bail!("pw-dump exited")
}

struct Idle {
    tx: Sender<Event>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Idle {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &WlConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Idle {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        (): &(),
        _: &WlConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_idle_notifier_v1::ExtIdleNotifierV1, ()> for Idle {
    fn event(
        _: &mut Self,
        _: &ext_idle_notifier_v1::ExtIdleNotifierV1,
        _: ext_idle_notifier_v1::Event,
        (): &(),
        _: &WlConnection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ext_idle_notification_v1::ExtIdleNotificationV1, ()> for Idle {
    fn event(
        state: &mut Self,
        _: &ext_idle_notification_v1::ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        (): &(),
        _: &WlConnection,
        _: &QueueHandle<Self>,
    ) {
        let idle = match event {
            ext_idle_notification_v1::Event::Idled => true,
            ext_idle_notification_v1::Event::Resumed => false,
            _ => return,
        };
        // The daemon is gone if this fails; the thread ends on the next dispatch error.
        let _ = state.tx.send_blocking(Event::Idle(idle));
    }
}

/// Blocking Wayland loop for its own thread. Uses the input-only idle
/// notification (protocol v2) so a playing video's idle inhibitor doesn't
/// hide that nobody is touching the keyboard.
pub fn run_idle(tx: Sender<Event>) -> Result<()> {
    let conn = WlConnection::connect_to_env().context("connecting to the Wayland compositor")?;
    let (globals, mut queue) = registry_queue_init::<Idle>(&conn)?;
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("binding wl_seat")?;
    let notifier: ext_idle_notifier_v1::ExtIdleNotifierV1 = globals
        .bind(&qh, 2..=2, ())
        .context("compositor lacks ext_idle_notifier_v1 v2")?;
    let _notification =
        notifier.get_input_idle_notification(IDLE_PROBE_SECS * 1000, &seat, &qh, ());
    let mut state = Idle { tx };
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}
