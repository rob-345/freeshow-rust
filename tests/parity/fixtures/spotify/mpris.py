#!/usr/bin/python3
"""Owned private-bus test service; never connects to a desktop session."""
import os
import signal
import sys

from gi.repository import Gio, GLib

profile = sys.argv[1]
assert profile in {
    "ordinary", "signedLength", "negative", "playingTitle", "quoted",
    "missingTitle", "emptyTitle", "missingArt", "failPlayback",
    "holdPosition", "holdMetadata",
}
address = os.environ["DBUS_SESSION_BUS_ADDRESS"]
assert address.startswith("unix:path=") and ";" not in address
connection = Gio.DBusConnection.new_for_address_sync(
    address,
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None,
)
xml = """<node><interface name="org.freedesktop.DBus.Properties">
<method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/>
<arg type="v" direction="out"/></method></interface></node>"""
info = Gio.DBusNodeInfo.new_for_xml(xml)
loop = GLib.MainLoop()
held = []
held_once = False
title = "Song 日本語"
if profile == "playingTitle":
    title = "Playing title"
elif profile == "quoted":
    title = 'say "hello"\nline 🌍'
elif profile == "emptyTitle":
    title = ""
metadata = {
    "xesam:title": GLib.Variant("s", title),
    "xesam:artist": GLib.Variant("as", ["First", "Second"]),
    "mpris:length": GLib.Variant("x" if profile == "signedLength" else "t", 120000000),
    "mpris:artUrl": GLib.Variant("s", "file:///fixture/a.jpg"),
}
if profile == "missingTitle":
    del metadata["xesam:title"]
if profile == "missingArt":
    del metadata["mpris:artUrl"]
values = {
    "Metadata": GLib.Variant("a{sv}", metadata),
    "PlaybackStatus": GLib.Variant("s", "Paused"),
    "Position": GLib.Variant("x", -3000000 if profile == "negative" else 3000000),
    "Volume": GLib.Variant("d", -0.5 if profile == "negative" else 0.5),
}


def method(_conn, _sender, _path, _interface, name, parameters, invocation):
    global held_once
    interface, prop = parameters.unpack()
    assert name == "Get" and interface == "org.mpris.MediaPlayer2.Player"
    assert prop in values
    print("GET " + prop, flush=True)
    if profile == "failPlayback" and prop == "PlaybackStatus":
        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "controlled property error")
    elif not held_once and ((profile == "holdPosition" and prop == "Position") or (profile == "holdMetadata" and prop == "Metadata")):
        held_once = True
        held.append((invocation, values[prop]))
    else:
        invocation.return_value(GLib.Variant("(v)", (values[prop],)))


def control(_fd, condition):
    if condition & GLib.IOCondition.HUP:
        loop.quit()
        return False
    command = sys.stdin.readline().strip()
    if command == "release":
        for invocation, value in held:
            invocation.return_value(GLib.Variant("(v)", (value,)))
        held.clear()
    elif command == "stop":
        loop.quit()
    else:
        raise RuntimeError("unexpected fixture control")
    return True


registration = connection.register_object("/org/mpris/MediaPlayer2", info.interfaces[0], method, None, None)
result = connection.call_sync(
    "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
    GLib.Variant("(su)", ("org.mpris.MediaPlayer2.spotify", 0)), GLib.VariantType("(u)"),
    Gio.DBusCallFlags.NONE, 1000, None,
)
assert result.unpack()[0] == 1
GLib.io_add_watch(sys.stdin.fileno(), GLib.IOCondition.IN | GLib.IOCondition.HUP, control)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, lambda: (loop.quit(), False)[1])
print("READY", flush=True)
try:
    loop.run()
finally:
    for invocation, _value in held:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "fixture stopped")
    connection.unregister_object(registration)
    connection.close_sync(None)
