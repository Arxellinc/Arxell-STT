#!/usr/bin/python3
"""Disposable manual dictation target. No saving, submission, or global input.

Launch beside the release-candidate app. The user focuses a field and starts
Arxell with its configured shortcut. Only character counts are logged, never
field contents. Closing this window destroys its temporary text.
"""
import os
import gi

gi.require_version('Gtk', '3.0')
from gi.repository import Gtk

version = os.environ.get('ARXELL_TEST_VERSION', '0.1.2-rc.2')
window = Gtk.Window(title=f'Arxell TEST TARGET — {version}')
window.set_default_size(680, 260)
window.connect('destroy', Gtk.main_quit)
box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
box.set_border_width(18)
window.add(box)
heading = Gtk.Label(label='Disposable dictation test — nothing here can run or submit a command')
heading.set_line_wrap(True)
box.pack_start(heading, False, False, 0)
fields = []
for index, label in enumerate(['Dictation test field: click here, then press Ctrl+Space', 'Second field: moving here should stop the previous delivery']):
    caption = Gtk.Label(label=label)
    caption.set_xalign(0)
    box.pack_start(caption, False, False, 0)
    field = Gtk.Entry()
    field.get_accessible().set_name(f'Arxell disposable dictation field {index + 1}')
    field.connect('changed', lambda entry, number=index + 1: print(f'test target: field={number} characters={len(entry.get_text())}', flush=True))
    box.pack_start(field, False, False, 0)
    fields.append(field)
clear = Gtk.Button(label='Clear disposable fields')
def clear_fields(_button):
    for field in fields:
        field.set_text('')
    fields[0].grab_focus()
clear.connect('clicked', clear_fields)
box.pack_start(clear, False, False, 0)
window.show_all()
fields[0].grab_focus()
window.present()
Gtk.main()
