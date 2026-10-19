"""One real ephemeral WebKitGTK view, with the installed Wry audio policy.

The explicit media permission grant is fixture authority, not Tauri permission
parity. There is no substituted AudioContext, worklet, media or capture backend.
"""
import json
import sys
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gtk, GLib, WebKit2


def run(url, title, policy="allow"):
    context = WebKit2.WebContext.new_ephemeral()
    manager = WebKit2.UserContentManager()
    assert manager.register_script_message_handler("receipt")
    policies = WebKit2.WebsitePolicies(autoplay=WebKit2.AutoplayPolicy.ALLOW if policy == "allow" else WebKit2.AutoplayPolicy.DENY)
    view = WebKit2.WebView(web_context=context, user_content_manager=manager, website_policies=policies)
    view.get_settings().set_enable_webaudio(True)
    window = Gtk.Window(title=title)
    window.set_decorated(False)
    window.set_default_size(800, 600)
    window.add(view)
    finished = False
    native = {"webkit": ".".join(str(v) for v in [WebKit2.get_major_version(), WebKit2.get_minor_version(), WebKit2.get_micro_version()]), "autoplay": policy, "webAudio": view.get_settings().get_enable_webaudio(), "mediaStream": view.get_settings().get_enable_media_stream(), "ephemeral": context.is_ephemeral(), "permissionRequests": []}

    def finish(data):
        nonlocal finished
        if finished:
            return
        finished = True
        print(json.dumps(dict(data, native=native), ensure_ascii=False), flush=True)
        view.terminate_web_process()
        window.destroy()
        Gtk.main_quit()

    def receipt(_manager, result):
        try:
            text = result.get_js_value().to_string()
            if len(text.encode()) > 256 * 1024:
                raise ValueError("WebKit receipt bound")
            data = json.loads(text)
            if data.get("finished"):
                finish(data)
            else:
                print(json.dumps(data), flush=True)
        except Exception as error:
            finish({"finished": True, "ok": False, "error": str(error)})

    def permission(_view, request):
        if isinstance(request, WebKit2.UserMediaPermissionRequest):
            if len(native["permissionRequests"]) >= 4:
                request.deny()
                return True
            native["permissionRequests"].append({"audio": WebKit2.user_media_permission_is_for_audio_device(request), "video": WebKit2.user_media_permission_is_for_video_device(request), "fixtureGrant": True})
            if WebKit2.user_media_permission_is_for_audio_device(request) and not WebKit2.user_media_permission_is_for_video_device(request):
                request.allow()
            else:
                request.deny()
            return True
        return False

    manager.connect("script-message-received::receipt", receipt)
    view.connect("permission-request", permission)
    view.connect("web-process-terminated", lambda _view, reason: finish({"finished": True, "ok": False, "error": "Real WebKit process terminated: " + str(reason)}) if not finished else None)
    GLib.timeout_add_seconds(18, lambda: finish({"finished": True, "ok": False, "error": "Native WebKit fixture deadline"}) or False)
    window.show_all()
    view.load_uri(url)
    Gtk.main()


if __name__ == "__main__":
    run(*sys.argv[1:])
