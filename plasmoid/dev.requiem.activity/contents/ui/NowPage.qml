import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.plasma.components as PC3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami

ColumnLayout {
    required property var app
    readonly property var snap: app.current.snapshot || ({})
    spacing: Kirigami.Units.largeSpacing

    PlasmaExtras.PlaceholderMessage {
        visible: !app.online
        Layout.fillWidth: true
        Layout.fillHeight: true
        iconName: "dialog-error"
        text: "The activity daemon isn't running"
        explanation: (app.error ? app.error + "\n\n" : "") + "Start it with: systemctl --user start activity"
    }

    RowLayout {
        visible: app.online
        spacing: Kirigami.Units.largeSpacing
        Rectangle {
            Layout.preferredWidth: Kirigami.Units.iconSizes.huge
            Layout.preferredHeight: Kirigami.Units.iconSizes.huge
            color: app.stateColors[app.stateName] || Kirigami.Theme.highlightColor
            opacity: 0.85
            Kirigami.Icon {
                anchors.centerIn: parent
                width: parent.width * 0.6
                height: width
                source: app.stateIcons[app.stateName] || "appointment-soon"
                color: "white"
                isMask: true
            }
        }
        ColumnLayout {
            Layout.fillWidth: true
            Kirigami.Heading {
                text: app.stateName
                level: 1
            }
            PC3.Label {
                text: "rule: " + (app.current.verdict && app.current.verdict.rule || "none")
                opacity: 0.7
            }
            PC3.Label {
                Layout.fillWidth: true
                elide: Text.ElideRight
                text: (snap.class || "no window") + (snap.caption ? " — " + snap.caption : "")
            }
            PC3.Label {
                opacity: 0.7
                text: [snap.media_playing ? "media playing" : "", snap.fullscreen ? "fullscreen" : "",
                       snap.input_idle_secs ? "idle " + Math.round(snap.input_idle_secs / 60) + " min" : "",
                       snap.locked ? "locked" : ""].filter(s => s).join(" · ")
            }
        }
    }

    Kirigami.Heading {
        visible: app.online
        level: 3
        text: app.active.length ? "Waiting on you" : (app.nextUp ? "Next up" : "Nothing scheduled")
    }

    PC3.Label {
        visible: app.online && !app.active.length && app.nextUp
        text: app.nextUp ? app.nextUp.reminder.title + " — " + app.clock(app.nextUp.next) : ""
    }

    ListView {
        visible: app.online
        Layout.fillWidth: true
        Layout.fillHeight: true
        clip: true
        spacing: Kirigami.Units.smallSpacing
        model: app.active
        delegate: PlasmaExtras.ExpandableListItem {
            required property var modelData
            width: ListView.view.width
            icon: modelData.phase.phase === "deferred" ? "chronometer-pause" : "appointment-soon"
            title: modelData.reminder.title
            subtitle: modelData.phase.phase === "deferred"
                ? "held since " + app.clock(modelData.phase.since)
                : "pinged ×" + modelData.phase.count + " · due " + app.clock(modelData.next)
            defaultActionButtonAction: QQC2.Action {
                icon.name: "dialog-ok"
                text: "Done"
                onTriggered: app.call("Ack", [String(modelData.reminder.id)], () => app.refresh())
            }
            contextualActions: [
                QQC2.Action {
                    icon.name: "chronometer"
                    text: "Snooze 10m"
                    onTriggered: app.call("Snooze", [String(modelData.reminder.id), "10"], () => app.refresh())
                },
                QQC2.Action {
                    icon.name: "media-skip-forward"
                    text: "Skip"
                    onTriggered: app.call("Skip", [String(modelData.reminder.id)], () => app.refresh())
                }
            ]
        }
    }
}
