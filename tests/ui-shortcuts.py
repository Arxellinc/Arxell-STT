"""Isolated WebKit shortcut-recorder checks; no microphone or global input."""
import json
from pathlib import Path
import subprocess
import sys

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import GLib, Gtk, WebKit2

root = Path(__file__).resolve().parent.parent
server = subprocess.Popen(["node", "tests/ui-preview.mjs"], cwd=root, stdout=subprocess.PIPE, text=True)
url = server.stdout.readline().strip().split()[-1]
window = Gtk.Window(title="Arxell isolated shortcut-recorder test")
window.set_default_size(720, 520)
window.set_focus_on_map(False)
window.set_accept_focus(False)
view = WebKit2.WebView()
window.add(view)
window.show_all()
failed = True
stage = 0

SCRIPT = r"""
(async () => {
  const $ = selector => document.querySelector(selector);
  let input;
  let button;
  const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
  const check = (ok, text) => { if (!ok) throw new Error(text); };
  const until = async predicate => {
    for (let i = 0; i < 100; ++i) { if (predicate()) return; await wait(20); }
    throw new Error('Timed out waiting for recorder');
  };
  const calls = name => testEvents.filter(event => event.name === name);
  const key = (code, options = {}) => {
    const event = new KeyboardEvent('keydown', {bubbles:true, cancelable:true, code, key:code, ...options});
    document.dispatchEvent(event);
    check(event.defaultPrevented, 'captured key must not trigger browser/button actions');
  };
  const recording = () => button.getAttribute('aria-pressed') === 'true';
  const idle = () => !recording() && !button.disabled;
  const begin = async () => { button.click(); await until(recording); };
  const finish = async () => { button.click(); await until(idle); };
  const lastSaved = () => calls('save_settings').at(-1)?.args.settings.shortcut;
  await until(() => !$('#dictate').disabled);
  input = document.getElementById('setting-shortcut');
  button = $('#record-shortcut');
  $('#settings-toggle').click();
  check(button.getBoundingClientRect().left >= input.getBoundingClientRect().right, 'record button is to the right of the field');
  const before = calls('save_settings').length;
  await begin();
  check(input.readOnly && $('#dictate').disabled, 'capture prevents editing and microphone start');
  check(document.getElementById('setting-cancel_key').disabled, 'other settings cannot change mid-capture');
  key('ControlLeft', {key:'Control', ctrlKey:true});
  key('Space', {key:' ', ctrlKey:true, shiftKey:true});
  check(input.value === 'Ctrl+Shift+Space', 'shows exact captured chord');
  await wait(350);
  check(calls('save_settings').length === before, 'does not auto-save a candidate before Stop recording');
  check(button.textContent === 'Stop recording', 'second click ends recording');
  await finish();
  check(lastSaved() === 'Ctrl+Shift+Space', 'second click saves captured shortcut automatically');
  check(!input.readOnly && !input.disabled, 'field unlocked after save');
  check(!document.getElementById('setting-cancel_key').disabled, 'other settings unlocked');

  let saves = calls('save_settings').length;
  await begin(); key('KeyK', {ctrlKey:true}); key('Escape', {key:'Escape'});
  await until(idle);
  check(input.value === 'Ctrl+Shift+Space' && calls('save_settings').length === saves, 'Escape preserves previous binding');
  check(calls('cancel_dictation').length === 0, 'Escape capture does not cancel a dictation session');
  await begin(); await finish();
  check(calls('save_settings').length === saves, 'stopping without a chord preserves binding');
  await begin(); key('KeyA', {key:'a'});
  check(input.getAttribute('aria-invalid') === 'true', 'bare key explains modifier requirement');
  await finish();
  check(calls('save_settings').length === saves, 'invalid key is not saved');
  await begin(); key('Equal', {key:'+', ctrlKey:true, shiftKey:true});
  key('Equal', {key:'+', ctrlKey:true, shiftKey:true, repeat:true});
  await finish();
  check(lastSaved() === 'Ctrl+Shift+Equal', 'plus and key repeat map safely');

  await begin(); key('KeyZ', {ctrlKey:true});
  window.dispatchEvent(new Event('blur'));
  await until(idle);
  check(input.value === 'Ctrl+Shift+Equal', 'window blur discards uncommitted capture');
  await begin(); key('KeyZ', {ctrlKey:true});
  testEmit('shortcut-capture-cancelled', null);
  await until(idle);
  check(input.value === 'Ctrl+Shift+Equal', 'native focus/close event discards capture');
  await begin(); key('KeyZ', {ctrlKey:true});
  $('#settings-toggle').click();
  await until(idle);
  check(input.value === 'Ctrl+Shift+Equal', 'leaving settings discards uncommitted capture');
  $('#settings-toggle').click();

  // The native OS can consume the already-registered chord before DOM keydown.
  await begin();
  testEmit('shortcut-captured', 'Ctrl+Shift+Equal');
  check(input.value === 'Ctrl+Shift+Equal', 'captures existing registered binding through native event');
  check(calls('start_dictation').length === 0, 'recording shortcut never starts microphone');
  await finish();

  await begin(); key('KeyP', {ctrlKey:true}); window.testFailSave = true;
  await finish();
  check(input.value === 'Ctrl+Shift+Equal', 'failed registration restores previous binding');
  check($('#message').textContent.includes('already in use'), 'registration failure is visible');
  check(!input.disabled, 'save failure unlocks controls');

  // A lost window while the native begin command is still pending must clean up.
  window.testBeginDelay = 100;
  button.click(); window.dispatchEvent(new Event('blur'));
  await until(idle);
  window.testBeginDelay = 0;
  check(input.value === 'Ctrl+Shift+Equal', 'cancel during begin preserves binding');
  await begin(); key('F9', {key:'F9', ctrlKey:true});
  key('Enter', {key:'Enter'}); await until(idle);
  check(lastSaved() === 'Ctrl+F9', 'Enter can confirm a captured chord without mouse');
  check(calls('end_shortcut_capture').length >= calls('begin_shortcut_capture').length, 'every capture releases native suppression');
  window.testResult = {ok:true};
})().catch(error => { window.testResult = {ok:false, error:String(error), stack:error.stack}; });
"""
RESTORE = r"""
(async () => {
  for (let i = 0; i < 100 && document.querySelector('#dictate').disabled; ++i) await new Promise(r => setTimeout(r, 20));
  if (document.getElementById('setting-shortcut').value !== 'Ctrl+F9') throw new Error('shortcut not restored after reload');
  document.querySelector('#settings-toggle').click();
  window.testResult = {ok:true};
})().catch(error => { window.testResult = {ok:false, error:String(error)}; });
"""
NARROW = r"""
(() => {
  const field = document.getElementById('setting-shortcut').getBoundingClientRect();
  const button = document.querySelector('#record-shortcut').getBoundingClientRect();
  if (button.left < field.right || button.right > innerWidth || field.width < 49) throw new Error('narrow-window recorder overlaps or clips');
  window.testResult = {ok:true, width:innerWidth};
})();
"""


