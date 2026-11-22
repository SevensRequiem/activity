import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.plasma.plasmoid
import org.kde.plasma.core as PlasmaCore
import org.kde.plasma.components as PC3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami
import org.kde.plasma.workspace.dbus as DBus

PlasmoidItem {
    id: root

    property var current: ({})
    property var reminders: []
    property var spans: []
    property bool online: false
    property bool editing: false
    property string error: ""

    readonly property string stateName: current.verdict ? current.verdict.state : "offline"
    readonly property var active: reminders.filter(r => r.phase && (r.phase.phase === "delivered" || r.phase.phase === "deferred"))
    readonly property var nextUp: reminders.filter(r => r.next && r.reminder.enabled).sort((a, b) => a.next - b.next)[0]

    readonly property var stateIcons: ({
        gaming: "input-gaming", watching: "video-television", focus: "document-edit",
        chatting: "dialog-messages", browsing: "internet-web-browser", drifting: "face-confused",
        idle: "player-time", away: "system-lock-screen", active: "appointment-soon", offline: "dialog-error"
    })
    readonly property var stateColors: ({
        gaming: "#ff6b9d", watching: "#b48cff", focus: "#57cbde", chatting: "#7bd88f",
        browsing: "#f2c46d", drifting: "#ff9f43", idle: "#6c7086", away: "#3b3f51", active: "#a6adc8"
    })

    function call(member, args, done) {
        const reply = DBus.SessionBus.asyncCall({
            service: "dev.requiem.Activity",
            path: "/dev/requiem/Activity",
            iface: "dev.requiem.Activity",
            member: member,
            arguments: args
        })
        reply.finished.connect(() => {
            if (reply.isError) {
                root.error = reply.error.message
                if (member === "CurrentState") root.online = false
                return
            }
            root.online = true
            root.error = ""
            if (done) done(reply.value)
        })
    }

    function refresh() {
        call("CurrentState", [], v => root.current = JSON.parse(v))
        call("ListReminders", [], v => root.reminders = JSON.parse(v))
        if (root.expanded) call("Timeline", [""], v => root.spans = JSON.parse(v))
    }

    function clock(secs) {
        if (!secs) return "—"
        const d = new Date(secs * 1000)
        const today = new Date()
        const time = d.toLocaleTimeString(Qt.locale(), "HH:mm")
        return d.toDateString() === today.toDateString() ? time : d.toLocaleDateString(Qt.locale(), "ddd d MMM") + " " + time
    }

    function describe(s) {
        const days = s.days && s.days.length ? " (" + s.days.join(", ") + ")" : ""
        const hm = t => String(t).slice(0, 5)
        if (s.kind === "once") return "once at " + String(s.at).replace("T", " ").slice(0, 16)
        if (s.kind === "daily") return "daily at " + s.times.map(hm).join(", ") + days
        return "every " + s.minutes + " min, " + hm(s.start) + "–" + hm(s.end) + days
    }

    Plasmoid.icon: stateIcons[stateName] || "appointment-soon"
    Plasmoid.status: active.length > 0 ? PlasmaCore.Types.NeedsAttentionStatus : PlasmaCore.Types.ActiveStatus
    toolTipMainText: online ? "Activity: " + stateName : "Activity daemon not running"
    toolTipSubText: nextUp ? "Next: " + nextUp.reminder.title + " at " + clock(nextUp.next) : "No upcoming reminders"

    Timer {
        interval: root.expanded ? 3000 : 20000
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: root.refresh()
    }

    onExpandedChanged: if (expanded) refresh()

    compactRepresentation: MouseArea {
        onClicked: root.expanded = !root.expanded
        Kirigami.Icon {
            anchors.fill: parent
            source: Plasmoid.icon
            active: parent.containsMouse
        }
        Rectangle {
            visible: root.active.length > 0
            anchors.right: parent.right
            anchors.top: parent.top
            width: Math.max(badge.implicitWidth + 4, height)
            height: parent.height * 0.45
            radius: height / 2
            color: "#ff6b9d"
            PC3.Label {
                id: badge
                anchors.centerIn: parent
                text: root.active.length
                font.pixelSize: parent.height * 0.75
                color: "white"
            }
        }
    }

    fullRepresentation: PlasmaExtras.Representation {
        Layout.preferredWidth: Kirigami.Units.gridUnit * 28
        Layout.preferredHeight: Kirigami.Units.gridUnit * 32
        Layout.minimumWidth: Kirigami.Units.gridUnit * 20
        Layout.minimumHeight: Kirigami.Units.gridUnit * 20
        collapseMarginsHint: true

        header: PlasmaExtras.PlasmoidHeading {
            PC3.TabBar {
                id: tabs
                anchors.fill: parent
                PC3.TabButton { text: "Now" }
                PC3.TabButton { text: "Reminders" }
                PC3.TabButton { text: "Today" }
            }
        }

        StackLayout {
            anchors.fill: parent
            anchors.margins: Kirigami.Units.smallSpacing
            currentIndex: root.editing ? 3 : tabs.currentIndex

            NowPage { app: root }
            RemindersPage {
                app: root
                onEdit: entry => { editor.load(entry); root.editing = true }
            }
            TodayPage { app: root }
            Editor {
                id: editor
                app: root
                onDone: root.editing = false
            }
        }
    }
}
