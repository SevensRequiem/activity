import QtQuick
import QtQuick.Layouts
import QtQuick.Controls as QQC2
import org.kde.plasma.components as PC3
import org.kde.plasma.extras as PlasmaExtras
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: page
    required property var app
    signal edit(var entry)

    RowLayout {
        Layout.fillWidth: true
        PC3.Label {
            Layout.fillWidth: true
            text: app.reminders.length + " reminders"
            opacity: 0.7
        }
        PC3.Button {
            icon.name: "list-add"
            text: "New"
            onClicked: page.edit(null)
        }
    }

    PC3.Label {
        visible: app.error !== ""
        Layout.fillWidth: true
        wrapMode: Text.Wrap
        color: Kirigami.Theme.negativeTextColor
        text: app.error
    }

    PC3.ScrollView {
        Layout.fillWidth: true
        Layout.fillHeight: true
        ListView {
            clip: true
            model: app.reminders
            spacing: Kirigami.Units.smallSpacing
            delegate: PlasmaExtras.ExpandableListItem {
                required property var modelData
                readonly property var r: modelData.reminder
                width: ListView.view.width
                icon: r.enabled ? (r.policy.hold === "never" ? "emblem-important" : "appointment-soon") : "appointment-missed"
                title: r.title
                subtitle: app.describe(r.schedule) + (r.enabled ? " · next " + app.clock(modelData.next) : " · disabled")
                defaultActionButtonAction: QQC2.Action {
                    icon.name: "document-edit"
                    text: "Edit"
                    onTriggered: page.edit(modelData)
                }
                contextualActions: [
                    QQC2.Action {
                        icon.name: r.enabled ? "media-playback-pause" : "media-playback-start"
                        text: r.enabled ? "Disable" : "Enable"
                        onTriggered: {
                            const spec = Object.assign({}, r)
                            delete spec.id
                            spec.enabled = !r.enabled
                            app.call("UpdateReminder", [String(r.id), JSON.stringify(spec)], () => app.refresh())
                        }
                    },
                    QQC2.Action {
                        icon.name: "edit-delete"
                        text: "Delete"
                        onTriggered: app.call("DeleteReminder", [String(r.id)], () => app.refresh())
                    }
                ]
            }
        }
    }
}
