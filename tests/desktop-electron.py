#!/usr/bin/python3
"""Real Electron AT-SPI discovery/acknowledgement; isolated renderer input seam.
NOT a native keyboard/portal or microphone test. Never sends global input.
Uses ARXELL_ELECTRON, installed electron, or this machine's OpenCode executable
with a separate minimal app and temporary profile (no OpenCode app/user data).
Run ./dev-build.sh test first. Requires a graphical desktop and GI-independent
Electron accessibility. The fixture explicitly enables renderer accessibility;
this does NOT prove that unconfigured Electron apps expose a usable field.
Creates/terminates only its own fixture processes.
"""
import os
import pathlib
import shutil
import signal
import subprocess
import tempfile
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
source = pathlib.Path(os.environ.get('ARXELL_ELECTRON') or shutil.which('electron') or '/opt/OpenCode/ai.opencode')
if not source.is_file():
    raise SystemExit('Set ARXELL_ELECTRON to an Electron executable')
binaries = sorted((ROOT / '.local/target/debug/deps').glob('arxell_speak_lib-*'), key=lambda p: p.stat().st_mtime, reverse=True)
binary = next((p for p in binaries if p.is_file() and os.access(p, os.X_OK)), None)
if not binary:
    raise SystemExit('Run ./dev-build.sh test first')

MAIN = r'''
const {app,BrowserWindow}=require('electron');const fs=require('node:fs');
app.setPath('userData',process.env.ARXELL_FIXTURE_PROFILE);
app.commandLine.appendSwitch('force-renderer-accessibility');
app.whenReady().then(async()=>{
 app.setAccessibilitySupportEnabled(true);
 const w=new BrowserWindow({width:650,height:240,title:'Arxell isolated Electron fixture',webPreferences:{nodeIntegration:false,contextIsolation:true}});
 const token=process.env.ARXELL_DESKTOP_TEST_TOKEN,stage=process.env.ARXELL_DESKTOP_TEST_STAGE,input=process.env.ARXELL_DESKTOP_TEST_RENDERER_INPUT||'';
 await w.loadURL('data:text/html,'+encodeURIComponent(`<h3>Isolated Electron fixture — synthetic text only</h3><textarea id="one" aria-label="${token}"></textarea><textarea id="two" aria-label="Focus switch"></textarea>`));
 let busy=false;
 setInterval(async()=>{
  if(busy)return;busy=true;
  try{
   const state=fs.existsSync(stage)?fs.readFileSync(stage,'utf8'):'';
   if(!fs.existsSync(stage+'.keys')&&await w.webContents.executeJavaScript('typeof window.__keys!=="undefined"&&(window.__keys.length>0)')){
    fs.writeFileSync(stage+'.keys',JSON.stringify(await w.webContents.executeJavaScript('window.__keys')));
   }
   if(state==='monitor-ready'){
    fs.writeFileSync(stage,'focus-pending');
    await new Promise(resolve=>setTimeout(resolve,300));
    w.hide();await new Promise(resolve=>setTimeout(resolve,200));w.show();w.focus();
    await w.webContents.executeJavaScript('document.getElementById("one").focus();window.__keys=[];addEventListener("keydown",e=>window.__keys.push(e.key),true)');
   }
   if(fs.existsSync(input)){
    const c=fs.readFileSync(input,'utf8');fs.unlinkSync(input);
    if(!w.isFocused()||!(await w.webContents.executeJavaScript('document.activeElement.id==="one"')))throw Error('Fixture lost focus; input refused');
    await w.webContents.insertText(c);fs.writeFileSync(input+'.ack','ok');
   }
   if(state==='typed'){
    fs.writeFileSync(stage+'.result',JSON.stringify(await w.webContents.executeJavaScript('[document.getElementById("one").value,document.getElementById("two").value,window.__keys||[]]')));    await w.webContents.executeJavaScript('document.getElementById("two").focus()');fs.writeFileSync(stage,'focus-changed');
   }
  }catch(e){fs.writeFileSync(stage+'.error',String(e));}finally{busy=false;}
 },10);
});app.on('window-all-closed',()=>app.quit());
'''

