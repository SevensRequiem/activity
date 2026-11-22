import QtQuick
import QtQuick.Layouts
import org.kde.plasma.components as PC3
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: page
    required property var app
    spacing: Kirigami.Units.largeSpacing

    readonly property real dayStart: {
        const d = new Date(app.now * 1000)
        d.setHours(0, 0, 0, 0)
        return d.getTime() / 1000
    }
    readonly property real nowSecs: app.now
    readonly property real labelWidth: Kirigami.Units.gridUnit * 3.5
    readonly property var media: app.mediaSpans.filter(s => s.kind !== "call")
    readonly property var calls: app.mediaSpans.filter(s => s.kind === "call")

    readonly property var totals: {
        const t = {}
        for (const s of app.spans) {
            const start = Math.max(s.start, dayStart)
            const end = s.end || nowSecs
            t[s.state] = (t[s.state] || 0) + Math.max(0, end - start)
        }
        return Object.keys(t).map(k => ({ state: k, secs: t[k] })).sort((a, b) => b.secs - a.secs)
    }

    function clipped(s) {
        return Math.min(s.end || nowSecs, dayStart + 86400) - Math.max(s.start, dayStart)
    }

    component Lane: RowLayout {
        id: lane
        property string label
        property var spans: []
        property var colorOf
        property real dayStart
        property real nowSecs
        property real labelWidth
        property real barHeight
        Layout.fillWidth: true
        PC3.Label {
            Layout.preferredWidth: lane.labelWidth
            text: lane.label
            opacity: 0.7
            font: Kirigami.Theme.smallFont
        }
        Item {
            id: track
            Layout.fillWidth: true
            Layout.preferredHeight: lane.barHeight
            Rectangle {
                anchors.fill: parent
                color: Kirigami.Theme.alternateBackgroundColor
            }
            Repeater {
                model: lane.spans
                Rectangle {
                    required property var modelData
                    readonly property real start: Math.max(modelData.start, lane.dayStart)
                    readonly property real end: Math.min(modelData.end || lane.nowSecs, lane.dayStart + 86400)
                    x: track.width * (start - lane.dayStart) / 86400
                    width: Math.max(1, track.width * (end - start) / 86400)
                    height: track.height
                    color: lane.colorOf(modelData)
                }
            }
            Rectangle {
                x: track.width * (lane.nowSecs - lane.dayStart) / 86400
                width: 2
                height: track.height
                color: Kirigami.Theme.textColor
            }
        }
    }

    Kirigami.Heading {
        level: 3
        text: "Today"
    }

    ColumnLayout {
        Layout.fillWidth: true
        spacing: 2
        Lane {
            label: "Activity"
            spans: app.spans
            colorOf: s => app.stateColors[s.state] || "gray"
            dayStart: page.dayStart
            nowSecs: page.nowSecs
            labelWidth: page.labelWidth
            barHeight: Kirigami.Units.gridUnit * 2
        }
        Lane {
            label: "Media"
            spans: page.media
            colorOf: s => app.kindColors[s.kind] || "gray"
            dayStart: page.dayStart
            nowSecs: page.nowSecs
            labelWidth: page.labelWidth
            barHeight: Kirigami.Units.gridUnit
        }
        Lane {
            label: "Calls"
            spans: page.calls
            colorOf: s => app.kindColors.call
            dayStart: page.dayStart
            nowSecs: page.nowSecs
            labelWidth: page.labelWidth
            barHeight: Kirigami.Units.gridUnit
        }
        RowLayout {
            Layout.fillWidth: true
            Item { Layout.preferredWidth: page.labelWidth }
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
    }

    Flow {
        Layout.fillWidth: true
        spacing: Kirigami.Units.largeSpacing
        Repeater {
            model: totals
            RowLayout {
                required property var modelData
                Rectangle {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 0.75
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 0.75
                    color: app.stateColors[modelData.state] || "gray"
                }
                PC3.Label {
                    text: modelData.state + " " + app.hm(modelData.secs)
                }
            }
        }
    }

    Kirigami.Heading {
        level: 4
        text: app.mediaSpans.length ? "Listening, watching and calls" : "Nothing played today"
    }

    PC3.ScrollView {
        Layout.fillWidth: true
        Layout.fillHeight: true
        ListView {
            clip: true
            model: app.mediaSpans.slice().reverse()
            delegate: RowLayout {
                id: row
                required property var modelData
                width: ListView.view.width
                Rectangle {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 0.5
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 0.75
                    color: app.kindColors[row.modelData.kind] || "gray"
                }
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 6
                    text: new Date(row.modelData.start * 1000).toLocaleTimeString(Qt.locale(), "HH:mm") + "–"
                          + new Date(row.modelData.end * 1000).toLocaleTimeString(Qt.locale(), "HH:mm")
                }
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 4
                    text: app.hm(page.clipped(row.modelData))
                }
                PC3.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                    text: row.modelData.source
                }
                PC3.Label {
                    opacity: 0.7
                    text: row.modelData.kind
                }
            }
        }
    }
}
