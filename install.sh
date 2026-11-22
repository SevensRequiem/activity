#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --locked
install -Dm755 target/release/activity "$HOME/.local/bin/activity"
install -Dm644 dist/activity.service "$HOME/.config/systemd/user/activity.service"
install -Dm644 dist/dev.requiem.activity.desktop "$HOME/.local/share/applications/dev.requiem.activity.desktop"

if kpackagetool6 -t Plasma/Applet -s dev.requiem.activity >/dev/null 2>&1; then
  kpackagetool6 -t Plasma/Applet -u plasmoid/dev.requiem.activity
else
  kpackagetool6 -t Plasma/Applet -i plasmoid/dev.requiem.activity
fi

systemctl --user daemon-reload
systemctl --user enable activity.service
systemctl --user restart activity.service
echo "installed: add the Activity widget to your panel or system tray, or launch Activity from the menu"
