#!/usr/bin/env python3
"""Qualify WebRTC in a private network namespace with loopback only."""
import os
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from test_component_updates import CARGO, WORKSPACE, run
from test_signed_browser_session import OwnedBrowser


def main():
    chromium = shutil.which("chromium") or shutil.which("chromium-browser")
    node = shutil.which("node")
    unshare = shutil.which("unshare")
    ip = shutil.which("ip")
    if not chromium or not node or not unshare or not ip:
        raise RuntimeError("Chromium, Node, unshare and ip are required")
    if sys.argv[1:] != ["--isolated"]:
        if sys.argv[1:]:
            raise RuntimeError("Unexpected argument")
        run(CARGO, "build", "--locked", "--offline", "-p", "amaterasu-webrtc-transport", "--example", "browser-loopback", "--example", "session-loopback")
        environment = dict(os.environ, AMATERASU_TEST_PARENT_NET=os.readlink("/proc/self/ns/net"))
        subprocess.run([unshare, "--user", "--map-root-user", "--net", sys.executable, __file__, "--isolated"],
                       check=True, env=environment)
        return
    parent_network = os.environ.pop("AMATERASU_TEST_PARENT_NET", None)
    if not parent_network or os.readlink("/proc/self/ns/net") == parent_network:
        raise RuntimeError("WebRTC qualification requires a private network namespace")
    subprocess.run([ip, "link", "set", "lo", "up"], check=True)
    links = json.loads(subprocess.check_output([ip, "-json", "link", "show"]))
    if {link["ifname"] for link in links} != {"lo"}:
        raise RuntimeError("WebRTC qualification requires a loopback-only namespace")
    with tempfile.TemporaryDirectory(prefix="amaterasu-webrtc-browser-") as directory:
        # Namespace root maps to the invoking user on the host. Chromium requires
        # --no-sandbox there; its network namespace contains only loopback.
        browser = OwnedBrowser(Path(directory) / "browser", chromium, "",
                               extra_args=("--no-sandbox", "--allow-loopback-in-peer-connection",
                                           "--disable-features=WebRtcHideLocalIpsWithMdns"))
        try:
            debugger = browser.start()
            subprocess.run([node, str(WORKSPACE / "scripts/test_webrtc_browser.mjs"), debugger,
                str(WORKSPACE / "target/debug/examples/browser-loopback")], check=True, timeout=40)
            subprocess.run([node, str(WORKSPACE / "scripts/test_webrtc_browser.mjs"), debugger,
                str(WORKSPACE / "target/debug/examples/session-loopback"), "session"], check=True, timeout=40)
        finally:
            browser.close()
    print("Not qualified: signed host integration, hardware capture, remote networks, or game input.")


if __name__ == "__main__":
    main()
