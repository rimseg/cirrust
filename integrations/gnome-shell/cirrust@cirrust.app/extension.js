// Cirrust — GNOME Shell indicator.
//
// Mirrors the Plasma widget: polls the app's session D-Bus service
// (org.cirrust.client /Sync) and shows the sync state in the top bar with a
// small menu (status, Sync now, Open). GNOME Shell 45+ (ESM extensions).

import GObject from 'gi://GObject';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';

const BUS_NAME = 'org.cirrust.client.Daemon';
const OBJECT_PATH = '/Sync';
const INTERFACE = 'org.cirrust.client.Sync';
const POLL_SECONDS = 3;

// Adwaita symbolic icons per sync state.
const STATE_ICONS = {
    idle: 'emblem-default-symbolic',
    syncing: 'emblem-synchronizing-symbolic',
    paused: 'media-playback-pause-symbolic',
    error: 'dialog-error-symbolic',
    offline: 'network-offline-symbolic',
    gone: 'network-offline-symbolic', // app not running
};

const Indicator = GObject.registerClass(
class CirrustIndicator extends PanelMenu.Button {
    _init() {
        super._init(0.0, 'Cirrust');

        this._icon = new St.Icon({
            icon_name: STATE_ICONS.gone,
            style_class: 'system-status-icon',
        });
        this.add_child(this._icon);

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
    }

    _setState(state, label) {
        this._icon.icon_name = STATE_ICONS[state] ?? STATE_ICONS.gone;
        this._statusItem.label.text = label;
        this._syncItem.setSensitive(state !== 'gone' && state !== 'syncing');
    }

    refresh() {
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, 'Status',
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            (conn, res) => {
                try {
                    const reply = conn.call_finish(res);
                    const [state, activeFolder, folderCount] = reply.deepUnpack();
                    let label;
                    if (state === 'syncing' && activeFolder)
                        label = `Syncing ${activeFolder}`;
                    else
                        label = `${state.charAt(0).toUpperCase()}${state.slice(1)}` +
                            (folderCount ? ` — ${folderCount} folder(s)` : '');
                    this._setState(state, label);
                } catch {
                    this._setState('gone', 'Not running — click Open to start');
                }
            });
    }

    _call(method) {
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, method,
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            () => this.refresh());
    }

    _open() {
        // Raise via D-Bus when running; otherwise launch the binary.
        Gio.DBus.session.call(
            BUS_NAME, OBJECT_PATH, INTERFACE, 'Open',
            null, null, Gio.DBusCallFlags.NONE, 800, null,
            (conn, res) => {
                try {
                    conn.call_finish(res);
                } catch {
                    try {
                        Gio.Subprocess.new(['cirrust'],
                            Gio.SubprocessFlags.NONE);
                    } catch (e) {
                        Main.notifyError('Cirrust',
                            'Could not launch the app — is it installed?');
                    }
                }
            });
    }
});

export default class CirrustExtension extends Extension {
    enable() {
        this._indicator = new Indicator();
        Main.panel.addToStatusArea(this.uuid, this._indicator);
        this._indicator.refresh();
        this._timer = GLib.timeout_add_seconds(
            GLib.PRIORITY_DEFAULT, POLL_SECONDS, () => {
                this._indicator?.refresh();
                return GLib.SOURCE_CONTINUE;
            });
    }

    disable() {
        if (this._timer) {
            GLib.source_remove(this._timer);
            this._timer = null;
        }
        this._indicator?.destroy();
        this._indicator = null;
    }
}
