/*
 * Cirrust — Plasma 6 sync-status widget.
 *
 * Talks to the running app's session D-Bus service
 * (org.cirrust.client, object /Sync, interface org.cirrust.client.Sync)
 * via `gdbus` run through a Plasma executable DataSource. It polls Status()
 * every few seconds and calls SyncNow()/Open() on the buttons.
 */
import QtQuick
import QtQuick.Layouts
import org.kde.plasma.plasmoid
import org.kde.plasma.components as PlasmaComponents
import org.kde.kirigami as Kirigami
import org.kde.plasma.plasma5support as Plasma5Support

PlasmoidItem {
    id: root

    // ---- Live state (from D-Bus) ---------------------------------------------
    property string syncState: "offline"   // idle | syncing | paused | error | offline
    property string activeFolder: ""
    property int folderCount: 0
    property string lastSync: ""
    property bool available: false          // is the app / service running?

    readonly property string dbusBase:
        "gdbus call --session --dest org.cirrust.client.Daemon " +
        "--object-path /Sync --method org.cirrust.client.Sync."
    readonly property string statusCmd: dbusBase + "Status"
    readonly property string syncCmd: dbusBase + "SyncNow"
    readonly property string openCmd: dbusBase + "Open"

    readonly property var stateIcons: ({
        "idle": "vcs-normal",
        "syncing": "view-refresh",
        "paused": "media-playback-pause",
        "error": "vcs-conflicting",
        "offline": "network-disconnect"
    })

    function iconFor(state) {
        return stateIcons[state] || "cloud"
    }

    function stateLabel() {
        if (!root.available)
            return "Not running"
        if (root.syncState === "syncing" && root.activeFolder !== "")
            return "Syncing " + root.activeFolder
        return root.syncState.charAt(0).toUpperCase() + root.syncState.slice(1)
    }

    // ---- D-Bus bridge (gdbus via executable engine) --------------------------
    Plasma5Support.DataSource {
        id: executable
        engine: "executable"
        connectedSources: []

        onNewData: (source, data) => {
            executable.disconnectSource(source)
            if (source === root.statusCmd) {
                root.parseStatus(data["stdout"] || "")
            } else if (source === root.syncCmd) {
                root.refresh() // reflect the new "syncing" state promptly
            }
        }

        function run(cmd) {
            executable.connectSource(cmd)
        }
    }

    function refresh() {
        executable.run(root.statusCmd)
    }

    // Parse gdbus output like:  ('idle', '', uint32 2, '2026-07-05T12:00:00+00:00')
    function parseStatus(out) {
        out = (out || "").trim()
        if (out.length === 0) {
            root.available = false
            root.syncState = "offline"
            root.activeFolder = ""
            root.folderCount = 0
            return
        }
        root.available = true

        var quoted = []
        var re = /'([^']*)'/g
        var m
        while ((m = re.exec(out)) !== null)
            quoted.push(m[1])

        root.syncState = quoted.length > 0 && quoted[0] !== "" ? quoted[0] : "idle"
        root.activeFolder = quoted.length > 1 ? quoted[1] : ""
        root.lastSync = quoted.length > 2 ? quoted[2] : ""

        var cm = out.match(/uint32\s+(\d+)/)
        root.folderCount = cm ? parseInt(cm[1]) : 0
    }

    Timer {
        interval: 3000
        running: true
        repeat: true
        onTriggered: root.refresh()
    }
    Component.onCompleted: root.refresh()

    // ---- Panel (compact) representation --------------------------------------
    compactRepresentation: Kirigami.Icon {
        source: root.iconFor(root.syncState)
        opacity: root.available ? 1.0 : 0.5
        active: mouseArea.containsMouse

        MouseArea {
            id: mouseArea
            anchors.fill: parent
            hoverEnabled: true
            onClicked: root.expanded = !root.expanded
        }

        RotationAnimator on rotation {
            running: root.syncState === "syncing"
            from: 0; to: 360
            duration: 1500
            loops: Animation.Infinite
        }
    }

    // ---- Expanded (full) representation ---------------------------------------
    fullRepresentation: ColumnLayout {
        Layout.minimumWidth: Kirigami.Units.gridUnit * 16
        Layout.minimumHeight: Kirigami.Units.gridUnit * 10
        spacing: Kirigami.Units.smallSpacing

        RowLayout {
            Layout.fillWidth: true
            Kirigami.Icon {
                source: root.iconFor(root.syncState)
                Layout.preferredWidth: Kirigami.Units.iconSizes.medium
                Layout.preferredHeight: Kirigami.Units.iconSizes.medium
            }
            ColumnLayout {
                spacing: 0
                PlasmaComponents.Label {
                    text: "Cirrust"
                    font.bold: true
                }
                PlasmaComponents.Label {
                    text: root.stateLabel()
                    opacity: 0.7
                    font.pointSize: Kirigami.Theme.smallFont.pointSize
                }
            }
        }

        Kirigami.Separator { Layout.fillWidth: true }

        PlasmaComponents.Label {
            text: root.available ? root.folderCount + " synced folder(s)"
                                 : "Start the app to sync your folders."
            opacity: 0.8
        }
        PlasmaComponents.Label {
            visible: root.available && root.lastSync !== ""
            text: "Last sync: " + root.lastSync.replace("T", " ").split(".")[0].split("+")[0]
            opacity: 0.6
            font.pointSize: Kirigami.Theme.smallFont.pointSize
        }

        Item { Layout.fillHeight: true }

        RowLayout {
            Layout.fillWidth: true
            PlasmaComponents.Button {
                text: "Sync now"
                icon.name: "view-refresh"
                enabled: root.available && root.syncState !== "syncing"
                onClicked: executable.run(root.syncCmd)
            }
            Item { Layout.fillWidth: true }
            PlasmaComponents.Button {
                text: "Open"
                icon.name: "cloud"
                onClicked: executable.run(root.openCmd)
            }
        }
    }
}
