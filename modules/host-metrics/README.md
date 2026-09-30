# Host metrics module

`amaterasu.host-metrics/1` owns the real-time statistics preferences, measured
host snapshots, and a bounded in-memory chart history. Settings live in the
module's revisioned `amaterasu.configuration/1` product document. The service
does not synthesize samples: it records only values read from its platform
provider while collection is enabled and a dashboard or active stream holds a
consumer lease.

Management operations are `settings_get {}`, `settings_update {settings,
expected_revision}`, and `stats_read {}`. Settings updates use configuration
compare-and-swap revisions. Internal `stream_activity_begin {session_id}` and
`stream_activity_end {session_id}` operations are restricted to
`amaterasu.stream-session` and keep sampling active for actual streams even
when no dashboard is open.

Defaults are enabled, 2,000 ms polling, 300 seconds of live chart retention,
300 points, pause after a hidden dashboard lease expires, and all four display
sections enabled. Poll intervals are 250 to 60,000 ms, retention is 30 to
3,600 seconds, and the point limit is 30 to 2,000. The live history is
intentionally in memory; persisted stream history is owned by
`amaterasu.session-history/1`.

Linux sampling reads `/proc/stat` for CPU utilization, `/proc/meminfo` for
memory use, and aggregates non-loopback counters from `/proc/net/dev` for
network rates. CPU temperature is read only from recognized CPU thermal or
hwmon sensors. GPU utilization, VRAM, and temperature use DRM device sysfs
attributes when the driver exposes them (currently the AMD DRM interfaces on
common Linux drivers). No generic Linux interface reports encoder utilization,
so that field remains unavailable. CPU and network rates are also unavailable
until two source readings establish a real delta.

On Windows and macOS this provider reports `available: false`, a platform value
of `unsupported`, and null metrics. Each unsupported or unreadable Linux metric
is independently null; a missing sensor never becomes a zero reading.