(ROOT / '.local').mkdir(exist_ok=True)
with tempfile.TemporaryDirectory(prefix='electron-field-', dir=ROOT / '.local') as tmp:
    directory = pathlib.Path(tmp)
    executable = directory / 'electron'
    try:
        os.link(source, executable)
    except OSError:
        shutil.copy2(source, executable)
    for item in source.parent.iterdir():
        if item.name not in (source.name, 'resources', 'electron'):
            (directory / item.name).symlink_to(item)
    resources = directory / 'resources/app'
    resources.mkdir(parents=True)
    (resources / 'package.json').write_text('{"name":"arxell-isolated-electron-fixture","version":"1.0.0","main":"main.cjs"}')
    (resources / 'main.cjs').write_text(MAIN)
    stage = directory / 'stage'
    token = 'arxell-test-' + uuid.uuid4().hex
    x11_xtest = bool(os.environ.get('ARXELL_DESKTOP_TEST_X11_XTEST'))
    env = dict(os.environ, ARXELL_DESKTOP_TEST_TOKEN=token, ARXELL_DESKTOP_TEST_ELECTRON='1', ARXELL_DESKTOP_TEST_STAGE=str(stage), ARXELL_FIXTURE_PROFILE=str(directory / 'profile'))
    if not x11_xtest:
        env['ARXELL_DESKTOP_TEST_RENDERER_INPUT'] = str(directory / 'input')
    else:
        env['ARXELL_DESKTOP_TEST_XTEST'] = '1'
    # Initialize the assistive client BEFORE launching Chromium. Renderer
    # accessibility is explicit in the fixture, not changed in the real app.
    import time
    native_env = dict(env, XDG_SESSION_TYPE='x11')
    native_env.pop('WAYLAND_DISPLAY', None)
    native = subprocess.Popen([str(binary), ('xtest_delivery_to_x11_fixture' if x11_xtest else 'native_delivery_to_named_fixture'), '--ignored', '--nocapture'], env=native_env)
    fixture = None
    with (directory / 'electron.log').open('w') as log:
        try:
            deadline = time.monotonic() + 5
            while not stage.exists():
                assert time.monotonic() < deadline and native.poll() is None, 'assistive client did not initialize'
                time.sleep(.01)
            if x11_xtest:
                env.pop('WAYLAND_DISPLAY', None)
                fixture = subprocess.Popen([str(executable), '--no-sandbox', '--disable-background-networking', '--ozone-platform=x11'], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            else:
                fixture = subprocess.Popen([str(executable), '--no-sandbox', '--disable-background-networking', '--ozone-platform=' + ('wayland' if os.environ.get('WAYLAND_DISPLAY') else 'x11')], env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            result = native.wait(timeout=30)
            if result:
                print((directory / 'electron.log').read_text()[-2000:])
                keys = pathlib.Path(str(stage) + '.keys')
                print('renderer key events:', keys.read_text() if keys.exists() else 'NONE reached the renderer')
                if pathlib.Path(str(stage) + '.error').exists():
                    print(pathlib.Path(str(stage) + '.error').read_text())
                raise SystemExit(result)
            import json
            values = json.loads(pathlib.Path(str(stage) + '.result').read_text())
            assert values[0:2] == ['Héllo 世界 + streaming', ''], values
            print('renderer key events:', values[2] if len(values) > 2 else 'n/a')
            print('PASS: ' + ('REAL XTest keyboard injection into an isolated X11 Electron fixture' if x11_xtest else 'real Electron Text-only discovery, Unicode streaming acknowledgements and focus refusal; renderer seam, NOT native keyboard validation'))
        finally:
            if native.poll() is None:
                native.terminate()
                native.wait(timeout=5)
            if fixture is not None:
                os.killpg(fixture.pid, signal.SIGTERM)
                fixture.wait(timeout=10)
