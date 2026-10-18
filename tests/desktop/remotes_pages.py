"""Load the actual compiled client in a separate, ephemeral WebKit view."""
import json
import sys
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2

fixture=json.load(sys.stdin)
kind=fixture["kind"]
manager=WebKit2.UserContentManager()
manager.register_script_message_handler("freeshowTest")
receipt=None

def received(_manager,result):
    global receipt
    if receipt is not None:
        return
    receipt=json.loads(result.get_js_value().to_string())
    window.destroy()
    Gtk.main_quit()

manager.connect("script-message-received::freeshowTest",received)
probe="""
(()=>{
const fixture=FIXTURE,errors=[];let submitted=false;
window.addEventListener('error',event=>errors.push(String(event.message)));
window.addEventListener('unhandledrejection',event=>errors.push(String(event.reason)));
setTimeout(()=>window.webkit.messageHandlers.freeshowTest.postMessage(JSON.stringify({kind:fixture.kind,failed:true,uri:location.href,text:document.body?.innerText,html:document.body?.innerHTML.slice(0,8000),errors})),18000);
const interval=setInterval(()=>{
 if(!document.body)return;
 if(fixture.kind==='REMOTE'){
  const input=document.querySelector('#password-input');
  if(input && !submitted){submitted=true;input.value=fixture.password;input.dispatchEvent(new Event('input',{bubbles:true}));input.dispatchEvent(new Event('change',{bubbles:true}));input.closest('form').dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));}
 }
 const ready=fixture.kind==='REMOTE'?!!document.querySelector('.phoneMode .tabs'):fixture.kind==='STAGE'?!!document.querySelector('.main .slide'):document.querySelectorAll('.controller .quart').length===2;
 if(!ready)return;
 clearInterval(interval);
 window.webkit.messageHandlers.freeshowTest.postMessage(JSON.stringify({kind:fixture.kind,title:document.title,uri:location.href,authenticated:!document.querySelector('#password-input'),text:document.body.innerText.slice(0,4000),resources:performance.getEntriesByType('resource').map(entry=>({name:entry.name,bytes:entry.encodedBodySize})),errors}));
},100);
})();
""".replace("FIXTURE",json.dumps(fixture))
manager.add_script(WebKit2.UserScript.new(probe,WebKit2.UserContentInjectedFrames.TOP_FRAME,WebKit2.UserScriptInjectionTime.START,None,None))
context=WebKit2.WebContext.new_ephemeral()
view=WebKit2.WebView(web_context=context,user_content_manager=manager)
window=Gtk.Window()
window.set_default_size(720,720)
window.add(view)
window.show_all()

def deadline():
    if receipt is None:
        print("Client page deadline: "+kind,file=sys.stderr)
        window.destroy()
        Gtk.main_quit()
    return False

GLib.timeout_add_seconds(20,deadline)
view.load_uri(fixture["url"])
Gtk.main()
if receipt is None:
    sys.exit(1)
assert receipt.get("authenticated") and not receipt["errors"],receipt
print(json.dumps(receipt))
