// Cirrust — Cinnamon panel applet.
//
// Same model as the Plasma widget and GNOME extension: poll the app's
// session D-Bus service (org.cirrust.client /Sync) and reflect the sync state.

const Applet = imports.ui.applet;
const PopupMenu = imports.ui.popupMenu;
const Gio = imports.gi.Gio;
const GLib = imports.gi.GLib;
const Util = imports.misc.util;

const BUS_NAME = 'org.cirrust.client.Daemon';
const OBJECT_PATH = '/Sync';
const INTERFACE = 'org.cirrust.client.Sync';
const POLL_SECONDS = 3;

// Symbolic icon names (Cinnamon appends "-symbolic").
const STATE_ICONS = {
    idle: 'emblem-default',
    syncing: 'emblem-synchronizing',
    paused: 'media-playback-pause',
    error: 'dialog-error',
    offline: 'network-offline',
    gone: 'network-offline',
};

class NextcloudVueClientApplet extends Applet.IconApplet {
    constructor(orientation, panelHeight, instanceId) {
        super(orientation, panelHeight, instanceId);

        this.set_applet_icon_symbolic_name(STATE_ICONS.gone);
        this.set_applet_tooltip('Cirrust — not running');

        this.menuManager = new PopupMenu.PopupMenuManager(this);
        this.menu = new Applet.AppletPopupMenu(this, orientation);
        this.menuManager.addMenu(this.menu);

        this._statusItem = new PopupMenu.PopupMenuItem('Not running', {
            reactive: false,
        });
        this.menu.addMenuItem(this._statusItem);
        this.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        this._syncItem = new PopupMenu.PopupMenuItem('Sync now');
        this._syncItem.connect('activate', () => this._call('SyncNow'));
        this.menu.addMenuItem(this._syncItem);

        const openItem = new PopupMenu.PopupMenuItem('Open Cirrust');
        openItem.connect('activate', () => this._open());
        this.menu.addMenuItem(openItem);

        this._refresh();
        this._timer = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT, POLL_SECONDS, () => {
                this._refresh();
                return GLib.SOURCE_CONTINUE;
            });
    }

    on_applet_clicked() {
        this.menu.toggle();
    }

    on_applet_removed_from_panel() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = null;
        }
    }

    _setState(state, label) {
        this.set_applet_icon_symbolic_name(STATE_ICONS[state] || STATE_ICONS.gone);
        this.set_applet_tooltip(`Cirrust — ${label}`);
        this._statusItem.label.text = label;
    }

    _refresh() {
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, 'Status',
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            (conn, res) => {
                try {
                    const [state, activeFolder, folderCount] =
                        conn.call_finish(res).deep_unpack();
                    let label;
                    if (state === 'syncing' && activeFolder)
                        label = `Syncing ${activeFolder}`;
                    else
                        label = state.charAt(0).toUpperCase() + state.slice(1) +
                            (folderCount ? ` — ${folderCount} folder(s)` : '');
                    this._setState(state, label);
                } catch (e) {
                    this._setState('gone', 'Not running');
                }
            });
    }

    _call(method) {
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, method,
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            () => this._refresh());
    }

    _open() {
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, 'Open',
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch (e) {
                    Util.spawnCommandLine('cirrust');
                }
            });
    }
}

function main(metadata, orientation, panelHeight, instanceId) {
    return new NextcloudVueClientApplet(orientation, panelHeight, instanceId);
}
