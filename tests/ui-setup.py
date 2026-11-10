"""Isolated startup permission/readiness tests. Mock IPC; no mic or OS consent."""
import json
from pathlib import Path
import subprocess
import sys
import gi

gi.require_version('Gtk', '3.0')
gi.require_version('WebKit2', '4.1')
from gi.repository import GLib, Gtk, WebKit2

root = Path(__file__).resolve().parent.parent
server = subprocess.Popen(['node', 'tests/ui-preview.mjs'], cwd=root, stdout=subprocess.PIPE, text=True)
url = server.stdout.readline().strip().split()[-1]
window = Gtk.Window(title='Arxell isolated setup test — no native input')
window.set_default_size(400, 600)
window.set_focus_on_map(False)
window.set_accept_focus(False)
view = WebKit2.WebView()
window.add(view)
window.show_all()
failed = True
SCRIPT = r'''
(async()=>{
 const $=s=>document.querySelector(s),calls=n=>testEvents.filter(e=>e.name===n);
 const check=(ok,text)=>{if(!ok)throw Error(text)};
 const wait=ms=>new Promise(r=>setTimeout(r,ms));
 const until=async fn=>{for(let i=0;i<500;i++){if(fn())return;await wait(20)}throw Error('Setup UI timed out')};
 await until(()=>typeof testCompletePermissions==='function');
 check(calls('setup_permissions').length===1,'first launch automatically starts required setup exactly once');
 const banner=$('#setup-panel'), bannerRect=banner.getBoundingClientRect();
 check(!banner.hidden && banner.previousElementSibling.tagName==='HEADER' && banner.parentElement===document.body,'setup is directly below the header, outside scrolling content');
 check(bannerRect.height<=48 && !$('#setup-checks') && !banner.querySelector('h2'),'setup is a slim single row, not a checklist section');
 check($('#setup-message').textContent.includes('microphone') && $('#setup-message').textContent.includes('desktop input'),'outstanding access is shown');
 check(!/verified|global shortcut|transcription model/i.test(banner.textContent),'completed checks are not displayed');
 check($('#setup-permissions').getBoundingClientRect().left >= $('#setup-message').getBoundingClientRect().right,'retry is on the right');
 check($('#dictate').disabled && $('#resume').disabled,'record and resume blocked while permission is pending');
 check($('#state-label').getAttribute('aria-label')!=='Ready','loaded model alone never shows Ready');
 check(document.documentElement.scrollWidth<=innerWidth+1,'setup fits 400 px without horizontal clipping');
 $('#dictate').dispatchEvent(new MouseEvent('click',{bubbles:true}));
 await wait(50);
 check(calls('start_dictation').length===0,'programmatic click cannot bypass the setup gate');
 let blocked=false;try{await __TAURI__.core.invoke('start_dictation')}catch{blocked=true}
 check(blocked,'mock native start also enforces setup');
 testPermissionHold=false;testCompletePermissions();
 await until(()=>!$('#dictate').disabled && $('#setup-panel').hidden);
 check($('#state-label').getAttribute('aria-label')==='Ready','Ready only after native requirements complete');
 testEmit('model-state',{state:'missing',name:null});
 check($('#dictate').disabled,'missing model immediately disables recording');
 await until(()=>!$('#setup-panel').hidden);
 const before=calls('setup_permissions').length;
 testEmit('model-state',{state:'ready',name:'Test model'});
 await until(()=>!$('#dictate').disabled);
 check(calls('setup_permissions').length===before,'model readiness does not re-prompt for OS permissions');
 testMicReady=true;testDesktopReady=false;
 await until(()=>$('#dictate').disabled && !$('#setup-panel').hidden);
 await wait(1700);
 check(calls('setup_permissions').length===1,'revocation blocks input without a repeated prompt loop');
 testPermissionFail=true;
 $('#setup-permissions').click();
 await until(()=>$('#setup-message').textContent.includes('denied') && !$('#setup-permissions').disabled);
 check($('#dictate').disabled && calls('setup_permissions').length===2,'denial stays not-ready and offers explicit retry');
 // The retry action must remain reachable even after scrolling deep into Settings.
 $('#settings-toggle').click();
 $('main').scrollTop=$('main').scrollHeight;
 const retry=$('#setup-permissions'), rect=retry.getBoundingClientRect();
 check(!retry.disabled && rect.top>=0 && rect.bottom<=innerHeight,'banner retry stays visible above scrolled settings');
 testPermissionHold=true;testPermissionFail=false;testCompletePermissions=null;
 retry.click();
 await until(()=>calls('setup_permissions').length===3 && typeof testCompletePermissions==='function');
 check(!$('#message').textContent.includes('denied') && $('#message').textContent.includes('Checking required access'),'retry clears the obsolete footer error');
 check(!$('#setup-message').textContent.includes('denied'),'pending retry does not display the previous failure as current status');
 delete window.testDesktopReady;testPermissionHold=false;testCompletePermissions();
 await until(()=>!$('#dictate').disabled && $('#setup-panel').hidden);
 check($('#message').textContent==='Required access verified.','successful retry replaces the failure with verified status');
 check($('#desktop-keyboard-status').textContent==='Granted' && $('#desktop-permission').hidden,'settings permission status updates after grant');
 testDesktopReady=false;
 await until(()=>$('#dictate').disabled);
 const external=document.getElementById('setting-advanced.desktop_enabled');
 external.checked=false;external.dispatchEvent(new Event('change',{bubbles:true}));
 await until(()=>calls('save_settings').length && !$('#dictate').disabled);
 check($('#state-label').getAttribute('aria-label').includes('in-app only'),'explicit opt-out labels the reduced mode honestly');
 check(calls('setup_permissions').length===3,'disabling external typing never opens a new permission request');
 check($('#desktop-keyboard-status').textContent==='Disabled','explicit opt-out is reflected in keyboard status');
 window.testResult={ok:true,width:innerWidth};
})().catch(error=>{window.testResult={ok:false,error:String(error)}});
'''

def loaded(_view, event):
    if event == WebKit2.LoadEvent.FINISHED:
        view.run_javascript(SCRIPT, None, None, None)

def poll():
    def got(_view, result, _data):
        global failed
        try:
            value = view.run_javascript_finish(result).get_js_value().to_string()
            if value == 'null':
                return
            outcome = json.loads(value)
            print('Setup UI:', outcome, flush=True)
            failed = not outcome['ok']
        except Exception as error:
            print(error, file=sys.stderr)
        Gtk.main_quit()
    view.run_javascript('JSON.stringify(window.testResult || null)', None, got, None)
    return True

view.connect('load-changed', loaded)
view.load_uri(url+'?setup=pending')
GLib.timeout_add(150, poll)
GLib.timeout_add_seconds(40, lambda: Gtk.main_quit())
try:
    Gtk.main()
finally:
    server.terminate()
    server.wait(timeout=5)
    window.destroy()
sys.exit(1 if failed else 0)
