# Public network policy

This independently built library owns public listener IP selection, validated
GameStream base-port offsets, transport endpoint readback and non-reusing UDP
socket binding. It uses the workspace's existing serde and socket2 dependencies.
It does not authorize callers, alter management admission or enable remote access.

Omitted family and address retain a caller's legacy IP. Explicit family defaults
select loopbacks unless that caller independently enables remote listening.
Explicit IPs select one compatible family. IPv6 UDP sockets are v6-only so actual
media/control ownership matches the separate IPv4/IPv6 RTSP preflight.

Endpoint readback requires the peer's family, a nonzero actual port and the exact
requested concrete IP/fixed port. Wildcards allow same-family concrete routing;
omitted binds preserve local-peer loopback defaults. Per-session owners keep
responsibility for authorization, cancellation, leases and cleanup.

Default socket tests bind loopback and ephemeral ports. The real wildcard case
is separately ignored and requires an isolated network namespace. Linux test
success does not qualify Windows/macOS, installed hosts or real clients.
