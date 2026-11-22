use std::collections::HashMap;

use anyhow::{Context, Result};
use async_channel::Sender;
use futures_lite::StreamExt;
use zbus::zvariant::Value;

use crate::daemon::Event;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[expect(
        clippy::too_many_arguments,
        reason = "mirrors the freedesktop Notify signature"
    )]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    fn close_notification(&self, id: u32) -> zbus::Result<()>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;
}

pub const ACTION_DONE: &str = "done";
pub const ACTION_SNOOZE: &str = "snooze";
pub const ACTION_SKIP: &str = "skip";
pub const SNOOZE_MIN: u32 = 10;

pub async fn desktop(
    proxy: &NotificationsProxy<'_>,
    replaces: Option<u32>,
    title: &str,
    body: &str,
    count: u32,
    urgent: bool,
) -> Result<u32> {
    let summary = if count > 1 {
        format!("{title} (again ×{count})")
    } else {
        title.to_owned()
    };
    let hints = HashMap::from([
        ("urgency", Value::U8(if urgent { 2 } else { 1 })),
        (
            "desktop-entry",
            Value::from("io.github.sevensrequiem.activity"),
        ),
    ]);
    let actions = [
        ACTION_DONE,
        "Done",
        ACTION_SNOOZE,
        "Snooze 10m",
        ACTION_SKIP,
        "Skip",
    ];
    // Timeout 0 keeps it on screen until acted on; forward-continue handles the rest.
    proxy
        .notify(
            "Activity",
            replaces.unwrap_or(0),
            "appointment-soon",
            &summary,
            body,
            &actions,
            hints,
            0,
        )
        .await
        .context("sending desktop notification")
}

/// Information only: no actions, and it expires like any other notification.
pub async fn summary(proxy: &NotificationsProxy<'_>, title: &str, body: &str) -> Result<u32> {
    let hints = HashMap::from([(
        "desktop-entry",
        Value::from("io.github.sevensrequiem.activity"),
    )]);
    proxy
        .notify(
            "Activity",
            0,
            "view-statistics",
            title,
            body,
            &[],
            hints,
            -1,
        )
        .await
        .context("sending summary notification")
}

pub async fn watch_actions(conn: zbus::Connection, tx: Sender<Event>) -> Result<()> {
    let proxy = NotificationsProxy::new(&conn).await?;
    let mut actions = proxy.receive_action_invoked().await?;
    while let Some(signal) = actions.next().await {
        let args = signal.args()?;
        tx.send(Event::Action {
            notification: args.id,
            key: args.action_key,
        })
        .await?;
    }
    Ok(())
}

const KDECONNECT: &str = "org.kde.kdeconnect";

async fn reachable(conn: &zbus::Connection, id: &str) -> bool {
    let path = format!("/modules/kdeconnect/devices/{id}");
    let Ok(props) = zbus::fdo::PropertiesProxy::builder(conn)
        .destination(KDECONNECT)
        .and_then(|b| b.path(path))
    else {
        return false;
    };
    let Ok(props) = props.build().await else {
        return false;
    };
    let Ok(iface) = zbus::names::InterfaceName::try_from("org.kde.kdeconnect.device") else {
        return false;
    };
    props
        .get(iface, "isReachable")
        .await
        .ok()
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

/// The configured device if it's reachable, else the first reachable paired one.
async fn phone(conn: &zbus::Connection, configured: Option<&str>) -> Option<String> {
    if let Some(id) = configured {
        return reachable(conn, id).await.then(|| id.to_owned());
    }
    let reply = conn
        .call_method(
            Some(KDECONNECT),
            "/modules/kdeconnect",
            Some("org.kde.kdeconnect.daemon"),
            "devices",
            &(true, true),
        )
        .await
        .ok()?;
    let ids: Vec<String> = reply.body().deserialize().ok()?;
    ids.into_iter().next()
}

/// Returns whether a phone was reached, so the caller can log a miss.
pub async fn to_phone(
    conn: &zbus::Connection,
    configured: Option<&str>,
    message: &str,
    ring: bool,
) -> Result<bool> {
    let Some(id) = phone(conn, configured).await else {
        return Ok(false);
    };
    let base = format!("/modules/kdeconnect/devices/{id}");
    conn.call_method(
        Some(KDECONNECT),
        format!("{base}/ping").as_str(),
        Some("org.kde.kdeconnect.device.ping"),
        "sendPing",
        &(message,),
    )
    .await
    .context("KDE Connect ping")?;
    if ring {
        conn.call_method(
            Some(KDECONNECT),
            format!("{base}/findmyphone").as_str(),
            Some("org.kde.kdeconnect.device.findmyphone"),
            "ring",
            &(),
        )
        .await
        .context("KDE Connect ring")?;
    }
    Ok(true)
}
