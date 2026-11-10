"""Observational UX probes, not acceptance tests that require bugs to remain.
Uses an isolated WebKit/mock profile: no real files saved, mic, or desktop input.
"""
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
window = Gtk.Window(title="Arxell isolated usability probes")
window.set_default_size(720, 520)
view = WebKit2.WebView()
window.add(view)
window.show_all()
failed = True
stage = 0

SCRIPT = r"""
(async () => {
  const $ = selector => document.querySelector(selector);
  const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
  const until = async predicate => {
    for (let i = 0; i < 100; ++i) { if (predicate()) return; await wait(20); }
    throw new Error('Probe timed out');
  };
  const calls = name => testEvents.filter(event => event.name === name);
  await until(() => !$('#dictate').disabled);
  const findings = {};
  findings.startEnabledWithoutReady = !$('#dictate').disabled && $('#state-label').getAttribute('aria-label') === 'No model loaded';
  $('#settings-toggle').click();
  const preloadsBeforeVerify = calls('preload_model').length;
  $('#verify').click();
  await until(() => calls('verify_model_file').length && !$('#verify').disabled);
  findings.verifyRequestsModelLoad = calls('preload_model').length > preloadsBeforeVerify;

  const store = await import('/history.js');
  const retention = document.getElementById('setting-history_limit');
  retention.value = '0'; retention.dispatchEvent(new Event('change', {bubbles:true}));
  await wait(350);
  await store.clearHistory();
  for (let i = 0; i < 20; ++i) await store.archiveTranscript({text:`UX probe entry ${i}`, date:new Date().toISOString()}, 0);
  let confirmation = '';
  window.confirm = text => { confirmation = text; return true; };
  $('#reset').click();
  await until(() => calls('reset_settings').length && retention.value === '10');
  await wait(200);
  findings.restoreDefaults = {before:20, after:(await store.loadHistory(100)).entries.length, confirmation};

  $('#settings-toggle').click();
  const RealDate = Date;
  window.Date = class extends RealDate { constructor(...args) { super(...(args.length ? args : ['2026-11-05T20:00:00.000Z'])); } };
  $('#transcript-text').value = 'First different export'; $('#transcript-text').dispatchEvent(new Event('input', {bubbles:true})); $('#save').click();
  await until(() => calls('save_transcript').length === 1);
  $('#transcript-text').value = 'Second different export'; $('#transcript-text').dispatchEvent(new Event('input', {bubbles:true})); $('#save').click();
  await until(() => calls('save_transcript').length === 2);
  findings.sameSecondExports = calls('save_transcript').map(event => ({filename:event.args.filename, text:event.args.text}));
  window.Date = RealDate;

  $('#dictate').click();
  await until(() => $('#dictate').classList.contains('active') && !$('#dictate').disabled);
  await wait(400);
  findings.liveDraft = {visible:$('#transcript-text').value, persisted:(await store.loadDraft())?.text ?? null};
  $('#dictate').click();
  await until(() => !$('#dictate').classList.contains('active'));
  await wait(400);
  findings.completedDraft = (await store.loadDraft())?.text;
  $('#settings-toggle').click();
  window.probeFindings = findings;
  window.testResult = {ok:true, findings};
})().catch(error => { window.testResult = {ok:false, error:String(error), stack:error.stack}; });
"""
NARROW = r"""
(() => {
  for (const toggle of document.querySelectorAll('.advanced-toggle')) {
    if (toggle.getAttribute('aria-expanded') === 'false') toggle.click();
  }
  const clipped = [];
  for (const element of document.querySelectorAll('.setting-row input, .setting-row select, .setting-row button')) {
    const bounds = element.getBoundingClientRect();
    const card = element.closest('.card').getBoundingClientRect();
    if (bounds.width && bounds.right > card.right + 1) clipped.push({id:element.id, overflow:Math.round(bounds.right-card.right)});
  }
  window.testResult = {ok:true, width:innerWidth, clippedControls:clipped};
})();
"""


def evaluate(script):
    view.run_javascript(script, None, None, None)


def loaded(_view, event):
    if event == WebKit2.LoadEvent.FINISHED:
        evaluate(SCRIPT)


def poll():
    def got(_view, result, _data):
        global failed, stage
        try:
            value = view.run_javascript_finish(result).get_js_value().to_string()
            if value == 'null':
                return
            outcome = json.loads(value)
            print(json.dumps({'stage':stage, **outcome}, indent=2), flush=True)
            if outcome['ok'] and stage == 0:
                stage = 1
                evaluate('window.testResult = null')
                window.resize(400, 520)
                GLib.timeout_add(300, lambda: (evaluate(NARROW), False)[1])
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
GLib.timeout_add_seconds(30, lambda: Gtk.main_quit())
try:
    Gtk.main()
finally:
    server.terminate()
    window.destroy()
sys.exit(1 if failed else 0)
