// Runs only inside the throwaway session scripts/demo-video.sh records.
// GNOME Shell keeps window positions and input to itself, so the tour asks
// this extension to place windows, say where windows and dock icons are,
// and move the pointer and press keys through virtual devices.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const IFACE = `<node><interface name="dev.penguinmail.Tour">
  <method name="Place">
    <arg type="s" name="title" direction="in"/>
    <arg type="i" name="x" direction="in"/><arg type="i" name="y" direction="in"/>
    <arg type="i" name="width" direction="in"/><arg type="i" name="height" direction="in"/>
  </method>
  <method name="Frame">
    <arg type="s" name="title" direction="in"/>
    <arg type="(iiii)" name="frame" direction="out"/>
  </method>
  <method name="Titles"><arg type="as" name="titles" direction="out"/></method>
  <method name="DashIcon">
    <arg type="s" name="app" direction="in"/>
    <arg type="(dddd)" name="box" direction="out"/>
  </method>
  <method name="Pointer">
    <arg type="d" name="x" direction="in"/><arg type="d" name="y" direction="in"/>
  </method>
  <method name="Button">
    <arg type="u" name="button" direction="in"/><arg type="b" name="pressed" direction="in"/>
  </method>
  <method name="Scroll">
    <arg type="d" name="dx" direction="in"/><arg type="d" name="dy" direction="in"/>
  </method>
  <method name="Key">
    <arg type="u" name="keyval" direction="in"/><arg type="b" name="pressed" direction="in"/>
  </method>
</interface></node>`;

function now() {
    return GLib.get_monotonic_time();
}

// The newest window whose title holds `title`, since a composer's title
// can share words with the conversation behind it.
function windowTitled(title) {
    const found = global.get_window_actors()
        .map(actor => actor.meta_window)
        .filter(window => (window.get_title() ?? '').includes(title));
    if (found.length === 0)
        throw new Error(`no window titled ${title}`);
    return found[found.length - 1];
}

export default class TourExtension extends Extension {
    enable() {
        const seat = global.stage.context.get_backend().get_default_seat();
        this._pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        this._keyboard = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        const tour = {
            Place: (title, x, y, width, height) => {
                windowTitled(title).move_resize_frame(false, x, y, width, height);
            },
            Frame: title => {
                const frame = windowTitled(title).get_frame_rect();
                return [frame.x, frame.y, frame.width, frame.height];
            },
            Titles: () => global.get_window_actors().map(actor => actor.meta_window.get_title() ?? ''),
            DashIcon: app => {
                for (const item of Main.overview.dash._box.get_children()) {
                    if (item.child?.app?.get_id() !== app)
                        continue;
                    const [x, y] = item.get_transformed_position();
                    const [width, height] = item.get_transformed_size();
                    return [x, y, width, height];
                }
                throw new Error(`no dash icon for ${app}`);
            },
            Pointer: (x, y) => this._pointer.notify_absolute_motion(now(), x, y),
            // `button` is Clutter's number: 1 is the left button, 3 the right.
            Button: (button, pressed) => this._pointer.notify_button(now(), button,
                pressed ? Clutter.ButtonState.PRESSED : Clutter.ButtonState.RELEASED),
            Scroll: (dx, dy) => this._pointer.notify_scroll_continuous(now(), dx, dy,
                Clutter.ScrollSource.FINGER, Clutter.ScrollFinishFlags.NONE),
            Key: (keyval, pressed) => this._keyboard.notify_keyval(now(), keyval,
                pressed ? Clutter.KeyState.PRESSED : Clutter.KeyState.RELEASED),
        };
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, tour);
        this._dbus.export(Gio.DBus.session, '/dev/penguinmail/Tour');

        // The recording indicator would sit in every frame of the video.
        this._hider = GLib.timeout_add(GLib.PRIORITY_DEFAULT, 200, () => {
            for (const name of ['screenRecording', 'screenSharing', 'remoteAccess']) {
                const box = Main.panel.statusArea[name]?.container;
                if (box?.visible)
                    box.hide();
            }
            return GLib.SOURCE_CONTINUE;
        });
    }

    disable() {
        GLib.source_remove(this._hider);
        this._dbus.unexport();
        this._pointer = null;
        this._keyboard = null;
    }
}
