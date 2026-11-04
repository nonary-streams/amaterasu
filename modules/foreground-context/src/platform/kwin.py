# Read-only transient KWin observation with recoverable cleanup obligations.
import fcntl
import json
import os
import stat
import sys
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

runtime = os.environ['XDG_RUNTIME_DIR']
info = os.stat(runtime, follow_symlinks=False)
if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
    raise RuntimeError('unsafe runtime directory')
plugin = 'amaterasu-foreground-observer'
lock_path = os.path.join(runtime, plugin + '.lock')
marker = os.path.join(runtime, plugin + '.pending')
path = os.path.join(runtime, plugin + '.js')

def owned_file(path, flags):
    fd = os.open(path, flags | os.O_NOFOLLOW, 0o600)
    info = os.fstat(fd)
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        os.close(fd)
        raise RuntimeError('unsafe observer file')
    return fd

lock = owned_file(lock_path, os.O_CREAT | os.O_RDWR)
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
# This sentinel is permanent: a late load can arrive after a negative query.
# Never delete it, even after successful cleanup.
with os.fdopen(owned_file(marker, os.O_CREAT | os.O_RDWR), 'r+') as obligation:
    obligation.write('cleanup required on every invocation\n')
    obligation.flush()
    os.fsync(obligation.fileno())
DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
kwin_owner = bus.get_name_owner('org.kde.KWin')
scripting = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')

def reconcile():
    if scripting.isScriptLoaded(plugin, timeout=0.1):
        scripting.unloadScript(plugin, timeout=0.1)
    if scripting.isScriptLoaded(plugin, timeout=0.1):
        raise RuntimeError('script cleanup pending')

reconcile()
if len(sys.argv) == 2 and sys.argv[1] == '--reconcile':
    sys.exit(0)
loop = GLib.MainLoop()
result = None

class Receiver(dbus.service.Object):
    @dbus.service.method('org.amaterasu.ForegroundObservation', in_signature='ib', out_signature='', sender_keyword='sender')
    def report(self, pid, fullscreen, sender=None):
        global result
        if sender == kwin_owner and int(pid) > 0:
            result = {'pid': int(pid), 'fullscreen': bool(fullscreen)}
            loop.quit()

receiver = Receiver(bus, '/org/amaterasu/ForegroundObservation')
script = '''
try {
    var window = workspace.activeWindow;
    if (window && window.pid > 0) {
        callDBus(%s, '/org/amaterasu/ForegroundObservation', 'org.amaterasu.ForegroundObservation', 'report', window.pid, window.fullScreen);
    }
} finally {
    callDBus('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', %s);
}
''' % (json.dumps(str(bus.get_unique_name())), json.dumps(plugin))
try:
    with os.fdopen(owned_file(path, os.O_CREAT | os.O_RDWR), 'r+') as source:
        source.seek(0)
        source.truncate()
        source.write(script)
        source.flush()
    script_id = bus.call_blocking('org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss', (path, plugin), timeout=0.1)
    if int(script_id) >= 0:
        instance = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting/Script' + str(script_id)), 'org.kde.kwin.Script')
        instance.run(timeout=0.1)
        GLib.timeout_add(100, lambda: (loop.quit(), False)[1])
        loop.run()
finally:
    try:
        reconcile()
    except Exception:
        # The marker remains for the next observer invocation.
        result = None
print(json.dumps(result, separators=(',', ':')))
