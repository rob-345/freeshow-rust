"""Bounded WebView execution receipts shared by real desktop probes."""
import json

WRAPPER = """
                const receipts=window.__desktopScriptReceipts ||= new Map();
                const key=KEY;
                if(!receipts.has(key)) {
                    const entry={state:'running'};
                    receipts.set(key,entry);
                    if(receipts.size>128)receipts.delete(receipts.keys().next().value);
                    try {
                        entry.value=(function(){ SOURCE }).apply(window,arguments);
                        entry.state='done';
                    } catch(error) {
                        entry.state='error';entry.error=String(error);throw error;
                    }
                }
                const entry=receipts.get(key);
                if(entry.state!=='done')throw new Error(entry.error || 'Script still running');
                return entry.value;
            """

def wrap_script(key, source):
    return WRAPPER.replace("KEY", json.dumps(str(key)), 1).replace("SOURCE", source, 1)
