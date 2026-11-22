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

    function parseWhen(text) {
        const s = text.trim().toLowerCase()
        let m = s.match(/^(\d{1,2}):(\d{2})$/)
        if (m) {
            const d = new Date()
            d.setHours(Number(m[1]), Number(m[2]), 0, 0)
            if (d.getTime() > Date.now()) d.setDate(d.getDate() - 1)
            return d.getTime() / 1000
        }
        m = s.match(/^(?:(\d+)\s*h)?\s*(?:(\d+)\s*m(?:in)?)?$/)
        if (m && (m[1] || m[2])) return Date.now() / 1000 - (Number(m[1] || 0) * 60 + Number(m[2] || 0)) * 60
        m = s.match(/^(\d+)$/)
        if (m) return Date.now() / 1000 - Number(m[1]) * 60
        return null
    }

    function minGap(reminderId) {
        const r = app.medicines.find(x => x.reminder.id === reminderId)
        return r ? r.reminder.schedule.min_gap_min : 0
    }

    function warningText(w) {
        if (w.kind === "too_soon") return "Only " + app.dur(w.gap_min) + " from another dose (minimum " + app.dur(w.min_gap_min) + ")"
        if (w.kind === "over_max") return w.count + " doses in 24 hours (maximum " + w.max + ")"
        return JSON.stringify(w)
    }

    PlasmaExtras.PlaceholderMessage {
        visible: app.medicines.length === 0
        Layout.fillWidth: true
        Layout.fillHeight: true
        iconName: "medication"
        text: "No medicines yet"
        explanation: "Add one under Reminders → New, then pick \"Medicine\""
    }

    Kirigami.InlineMessage {
        Layout.fillWidth: true
        visible: app.doseWarnings.length > 0
        type: Kirigami.MessageType.Warning
        text: "Logged, but: " + app.doseWarnings.map(page.warningText).join("; ")
        actions: Kirigami.Action {
            text: "Dismiss"
            icon.name: "dialog-close"
            onTriggered: app.doseWarnings = []
        }
    }

    Repeater {
        model: app.medicines
        ColumnLayout {
            id: med
            required property var modelData
            readonly property var r: modelData.reminder
            readonly property var d: modelData.dose
            readonly property bool tooEarly: d.earliest && d.earliest > Date.now() / 1000
            readonly property bool waiting: modelData.phase && modelData.phase.phase === "deferred"
            property bool backdating: false
            Layout.fillWidth: true
            spacing: Kirigami.Units.smallSpacing

            Kirigami.Heading {
                level: 3
                text: med.r.title + (med.r.enabled ? "" : " (disabled)")
            }
            PC3.Label {
                text: "Last dose: " + (med.d.last ? app.clock(med.d.last) + " · " + app.ago(med.d.last) : "none logged yet")
            }
            PC3.Label {
                text: "Next due: " + app.clock(med.modelData.next) + (med.waiting ? " · waiting until you're back" : "")
            }
            PC3.Label {
                visible: !!med.d.earliest
                color: med.tooEarly ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.positiveTextColor
                text: med.tooEarly ? "Too soon: safe from " + app.clock(med.d.earliest) : "Safe to take now"
            }
            PC3.Label {
                opacity: 0.7
                text: med.d.in_24h + (med.d.max ? " of " + med.d.max : "") + " in the last 24 hours · " + app.describe(med.r.schedule)
            }
            RowLayout {
                PC3.Button {
                    icon.name: "dialog-ok"
                    text: "Took it now"
                    onClicked: app.takeDose(med.r.id, 0)
                }
                PC3.Button {
                    icon.name: "chronometer"
                    text: "Took it earlier…"
                    checkable: true
                    checked: med.backdating
                    onToggled: med.backdating = checked
                }
            }
            RowLayout {
                visible: med.backdating
                PC3.TextField {
                    id: when
                    Layout.fillWidth: true
                    placeholderText: "07:30, or 45m / 1h20m ago"
                    onAccepted: logEarlier.clicked()
                }
                PC3.Button {
                    id: logEarlier
                    text: "Log"
                    enabled: page.parseWhen(when.text) !== null
                    onClicked: {
                        app.takeDose(med.r.id, page.parseWhen(when.text))
                        when.text = ""
                        med.backdating = false
                    }
                }
            }
            PC3.Label {
                visible: med.backdating && when.text !== "" && page.parseWhen(when.text) !== null
                opacity: 0.7
                text: "Logs it at " + app.clock(page.parseWhen(when.text) || 0)
            }
            Kirigami.Separator { Layout.fillWidth: true }
        }
    }

    Kirigami.Heading {
        visible: app.medicines.length > 0
        level: 3
        text: "Last 7 days"
    }

    PC3.ScrollView {
        visible: app.medicines.length > 0
        Layout.fillWidth: true
        Layout.fillHeight: true
        ListView {
            clip: true
            model: app.doses
            delegate: RowLayout {
                id: row
                required property var modelData
                required property int index
                readonly property var prev: {
                    for (let i = index + 1; i < app.doses.length; i++)
                        if (app.doses[i].reminder_id === modelData.reminder_id) return app.doses[i]
                    return null
                }
                readonly property int gapMin: prev ? Math.round((modelData.taken_at - prev.taken_at) / 60) : 0
                width: ListView.view.width
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 8
                    text: app.clock(row.modelData.taken_at)
                }
                PC3.Label {
                    Layout.preferredWidth: Kirigami.Units.gridUnit * 5
                    text: row.prev ? "+" + app.dur(row.gapMin) : ""
                    color: row.prev && row.gapMin < page.minGap(row.modelData.reminder_id) ? Kirigami.Theme.negativeTextColor : Kirigami.Theme.textColor
                }
                PC3.Label {
                    Layout.fillWidth: true
                    elide: Text.ElideRight
                    text: row.modelData.title + (row.modelData.logged_at - row.modelData.taken_at > 120 ? " · backdated" : "")
                }
                PC3.ToolButton {
                    icon.name: "edit-delete"
                    display: QQC2.AbstractButton.IconOnly
                    text: "Delete this dose"
                    PC3.ToolTip.text: text
                    PC3.ToolTip.visible: hovered
                    onClicked: app.call("DeleteDose", [String(row.modelData.id)], () => app.refresh())
                }
            }
        }
    }
}
