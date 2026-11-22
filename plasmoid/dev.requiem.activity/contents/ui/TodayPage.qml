import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PC3
import org.kde.kirigami as Kirigami

ColumnLayout {
    required property var app
    spacing: Kirigami.Units.largeSpacing

    readonly property real dayStart: {
        const d = new Date()
        d.setHours(0, 0, 0, 0)
        return d.getTime() / 1000
    }
    readonly property real nowSecs: Date.now() / 1000

    readonly property var totals: {
        const t = {}
        for (const s of app.spans) {
            const start = Math.max(s.start, dayStart)
            const end = s.end || nowSecs
            t[s.state] = (t[s.state] || 0) + Math.max(0, end - start)
        }
        return Object.keys(t).map(k => ({ state: k, secs: t[k] })).sort((a, b) => b.secs - a.secs)
    }

    Kirigami.Heading {
        level: 3
        text: "Today"
    }

    Item {
        Layout.fillWidth: true
        Layout.preferredHeight: Kirigami.Units.gridUnit * 2
        Rectangle {
            anchors.fill: parent
            color: Kirigami.Theme.alternateBackgroundColor
        }
        Repeater {
            model: app.spans
            Rectangle {
                required property var modelData
                readonly property real start: Math.max(modelData.start, dayStart)
                readonly property real end: modelData.end || nowSecs
                x: parent.width * (start - dayStart) / 86400
                width: Math.max(1, parent.width * (end - start) / 86400)
                height: parent.height
                color: app.stateColors[modelData.state] || "gray"
            }
        }
        Rectangle {
            x: parent.width * (nowSecs - dayStart) / 86400
            width: 2
            height: parent.height
            color: Kirigami.Theme.textColor
        }
    }

    RowLayout {
        Layout.fillWidth: true
        Repeater {
            model: ["00", "06", "12", "18", "24"]
            PC3.Label {
                required property string modelData
                Layout.fillWidth: true
                horizontalAlignment: modelData === "24" ? Text.AlignRight : (modelData === "00" ? Text.AlignLeft : Text.AlignHCenter)
                text: modelData + ":00"
                opacity: 0.6
                font: Kirigami.Theme.smallFont
            }
        }
    }

    Repeater {
        model: totals
        RowLayout {
            required property var modelData
            Layout.fillWidth: true
            Rectangle {
                Layout.preferredWidth: Kirigami.Units.gridUnit
                Layout.preferredHeight: Kirigami.Units.gridUnit
                color: app.stateColors[modelData.state] || "gray"
            }
            PC3.Label {
                Layout.fillWidth: true
                text: modelData.state
            }
            PC3.Label {
                text: Math.floor(modelData.secs / 3600) + "h " + Math.round((modelData.secs % 3600) / 60) + "m"
            }
        }
    }

    Item { Layout.fillHeight: true }
}
