import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.plasma.components as PC3
import org.kde.kirigami as Kirigami

PC3.ScrollView {
    id: editor
    required property var app
    signal done()

    property string editId: ""
    property string status: ""
    readonly property var dayKeys: ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
    readonly property var busyKeys: ["gaming", "watching", "focus", "chatting", "browsing"]
    property var days: []
    property var busy: ["gaming", "watching", "focus"]

    function toggle(list, key, on) {
        const out = list.filter(k => k !== key)
        if (on) out.push(key)
        return out
    }

    function hm(t) { return String(t).slice(0, 5) }

    function load(entry) {
        status = ""
        const r = entry ? entry.reminder : null
        const s = r ? r.schedule : { kind: "daily", times: ["09:00"], days: [] }
        const p = r ? r.policy : { hold: "when_busy", busy: ["gaming", "watching", "focus"], max_defer_min: 120,
                                   repeat_every_min: 10, max_repeats: 3, escalate_after: 3, ring_phone: false }
        editId = r ? String(r.id) : ""
        title.text = r ? r.title : ""
        body.text = r ? r.body : ""
        kind.currentIndex = ["daily", "every", "once"].indexOf(s.kind)
        times.text = s.kind === "daily" ? s.times.map(hm).join(", ") : "09:00"
        every.value = s.kind === "every" ? s.minutes : 60
        from.text = s.kind === "every" ? hm(s.start) : "09:00"
        to.text = s.kind === "every" ? hm(s.end) : "22:00"
        at.text = s.kind === "once" ? String(s.at).slice(0, 16) : Qt.formatDateTime(new Date(Date.now() + 3600000), "yyyy-MM-dd'T'HH:00")
        days = s.days || []
        hold.currentIndex = p.hold === "never" ? 1 : 0
        busy = p.busy
        maxDefer.value = p.max_defer_min
        repeat.value = p.repeat_every_min
        maxRepeats.value = p.max_repeats
        escalate.value = p.escalate_after
        ring.checked = p.ring_phone
        enabled.checked = r ? r.enabled : true
    }

    function spec() {
        const k = ["daily", "every", "once"][kind.currentIndex]
        let schedule
        if (k === "daily") schedule = { kind: k, times: times.text.split(",").map(t => t.trim()).filter(t => t), days: days }
        else if (k === "every") schedule = { kind: k, minutes: every.value, start: from.text.trim(), end: to.text.trim(), days: days }
        else schedule = { kind: k, at: at.text.trim() }
        return {
            title: title.text, body: body.text, schedule: schedule, enabled: enabled.checked,
            policy: {
                hold: hold.currentIndex === 1 ? "never" : "when_busy", busy: busy,
                max_defer_min: maxDefer.value, repeat_every_min: repeat.value,
                max_repeats: maxRepeats.value, escalate_after: escalate.value, ring_phone: ring.checked
            }
        }
    }

    function save() {
        const json = JSON.stringify(spec())
        const finish = () => { app.refresh(); done() }
        if (editId) app.call("UpdateReminder", [editId, json], finish)
        else app.call("AddReminder", [json], finish)
        status = "saving…"
    }

    Connections {
        target: app
        function onErrorChanged() { if (app.error) editor.status = app.error }
    }

    Kirigami.FormLayout {
        width: editor.availableWidth

        PC3.TextField { id: title; Kirigami.FormData.label: "Title:"; placeholderText: "Drink water" }
        PC3.TextField { id: body; Kirigami.FormData.label: "Details:"; placeholderText: "optional" }

        Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: "When" }

        PC3.ComboBox { id: kind; Kirigami.FormData.label: "Repeats:"; model: ["Daily at times", "Every N minutes", "Once"] }
        PC3.TextField {
            id: times
            visible: kind.currentIndex === 0
            Kirigami.FormData.label: "Times:"
            placeholderText: "09:00, 14:30"
        }
        PC3.SpinBox { id: every; visible: kind.currentIndex === 1; Kirigami.FormData.label: "Every (min):"; from: 1; to: 1440 }
        RowLayout {
            visible: kind.currentIndex === 1
            Kirigami.FormData.label: "Between:"
            PC3.TextField { id: from; Layout.preferredWidth: Kirigami.Units.gridUnit * 4 }
            PC3.Label { text: "and" }
            PC3.TextField { id: to; Layout.preferredWidth: Kirigami.Units.gridUnit * 4 }
        }
        PC3.TextField {
            id: at
            visible: kind.currentIndex === 2
            Kirigami.FormData.label: "At:"
            placeholderText: "2026-11-23T15:00"
        }
        Flow {
            visible: kind.currentIndex !== 2
            Kirigami.FormData.label: "Days:"
            Layout.fillWidth: true
            Repeater {
                model: editor.dayKeys
                PC3.CheckBox {
                    required property string modelData
                    text: modelData
                    checked: editor.days.includes(modelData)
                    onToggled: editor.days = editor.toggle(editor.days, modelData, checked)
                }
            }
        }
        PC3.Label {
            visible: kind.currentIndex !== 2
            text: "No days ticked means every day."
            opacity: 0.6
            font: Kirigami.Theme.smallFont
        }

        Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: "When I'm busy" }

        PC3.ComboBox { id: hold; Kirigami.FormData.label: "Behavior:"; model: ["Hold until I'm free", "Never hold back"] }
        Flow {
            visible: hold.currentIndex === 0
            Kirigami.FormData.label: "Busy means:"
            Layout.fillWidth: true
            Repeater {
                model: editor.busyKeys
                PC3.CheckBox {
                    required property string modelData
                    text: modelData
                    checked: editor.busy.includes(modelData)
                    onToggled: editor.busy = editor.toggle(editor.busy, modelData, checked)
                }
            }
        }
        PC3.SpinBox { id: maxDefer; visible: hold.currentIndex === 0; Kirigami.FormData.label: "Hold at most (min):"; from: 0; to: 1440 }

        Kirigami.Separator { Kirigami.FormData.isSection: true; Kirigami.FormData.label: "If I ignore it" }

        PC3.SpinBox { id: repeat; Kirigami.FormData.label: "Ping again every (min):"; from: 0; to: 1440 }
        PC3.SpinBox { id: maxRepeats; Kirigami.FormData.label: "Extra pings:"; from: 0; to: 50 }
        PC3.SpinBox { id: escalate; Kirigami.FormData.label: "Phone from ping #:"; from: 0; to: 51 }
        PC3.CheckBox { id: ring; Kirigami.FormData.label: "Last resort:"; text: "Ring my phone" }
        PC3.CheckBox { id: enabled; Kirigami.FormData.label: "Status:"; text: "Enabled" }

        PC3.Label {
            visible: editor.status !== ""
            Layout.fillWidth: true
            wrapMode: Text.Wrap
            text: editor.status
            color: editor.status === "saving…" ? Kirigami.Theme.textColor : Kirigami.Theme.negativeTextColor
        }

        RowLayout {
            PC3.Button { text: "Save"; icon.name: "document-save"; enabled: title.text.trim() !== ""; onClicked: editor.save() }
            PC3.Button { text: "Cancel"; icon.name: "dialog-cancel"; onClicked: editor.done() }
        }
    }
}
