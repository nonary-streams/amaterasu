#!/usr/bin/env python3
"""Signed frontend activation while the native supervisor and worker keep running."""
from __future__ import annotations

import http.client
import json
from pathlib import Path
import re
import shutil
import tempfile

from test_component_updates import CARGO, WORKSPACE, file_hash, run
from test_host_service import Console, worker_pid
from test_management_api import Api
from test_platform_modules import PlatformHarness


def get(api, path):
    connection = http.client.HTTPConnection(api.host, api.port, timeout=12)
    try:
        connection.request("GET", path)
        response = connection.getresponse()
        return response.status, response.read(), dict(response.getheaders())
    finally:
        connection.close()


def base_path(body):
    match = re.search(rb'<base href="(/web/[0-9a-f]{64}/)">', body)
    assert match, body[:400]
    return match[1].decode()


def main():
    run(CARGO, "build", "--workspace", "--offline")
    with tempfile.TemporaryDirectory(prefix="amaterasu-web-") as temporary:
        directory = Path(temporary)
        harness = PlatformHarness(directory)
        bootstrap_path = harness.root / "bootstrap.json"
        bootstrap = json.loads(bootstrap_path.read_text())
        bootstrap["policy"]["settings"]["amaterasu.management"].update(enabled=True, bind="127.0.0.1:0")
        bootstrap_path.write_text(json.dumps(bootstrap))
        harness.stage(harness.package(1))
        first = directory / "web-1"
        run(harness.publisher, "pack-web", harness.root, WORKSPACE / "frontend/dist", first, "1")
        run(harness.publisher, "stage-web", harness.root, first)
        shell_hash = file_hash(harness.host)
        console = Console(harness)
        try:
            api = Api(console)
            original_worker = worker_pid(console)
            status, html, headers = get(api, "/")
            assert status == 200 and "script-src 'self'" in headers["Content-Security-Policy"]
            base = base_path(html)
            status, original_js, headers = get(api, base + "app.js")
            assert status == 200 and headers["X-Content-Type-Options"] == "nosniff"
            assert get(api, base + "styles.css")[0] == 200
            assert get(api, "/bootstrap.json")[0] == 404
            assert get(api, base + "../manifest.json")[0] == 404
            assert api.post("/v1/auth/login", origin=f"http://{api.host}:{api.port}")[0] != 403
            source = directory / "updated-dist"
            shutil.copytree(WORKSPACE / "frontend/dist", source)
            with (source / "app.js").open("a") as output:
                output.write("\n// authenticated frontend update fixture\n")
            second = directory / "web-2"
            run(harness.publisher, "pack-web", harness.root, source, second, "2")
            run(harness.publisher, "stage-web", harness.root, second)
            status, updated_html, _ = get(api, "/")
            updated_base = base_path(updated_html)
            assert status == 200 and updated_base != base
            assert get(api, updated_base + "app.js")[1] != original_js
            assert get(api, base + "app.js")[1] == original_js
            run(harness.publisher, "stage-web", harness.root, first, expect_success=False)
            (second / "app.js").write_text("unapproved bytes")
            run(harness.publisher, "stage-web", harness.root, second, expect_success=False)
            assert base_path(get(api, "/")[1]) == updated_base
            assert worker_pid(console) == original_worker and file_hash(harness.host) == shell_hash
            console.eof()
        finally:
            console.close()
    print("Passed: signed live web updates, pinned assets, CSP, same-origin API, traversal/tamper/rollback rejection; unchanged shell and worker.")


if __name__ == "__main__":
    main()
