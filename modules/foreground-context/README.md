# Foreground context provider

`amaterasu.foreground-context/1` supplies an observed executable path and,
when known, fullscreen state. Trusted composition must select this provider
and set its `allowed_principals`; missing or unverified observations return
an unavailable snapshot. Consumers must enforce their own freshness and policy.

On KDE Wayland, the provider uses KWin's scripting interface for a read-only
observation. It requires `python3`, `dbus-python`, and PyGObject in the host
session. It retains a user-owned cleanup sentinel under `XDG_RUNTIME_DIR` and
reconciles its single script name on startup and future observations. A stopped
or unresponsive compositor can defer cleanup; a delayed load can leave at most
one script until reconciliation. X11 requires `xprop` and a local X11 session;
Xwayland cannot establish the native Wayland foreground. Unsupported sessions,
missing dependencies, failed queries, and inconsistent process identities
return unavailable. Windows is currently unavailable.

Executable identity is the observed process executable, which may be a
launcher or Wine executable rather than a configured game. The snapshot does
not prove application identity or successful presentation; consumers should
handle a mismatch as unknown for their intended policy.
