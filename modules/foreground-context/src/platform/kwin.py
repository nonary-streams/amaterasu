# Read-only transient KWin observation with recoverable cleanup obligations.
import fcntl
import json
import os
import re
import stat
import uuid
import dbus
import dbus.service
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib

runtime = os.environ['XDG_RUNTIME_DIR']
info = os.stat(runtime, follow_symlinks=False)
if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
    raise RuntimeError('unsafe runtime directory')
lock_path = os.path.join(runtime, 'amaterasu-foreground.lock')
lock = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
kwin_owner = bus.get_name_owner('org.kde.KWin')
scripting = dbus.Interface(bus.get_object('org.kde.KWin', '/Scripting'), 'org.kde.kwin.Scripting')
pattern = re.compile(r'amaterasu-foreground-[0-9a-f]{32}\.pending\Z')

def reconcile():
    for entry in os.scandir(runtime):
        if not pattern.fullmatch(entry.name):
            continue
        entry_info = entry.stat(follow_symlinks=False)
        if not stat.S_ISREG(entry_info.st_mode) or entry_info.st_uid != os.getuid() or entry_info.st_mode & 0o077:
            raise RuntimeError('unsafe cleanup obligation')
        name = entry.name[:-len('.pending')]
        if scripting.isScriptLoaded(name, timeout=0.1):
            scripting.unloadScript(name, timeout=0.1)
        if scripting.isScriptLoaded(name, timeout=0.1):
            raise RuntimeError('script cleanup pending')
        try:
            os.unlink(os.path.join(runtime, name + '.js'))
        except FileNotFoundError:
            pass
        os.unlink(entry.path)

reconcile()
plugin = 'amaterasu-foreground-' + uuid.uuid4().hex
marker = os.path.join(runtime, plugin + '.pending')
path = os.path.join(runtime, plugin + '.js')
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
    with os.fdopen(os.open(marker, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as obligation:
        obligation.write('pending\n')
        obligation.flush()
        os.fsync(obligation.fileno())
    with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600), 'w') as source:
        source.write(script)
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
