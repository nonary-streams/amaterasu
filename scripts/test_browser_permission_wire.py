#!/usr/bin/env python3
"""Probe a signed browser fixture's paired TLS application permissions."""
import sys
from test_nvhttp import get, tls_context

base, expected = sys.argv[1:]
assert base.startswith("https://127.0.0.1:"), "probe must use loopback"
context = tls_context()
for path in ("/applist", "/launch"):
    status, root = get(base, path, {"uniqueid": "signed-browser-fixture"}, context)
    if expected == "denied":
        assert status == 403 and root.get("status_code") == "403", (path, status, root.attrib)
    elif path == "/applist":
        assert status == 200 and root.get("status_code") == "200", (path, status, root.attrib)
    else:
        # This deliberately incomplete launch is rejected by request validation,
        # proving it passed the permission check without starting a stream.
        assert status == 400 and root.get("status_code") == "400", (path, status, root.attrib)