WIDE = r"""
try { (() => {
  const $ = selector => document.querySelector(selector);
  const main = $('main'), style = getComputedStyle(main);
  const left = main.getBoundingClientRect().left + main.clientLeft + parseFloat(style.paddingLeft);
  const available = main.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  const settings = $('#settings').getBoundingClientRect();
  if (available <= 900 || Math.abs(settings.width - 900) > 1) throw new Error('wide settings are not capped at 900 px');
  if (Math.abs(settings.left + settings.width / 2 - left - available / 2) > 1) throw new Error('wide settings are not centered');
  const groups = $('#settings-groups').getBoundingClientRect(), actions = $('.settings-actions').getBoundingClientRect();
  if (Math.abs(groups.left - actions.left) > 1 || Math.abs(groups.width - actions.width) > 1) throw new Error('settings groups/actions do not align');
  if (document.getElementById('setting-shortcut').getBoundingClientRect().width > 401) throw new Error('shortcut input stretches beyond 400 px');
  window.testResult = {ok:true, width:innerWidth, settingsWidth:settings.width};
})(); } catch (error) { window.testResult = {ok:false, error:String(error)}; }
"""


def evaluate(script):
    view.run_javascript(script, None, None, None)


def loaded(_view, event):
    if event == WebKit2.LoadEvent.FINISHED:
        evaluate(SCRIPT if stage == 0 else RESTORE)


def poll():
    def got(_view, result, _data):
        global failed, stage
        try:
            value = view.run_javascript_finish(result).get_js_value().to_string()
            if value == 'null':
                return
            outcome = json.loads(value)
            print(f"Shortcut stage {stage}: {outcome}", flush=True)
            if outcome['ok'] and stage == 0:
                stage = 1
                view.reload()
            elif outcome['ok'] and stage == 1:
                stage = 2
                evaluate('window.testResult = null')
                window.resize(400, 520)
                GLib.timeout_add(300, lambda: (evaluate(NARROW), False)[1])
            elif outcome['ok'] and stage == 2:
                stage = 3
                evaluate('window.testResult = null')
                window.resize(1400, 760)
                GLib.timeout_add(300, lambda: (evaluate(WIDE), False)[1])
            else:
                failed = not outcome['ok']
                Gtk.main_quit()
        except Exception as error:
            print(error, file=sys.stderr)
            Gtk.main_quit()
    view.run_javascript('JSON.stringify(window.testResult || null)', None, got, None)
    return True


view.connect('load-changed', loaded)
view.load_uri(url)
GLib.timeout_add(150, poll)
GLib.timeout_add_seconds(35, lambda: Gtk.main_quit())
try:
    Gtk.main()
finally:
    server.terminate()
    window.destroy()
sys.exit(1 if failed else 0)
