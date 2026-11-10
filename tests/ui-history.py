"""WebKit integration checks. Run with /usr/bin/python3 tests/ui-history.py.
Uses only the isolated mock backend; no microphone or desktop input.
"""
import json
import subprocess
import sys
from pathlib import Path

import gi
gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gtk, WebKit2, GLib

root = Path(__file__).resolve().parent.parent
server = subprocess.Popen(["node", "tests/ui-preview.mjs"], cwd=root, stdout=subprocess.PIPE, text=True)
url = server.stdout.readline().strip().split()[-1]
window = Gtk.Window()
window.set_default_size(720, 520)
view = WebKit2.WebView()
window.add(view)
window.show_all()
failed = True
stage = 0

SCRIPT = r"""
(async () => {
  const $ = s => document.querySelector(s);
  const wait = ms => new Promise(r => setTimeout(r, ms));
  const check = (ok, text) => { if (!ok) throw new Error(text); };
  const until = async predicate => {
    for (let i = 0; i < 100; ++i) { if (predicate()) return; await wait(30); }
    throw new Error('Timed out waiting for UI');
  };
  await until(() => !$('#dictate').disabled);
  check($('#history-toggle').getAttribute('aria-pressed') === 'true', 'history enabled by default');
  const start = async () => { $('#dictate').click(); await until(() => $('#dictate').classList.contains('active') && !$('#dictate').disabled); };
  const stop = async () => { $('#dictate').click(); await until(() => !$('#dictate').classList.contains('active')); await wait(300); };
  const microphoneX = $('#dictate').getBoundingClientRect().x;
  const microphoneY = $('#dictate').getBoundingClientRect().y;
  await start();
  check($('#dictate').getBoundingClientRect().x === microphoneX, 'microphone does not shift when Cancel appears');
  check($('#dictate').getBoundingClientRect().y === microphoneY, 'microphone does not move vertically while recording');
  await stop();
  check($('#dictate').getBoundingClientRect().x === microphoneX, 'microphone does not shift when Cancel disappears');
  check($('#transcript-text').value === 'Test transcript.', 'final transcript');
  check(document.getElementById('setting-advanced.terminal_enabled'), 'terminal typing option is present');
  check($('#desktop-keyboard-status').textContent === 'Granted', 'concise keyboard permission status');
  check(!$('#group-delivery') && !$('#desktop-capability'), 'delivery sections merged without verbose block');
  check(document.getElementById('setting-advanced.desktop_enabled').checked && document.getElementById('setting-advanced.terminal_enabled').checked, 'external and terminal typing enabled by default');
  const stamp = $('#timestamp').dateTime;
  await start();
  await until(() => document.querySelectorAll('.history-bubble').length === 1);
  check($('#history-list time').dateTime === stamp, 'preserves original timestamp');
  check($('#history-list .history-text').hidden, 'history starts collapsed');
  $('#history-list .history-summary').click();
  check(!$('#history-list .history-text').hidden, 'history expands');
  $('#history-list .history-summary').click();
  check($('#history-list .history-text').hidden, 'history collapses');
  check($('#history-list .history-resume').disabled, 'cannot resume while recording');
  await stop();
  $('#history-list [title="Save this transcription to file"]').click();
  await wait(100);
  check(testEvents.some(e => e.name === 'save_transcript' && e.args.text === 'Test transcript.'), 'history save uses original text');
  $('#history-list .history-resume').click();
  await until(() => $('#transcript-text').value === 'Test transcript. Test transcript');
  await stop();
  // Current resume must extend in place, without creating another history row.
  const before = document.querySelectorAll('.history-bubble').length;
  $('#resume').click(); await until(() => $('#dictate').classList.contains('active') && !$('#dictate').disabled);
  await wait(150);
  check(document.querySelectorAll('.history-bubble').length === before, 'current resume is not duplicated');
  await stop();
  const original = $('#transcript-text').value;
  $('#resume').click(); await until(() => $('#dictate').classList.contains('active') && !$('#dictate').disabled);
  $('#cancel').click(); await until(() => !$('#dictate').classList.contains('active'));
  check($('#transcript-text').value.trim() === original.trim(), 'cancelling resume preserves existing transcript');
  // Start through the global shortcut event path, rather than the button.
  testEmit('session-event', {session_id:100, kind:'state', state:'loading'});
  testEmit('session-event', {session_id:100, kind:'state', state:'listening'});
  testEmit('session-event', {session_id:100, kind:'interim', text:'Shortcut transcription'});
  testEmit('session-event', {session_id:100, kind:'final', text:'Shortcut transcription.'});
  testEmit('session-event', {session_id:100, kind:'state', state:'complete'});
  await wait(400);
  check(document.querySelectorAll('.history-bubble').length === before + 1, 'shortcut archives previous text');
  $('#history-toggle').click();
  check($('#history-panel').hidden, 'history toggle hides panel');
  $('#history-toggle').click();
  check(!$('#history-panel').hidden, 'history toggle restores panel');
  // Exercise IndexedDB retention and paging independently of the mock recognizer.
  const store = await import('/history.js');
  for (let i = 0; i < 110; ++i) await store.archiveTranscript({text:`Historical entry ${i} ` + 'long text '.repeat(30), date:new Date().toISOString()}, 0);
  await store.saveDraft({text:'Restored current draft', date:stamp});
  $('#history-more').hidden = false;
  $('#history-more').click();
  await until(() => document.querySelectorAll('.history-bubble').length === 100);
  $('#transcript-text').value = 'A long current transcription.\\n'.repeat(100);
  $('#transcript-text').dispatchEvent(new Event('input', {bubbles:true}));
  const current = $('.active-transcript').getBoundingClientRect();
  const record = $('#dictate').getBoundingClientRect();
  const textarea = $('#transcript-text');
  check(textarea.clientHeight >= parseFloat(getComputedStyle(textarea).lineHeight) * 4 - 1, 'at least four active lines');
  check(current.bottom <= record.top && record.bottom <= innerHeight, 'active text and record stay visible');
  check(textarea.scrollHeight > textarea.clientHeight, 'long active text scrolls inside its pinned card');
  const top = current.top;
  $('#history-panel').scrollTop = 0;
  check($('.active-transcript').getBoundingClientRect().top === top, 'scrolling history does not move active text');
  check($('#history-list .history-summary').scrollWidth > $('#history-list .history-summary').clientWidth, 'long summaries overflow for ellipsis');
  // Restore a stable draft for the reload check, rather than the large layout fixture.
  $('#transcript-text').value = 'Restored current draft';
  $('#transcript-text').dispatchEvent(new Event('input', {bubbles:true}));
  await wait(350);
  await store.trimHistory(3);
  check((await store.loadHistory(100)).entries.length === 3, 'retention trims old entries');
  check((await store.loadHistory(100)).entries[2].text.startsWith('Historical entry 109 '), 'retention keeps newest');
  const select = document.getElementById('setting-history_limit');
  select.value = '3'; select.dispatchEvent(new Event('change', {bubbles:true}));
  await wait(500);
  check(testEvents.some(e => e.name === 'save_settings' && e.args.settings.history_limit === 3), 'history limit setting saved');
  window.testResult = {ok:true};
})().catch(error => { window.testResult = {ok:false, error:String(error), stack:error.stack}; });
"""
RESTORE = r"""
(async () => {
  for (let i = 0; i < 100 && document.querySelector('#dictate').disabled; ++i) await new Promise(r => setTimeout(r, 30));
  if (document.querySelector('#transcript-text').value !== 'Restored current draft') throw new Error('draft not restored after reload');
  if (document.querySelectorAll('.history-bubble').length !== 3) throw new Error('history not restored after reload');
  window.testResult = {ok:true};
})().catch(error => { window.testResult = {ok:false, error:String(error)}; });
"""

def loaded(_view, event):
    if event == WebKit2.LoadEvent.FINISHED:
        view.run_javascript(SCRIPT if stage == 0 else RESTORE, None, None, None)


def poll():
    def got(_view, result, _data):
        global failed, stage
        try:
            value = view.run_javascript_finish(result).get_js_value().to_string()
            if value == 'null':
                return
            outcome = json.loads(value)
            print(f"Stage {stage}: {outcome}", flush=True)
            if outcome['ok'] and stage == 0:
                stage = 1
                view.reload()
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
GLib.timeout_add(200, poll)
GLib.timeout_add_seconds(40, lambda: Gtk.main_quit())
try:
    Gtk.main()
finally:
    server.terminate()
    window.destroy()
sys.exit(1 if failed else 0)
