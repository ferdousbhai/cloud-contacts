"""Exercise native windows on a private Broadway display and private D-Bus, never the owner's desktop."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

if not shutil.which('gtk4-broadwayd') or not shutil.which('dbus-run-session'):
    raise SystemExit('Native test needs gtk4-broadwayd and dbus-run-session')
root = Path(__file__).resolve().parent.parent
with tempfile.TemporaryDirectory(prefix='cloud-contacts-ui-') as tmp:
    env = os.environ.copy()
    env.update({'HOME': tmp, 'XDG_RUNTIME_DIR': tmp, 'XDG_CONFIG_HOME': tmp,
                'XDG_DATA_HOME': tmp, 'GDK_BACKEND': 'broadway', 'BROADWAY_DISPLAY': ':97',
                'GTK_A11Y': 'none', 'GTK_USE_PORTAL': '0', 'GIO_USE_VFS': 'local', 'GSETTINGS_BACKEND': 'memory', 'PYTHONPATH': str(root)})
    env.pop('DISPLAY', None)
    env.pop('WAYLAND_DISPLAY', None)
    server = subprocess.Popen(['gtk4-broadwayd', '--unixsocket', str(Path(tmp) / 'web.socket'), ':97'],
                              env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        deadline = time.monotonic() + 5
        while not list(Path(tmp).glob('broadway*.socket')) and time.monotonic() < deadline:
            if server.poll() is not None:
                raise RuntimeError(server.stderr.read().decode())
            time.sleep(.02)
        code = r'''import gi,time
from pathlib import Path
gi.require_version('Gtk','4.0')
gi.require_version('Adw','1')
from gi.repository import Gtk,Adw,GLib
from cloud_contacts.app import Window
class Fake:
 product='cloud-contacts'
 path=Path('/scratch/config.json')
 def __init__(self): self.edits=[]
 def accounts(self): return {'icloud':{'provider':'icloud'}}
 def list(self,**kwargs):
  if self.product=='cloud-contacts':
   return ([{'id':'icloud:a','account':'icloud','name':'Fixture Person','emails':[],'phones':[]},
            {'id':'icloud:b','account':'icloud','name':'Second Person','emails':[],'phones':[]}],[])
  return ([{'id':'icloud:task','account':'icloud','title':'Fixture Task','list':'Reminders','completed':False,'notes':'Fixture notes','due':None}],[])
 def lists(self): return ([{'id':'icloud:List%2Fid','account':'icloud','name':'Reminders'}],[])
 def edit(self,*args,**kwargs): self.edits.append((args,kwargs)); return {}
 def candidates(self): return ([],[])
fake=Fake()
app=Adw.Application(application_id='com.ferdousbhai.test')
app.register(None)
w=Window(app,fake);w.present()
ctx=GLib.MainContext.default()
def pump():
 for _ in range(35):
  while ctx.pending(): ctx.iteration(False)
  time.sleep(.01)
pump()
assert w.listbox.get_first_child() is not None
row=w.listbox.get_first_child();w.listbox.select_row(row)
w.edit();w.details(w.rows[0]);w.accounts();w.delete();w.new();pump()
if fake.product=='cloud-contacts':
 w.listbox.select_row(w.listbox.get_last_child());w.merge()
else:
 check=w.listbox.get_first_child().get_child().get_first_child()
 check.set_active(True);pump()
 assert fake.edits[0][1]['completion'] is True
for window in Gtk.Window.list_toplevels(): window.close()
pump()
print('Native cloud-contacts workflows passed on private Broadway + D-Bus')
'''
        result = subprocess.run(['dbus-run-session', '--', 'python3', '-c', code], env=env, cwd=root,
                                text=True, capture_output=True, timeout=25)
        print(result.stdout, end='')
        if result.returncode:
            print(result.stderr)
            raise SystemExit(result.returncode)
    finally:
        server.terminate()
        server.wait(timeout=5)
