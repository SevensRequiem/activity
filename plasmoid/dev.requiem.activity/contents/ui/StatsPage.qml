import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.plasma.components as PC3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: page
    required property var app
    spacing: Kirigami.Units.largeSpacing

    readonly property var s: app.stats
    readonly property var kinds: ["music", "video", "podcast", "call", "other"]
    readonly property var stateKeys: ["gaming", "watching", "focus", "chatting", "browsing", "drifting", "active"]

    readonly property var totals: {
        if (!s) return []
        const rows = []
        for (const k of kinds) rows.push({ key: k, color: app.kindColors[k], secs: s.media.total[k] || 0, prev: s.media.prev[k] || 0 })
        for (const k of stateKeys) rows.push({ key: k, color: app.stateColors[k], secs: s.states.total[k] || 0, prev: s.states.prev[k] || 0 })
        return rows.filter(r => r.secs || r.prev)
    }

    function day(i) {
        return new Date(s.dates[i] + "T12:00").toLocaleDateString(Qt.locale(), "ddd d MMM")
    }

    function change(r) {
        const d = r.secs - r.prev
        if (Math.abs(d) < 60) return "same"
        return (d > 0 ? "+" : "−") + app.hm(Math.abs(d))
    }

    onVisibleChanged: if (visible) app.fetchStats()

    component Bars: ColumnLayout {
        id: bars
        property string title
        property var columns: []
        property var keys: []
        property var colors: ({})
        property var labelOf
        property var fmt
        property var ticks: []
        readonly property real peak: Math.max(1, ...columns.map(c => keys.reduce((a, k) => a + (c[k] || 0), 0)))
        Layout.fillWidth: true
        spacing: 2
        PC3.Label {
            text: bars.title
            opacity: 0.7
            font: Kirigami.Theme.smallFont
        }
        Item {
            id: plot
            Layout.fillWidth: true
            Layout.preferredHeight: Kirigami.Units.gridUnit * 4
            Rectangle {
                anchors.fill: parent
                color: Kirigami.Theme.alternateBackgroundColor
            }
            Repeater {
                model: bars.columns
                Item {
                    id: column
                    required property var modelData
                    required property int index
                    readonly property var parts: bars.keys.filter(k => modelData[k]).map(k => ({ key: k, secs: modelData[k] }))
                    x: plot.width * index / bars.columns.length
                    width: Math.max(1, plot.width / bars.columns.length - 1)
                    height: plot.height
                    Column {
                        anchors.bottom: parent.bottom
                        width: parent.width
                        Repeater {
                            model: column.parts.slice().reverse()
                            Rectangle {
                                required property var modelData
                                width: parent.width
                                height: plot.height * modelData.secs / bars.peak
                                color: bars.colors[modelData.key] || "gray"
                            }
                        }
                    }
                    MouseArea {
                        id: hover
                        anchors.fill: parent
                        hoverEnabled: true
                    }
                    QQC2.ToolTip.visible: hover.containsMouse && parts.length > 0
                    QQC2.ToolTip.text: bars.labelOf(index) + "\n" + parts.map(p => p.key + "  " + bars.fmt(p.secs)).join("\n")
                }
            }
        }
        RowLayout {
            Layout.fillWidth: true
            Repeater {
                model: bars.ticks
                PC3.Label {
                    required property string modelData
                    required property int index
                    Layout.fillWidth: true
                    horizontalAlignment: index === 0 ? Text.AlignLeft : (index === bars.ticks.length - 1 ? Text.AlignRight : Text.AlignHCenter)
                    text: modelData
                    opacity: 0.6
                    font: Kirigami.Theme.smallFont
                }
            }
        }
    }

    component TopList: ColumnLayout {
        id: list
        property string title
        property var items: []
        property var colorOf
        property var fmt
        readonly property real peak: Math.max(1, ...items.map(i => i.secs))
        Layout.fillWidth: true
        visible: items.length > 0
        Kirigami.Heading {
            level: 4
            text: list.title
        }
        Repeater {
            model: list.items
            RowLayout {
                required property var modelData
                Layout.fillWidth: true
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 9
                    elide: Text.ElideRight
                    textFormat: Text.PlainText
                    text: modelData.name
                }
                Item {
                    Layout.fillWidth: true
                    Layout.preferredHeight: Kirigami.Units.gridUnit * 0.6
                    Rectangle {
                        width: parent.width * modelData.secs / list.peak
                        height: parent.height
                        color: list.colorOf(modelData)
                    }
                }
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 4
                    horizontalAlignment: Text.AlignRight
                    text: list.fmt(modelData.secs)
                }
            }
        }
    }

    RowLayout {
        Layout.fillWidth: true
        Kirigami.Heading {
            Layout.fillWidth: true
            level: 3
            text: "Last " + app.statsDays + " days"
        }
        Repeater {
            model: [7, 30, 90]
            PC3.ToolButton {
                required property int modelData
                text: modelData + "d"
                checkable: true
                autoExclusive: true
                checked: app.statsDays === modelData
                onClicked: {
                    app.statsDays = modelData
                    app.fetchStats()
                }
            }
        }
    }

    PlasmaExtras.PlaceholderMessage {
        visible: totals.length === 0
        Layout.fillWidth: true
        Layout.fillHeight: true
        iconName: "view-statistics"
        text: s ? "Nothing tracked in this period" : "Loading…"
    }

    PC3.ScrollView {
        id: scroll
        visible: totals.length > 0
        Layout.fillWidth: true
        Layout.fillHeight: true
        QQC2.ScrollBar.horizontal.policy: QQC2.ScrollBar.AlwaysOff

        ColumnLayout {
            width: scroll.availableWidth
            spacing: Kirigami.Units.largeSpacing

            Repeater {
                model: totals
                RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    Rectangle {
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 0.75
                        Layout.preferredHeight: Kirigami.Units.gridUnit * 0.75
                        color: modelData.color || "gray"
                    }
                    PC3.Label {
                        Layout.fillWidth: true
                        text: modelData.key
                    }
                    PC3.Label {
                        text: app.hm(modelData.secs)
                    }
                    PC3.Label {
                        Layout.preferredWidth: Kirigami.Units.gridUnit * 4
                        horizontalAlignment: Text.AlignRight
                        opacity: 0.7
                        text: page.change(modelData)
                    }
                }
            }

            PC3.Label {
                Layout.fillWidth: true
                opacity: 0.7
                font: Kirigami.Theme.smallFont
                wrapMode: Text.Wrap
                text: "Change is against the " + app.statsDays + " days before."
            }

            PC3.Label {
                visible: s && s.calls.count > 0
                Layout.fillWidth: true
                wrapMode: Text.Wrap
                text: s ? s.calls.count + (s.calls.count === 1 ? " call" : " calls") + " · " + app.hm(s.calls.total)
                          + " total · longest " + app.hm(s.calls.longest) + " · average " + app.hm(s.calls.average) : ""
            }

            Bars {
                title: "Listening, watching and calls per day"
                columns: s ? s.media.days : []
                keys: page.kinds
                colors: app.kindColors
                labelOf: page.day
                fmt: app.hm
                ticks: s ? [page.day(0), page.day(s.dates.length - 1)] : []
            }

            Bars {
                title: "Activity per day"
                columns: s ? s.states.days : []
                keys: page.stateKeys
                colors: app.stateColors
                labelOf: page.day
                fmt: app.hm
                ticks: s ? [page.day(0), page.day(s.dates.length - 1)] : []
            }

            Bars {
                title: "Listening, watching and calls by hour of day"
                columns: s ? s.media.hours : []
                keys: page.kinds
                colors: app.kindColors
                labelOf: i => String(i).padStart(2, "0") + ":00"
                fmt: app.hm
                ticks: ["00", "06", "12", "18", "24"]
            }

            Bars {
                title: "Activity by hour of day"
                columns: s ? s.states.hours : []
                keys: page.stateKeys
                colors: app.stateColors
                labelOf: i => String(i).padStart(2, "0") + ":00"
                fmt: app.hm
                ticks: ["00", "06", "12", "18", "24"]
            }

            TopList {
                title: "Top sites and players"
                items: s ? s.sources : []
                colorOf: t => app.kindColors[t.kind] || "gray"
                fmt: app.hm
            }

            TopList {
                title: "Top games"
                items: s ? s.games : []
                colorOf: t => app.stateColors.gaming
                fmt: app.hm
            }
        }
    }
}
