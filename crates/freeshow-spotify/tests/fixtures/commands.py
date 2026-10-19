#!/usr/bin/python3
"""Bounded private GIO peer: no ambient session or Spotify account."""
import json
import os
import signal
import sys
from gi.repository import Gio, GLib

profile = sys.argv[1]
assert profile in {"ordinary", "objectPath", "missing", "unsafe", "hold", "fail"}
address = os.environ["DBUS_SESSION_BUS_ADDRESS"]
assert address.startswith("unix:path=") and ";" not in address
connection = Gio.DBusConnection.new_for_address_sync(address, Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
xml = '''<node>
<interface name="org.freedesktop.DBus.Properties">
<method name="Get"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="out"/></method>
<method name="Set"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="v" direction="in"/></method>
</interface>
<interface name="org.mpris.MediaPlayer2.Player">
<method name="PlayPause"/><method name="Next"/><method name="Previous"/><method name="Pause"/>
<method name="SetPosition"><arg type="o" direction="in"/><arg type="x" direction="in"/></method>
</interface></node>'''
info = Gio.DBusNodeInfo.new_for_xml(xml)
loop = GLib.MainLoop()
held = []
count = 0

def event(name, args):
    global count
    count += 1
    assert count <= 128
    print(json.dumps({"name": name, "args": args}, ensure_ascii=False), flush=True)

def method(_conn, _sender, _path, interface, name, parameters, invocation):
    args = parameters.unpack()
    event(name, args)
    if name == "Get":
        assert args == ("org.mpris.MediaPlayer2.Player", "Metadata")
        metadata = {"xesam:title": GLib.Variant("s", "Controlled 日本語")}
        if profile != "missing":
            metadata["mpris:trackid"] = GLib.Variant("o" if profile == "objectPath" else "s", "/fixture/track_01" if profile != "unsafe" else "/fixture/not safe")
        value = GLib.Variant("(v)", (GLib.Variant("a{sv}", metadata),))
        if profile == "hold":
            assert len(held) < 4
            held.append((invocation, value))
        elif profile == "fail":
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "controlled private metadata failure")
        else:
            invocation.return_value(value)
    else:
        if name == "Set":
            assert args[0:2] == ("org.mpris.MediaPlayer2.Player", "Volume")
        else:
            assert interface == "org.mpris.MediaPlayer2.Player"
        invocation.return_value(GLib.Variant("()", ()))

def control(_fd, condition):
    if condition & GLib.IOCondition.HUP:
        loop.quit()
        return False
    line = sys.stdin.readline()
    assert len(line) <= 64
    if line.strip() == "release":
        for invocation, value in held:
            invocation.return_value(value)
        held.clear()
    elif line.strip() == "stop":
        loop.quit()
    else:
        raise RuntimeError("invalid fixture control")
    return True

registrations = [connection.register_object("/org/mpris/MediaPlayer2", item, method, None, None) for item in info.interfaces]
reply = connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", ("org.mpris.MediaPlayer2.spotify", 0)), GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE, 1000, None)
assert reply.unpack()[0] == 1
GLib.io_add_watch(sys.stdin.fileno(), GLib.IOCondition.IN | GLib.IOCondition.HUP, control)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, lambda: (loop.quit(), False)[1])
print("READY", flush=True)
try:
    loop.run()
finally:
    for invocation, _value in held:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "controlled fixture teardown")
    for item in registrations:
        connection.unregister_object(item)
    connection.close_sync(None)
