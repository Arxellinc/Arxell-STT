"""Opt-in native Linux delivery smoke test against an isolated GTK text field.

Build tests first: ./dev-build.sh test
Run: /usr/bin/python3 tests/desktop-field.py

This opens a temporary window. Only an AT-SPI object with this run's random
accessible name can receive the fixed synthetic text. No microphone, keyboard
injection, clipboard, terminal, agent, or arbitrary destination is used.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import uuid

import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GLib

root = Path(__file__).resolve().parent.parent
binaries = sorted((root / '.local/target/debug/deps').glob('arxell_speak_lib-*'), key=lambda p: p.stat().st_mtime, reverse=True)
binary = next((p for p in binaries if p.is_file() and os.access(p, os.X_OK)), None)
if binary is None:
    sys.exit('Run ./dev-build.sh test first.')

window = Gtk.Window(title='Arxell isolated desktop typing test — synthetic input only')
window.set_default_size(600, 160)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
box.set_border_width(16)
window.add(box)
box.pack_start(Gtk.Label(label='Isolated AT-SPI test. No global keyboard input is sent.'), False, False, 0)
entry = Gtk.Entry()
other = Gtk.Entry()
other.set_placeholder_text('Focus moves here to verify typing stops')
box.pack_start(entry, False, False, 0)
box.pack_start(other, False, False, 0)
token = 'arxell-test-' + uuid.uuid4().hex
entry.get_accessible().set_name(token)
window.show_all()
entry.grab_focus()
window.present()
work = tempfile.TemporaryDirectory(prefix='arxell-desktop-fixture-')
stage = Path(work.name) / 'stage'
env = dict(os.environ, ARXELL_DESKTOP_TEST_TOKEN=token, ARXELL_DESKTOP_TEST_STAGE=str(stage))
log = open(Path(work.name) / 'result.log', 'w+')
process = None
failed = True


def start():
    global process
    process = subprocess.Popen([str(binary), 'native_delivery_to_named_fixture', '--ignored', '--nocapture'], env=env, stdout=log, stderr=subprocess.STDOUT)
    return False


def poll():
    global failed
    if stage.exists() and stage.read_text() == 'typed':
        if entry.get_text() != 'Héllo 世界 + streaming':
            print('Unexpected fixture contents:', repr(entry.get_text()))
            process.terminate()
            Gtk.main_quit()
            return False
        other.grab_focus()
        stage.write_text('focus-changed')
    if process and process.poll() is not None:
        log.seek(0)
        print(log.read())
        failed = process.returncode != 0 or other.get_text() != ''
        Gtk.main_quit()
        return False
    return True

GLib.timeout_add(500, start)
GLib.timeout_add(30, poll)
GLib.timeout_add_seconds(25, lambda: Gtk.main_quit())
try:
    Gtk.main()
finally:
    if process and process.poll() is None:
        process.terminate()
    window.destroy()
    log.close()
    work.cleanup()
sys.exit(1 if failed else 0)
