# activity

Reminders for KDE Plasma 6 on Wayland that know what you're doing. A daemon watches the focused window, idle time, the screen lock and what's playing. It holds a reminder while you're gaming or watching something, repeats it while you ignore it, and pings your phone over KDE Connect when you're away. It also tracks time spent on music, video and voice calls, with daily and weekly stats.

Written in Rust. The UI is a Plasma widget that also runs as a standalone window.

## What it tracks

- **Activity state:** gaming, watching, focus, chatting, browsing, drifting (hopping between leisure apps), idle or away. This comes from the focused window, reported by a small KWin script, plus input idle time and the screen lock.
- **Media:** which MPRIS player is playing, and which site it's on. Browser tabs are matched by URL to YouTube, Spotify, Netflix, Crunchyroll and so on. Unknown sites are stored as their bare host.
- **Voice calls:** Discord clients (Vesktop, Discord, Equibop, WebCord) are seen through their PipeWire microphone stream.
- **Medicine:** each dose is due a set time after the last one you actually took, with a rolling 24 h cap.

Only start and end times, a kind and a source name are stored for media. URLs and titles are never written. Everything lives in a local SQLite database at `~/.local/share/activity/`.

## Requirements

- KDE Plasma 6 on Wayland (tested on Fedora)
- Rust 1.99+
- `pipewire-utils` for call tracking
- KDE Connect, optional, for phone pings

## Install

```
./install.sh
```

This builds the release binary into `~/.local/bin`, installs the widget and the systemd user unit, and starts the daemon. Then add the Activity widget to a panel, or launch Activity from the app menu.

## Use

```
activity add "Drink water" --every 60 --from 09:00 --to 22:00
activity add "Meds" --doses 6h --min-gap 4h
activity list
activity took 1 --ago 20m
activity state
activity timeline
```

`activity --help` lists everything. The daemon serves `io.github.sevensrequiem.Activity` on the session bus, and the CLI and the widget both talk to it there.

## Config

`~/.config/activity/config.toml`. Print the defaults with `activity default-config`. Settings include:

- classifier rules (window class and caption patterns mapped to states), idle and away thresholds
- media sources and voice apps, and the minimum length of a span worth keeping (`[media] min_secs`)
- the weekly summary notification (`[summary]`, Monday 09:00 by default)
