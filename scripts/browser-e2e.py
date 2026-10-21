#!/usr/bin/env python3
"""Browser acceptance tests for the uniblox client, driven over WebDriver (standard library only).

Run by `just browser-e2e` inside `nix develop .#browser`, after `just wasm` has built dist/. It starts
the signaling service, a static server for dist/ and chromedriver, then checks three paths in headless
Chromium (WebGL2 through SwiftShader):

1. first frame: the page renders a Bevy frame (`[uniblox-metrics] first-frame`);
2. save and reload: K saves the world to IndexedDB, the page reloads, L loads the same content ID
   (`[uniblox-save] saved <id>` then `[uniblox-save] loaded <id>`);
3. two tabs: two browsers in the same signaling room each receive the other's packets on both
   channels (`[uniblox-demo][STATE] from` and `[uniblox-demo][EVENT] from`).

Environment: CHROMEDRIVER and UNIBLOX_CHROMIUM (set by the dev shell), UNIBLOX_SERVICES (the
signaling binary; default: `cargo run --locked -p services`), UNIBLOX_DIST (default dist).
Exit 0 = all three pass.
"""

import contextlib
import functools
import http.server
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

TIMEOUT_S = 60
SIGNALING_PORT = (
    3536  # the client's compiled-in ROOM_URL is ws://127.0.0.1:3536/uniblox-demo
)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for_port(port, deadline):
    while time.monotonic() < deadline:
        with (
            contextlib.suppress(OSError),
            socket.create_connection(("127.0.0.1", port), timeout=1),
        ):
            return
        time.sleep(0.2)
    raise TimeoutError(f"nothing listened on port {port}")


class WebDriver:
    """The handful of W3C WebDriver calls the tests need."""

    def __init__(self, base):
        self.base = base

    def call(self, method, path, body=None):
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(
            self.base + path,
            method=method,
            data=data,
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.load(resp)["value"]
        except urllib.error.HTTPError as err:
            raise RuntimeError(
                f"{method} {path}: {err.code} {err.read()[:400]!r}"
            ) from None

    def session(self, chromium, profile):
        caps = {
            "capabilities": {
                "alwaysMatch": {
                    "browserName": "chrome",
                    "goog:loggingPrefs": {"browser": "ALL"},
                    "goog:chromeOptions": {
                        "binary": chromium,
                        "args": [
                            "--headless=new",
                            "--no-sandbox",
                            "--disable-dev-shm-usage",
                            "--window-size=960,640",
                            f"--user-data-dir={profile}",
                            # WebGL2 without a GPU.
                            "--use-angle=swiftshader",
                            "--enable-unsafe-swiftshader",
                            # Automated WebRTC: real host candidates (mDNS names do not resolve headlessly) and loopback.
                            "--disable-features=WebRtcHideLocalIpsWithMdns",
                            "--allow-loopback-in-peer-connection",
                        ],
                    },
                }
            }
        }
        return Session(self, self.call("POST", "/session", caps)["sessionId"])


class Session:
    def __init__(self, driver, sid):
        self.driver, self.sid, self.lines = driver, sid, []

    def call(self, method, path, body=None):
        return self.driver.call(method, f"/session/{self.sid}{path}", body)

    def goto(self, url):
        self.call("POST", "/url", {"url": url})

    def poll_console(self):
        for entry in self.call("POST", "/se/log", {"type": "browser"}):
            self.lines.append(entry["message"])
        return self.lines

    def wait_for(self, predicate, what, deadline):
        while time.monotonic() < deadline:
            for line in self.poll_console():
                found = predicate(line)
                if found:
                    return found
            time.sleep(0.25)
        raise TimeoutError(f"timed out waiting for {what}")

    def press(self, key):
        """A key press on the canvas, which Bevy's winit backend reads keyboard input from (so focus it first)."""
        canvas = self.call(
            "POST", "/element", {"using": "css selector", "value": "canvas"}
        )
        self.call("POST", f"/element/{next(iter(canvas.values()))}/click", {})
        self.call(
            "POST",
            "/actions",
            {
                "actions": [
                    {
                        "type": "key",
                        "id": "keyboard",
                        "actions": [
                            {"type": "keyDown", "value": key},
                            {"type": "pause", "duration": 100},
                            {"type": "keyUp", "value": key},
                        ],
                    }
                ]
            },
        )
        self.call("DELETE", "/actions")

    def close(self):
        with contextlib.suppress(Exception):
            self.call("DELETE", "")


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    """The static server for dist/, without a log line per request."""

    def log_message(self, *args):
        pass


def marker(text):
    return lambda line: text in line and line


def saved_id(line):
    tag = "[uniblox-save] saved "
    if tag in line:
        return line.split(tag, 1)[1].split()[0].strip('"\\')
    return None


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    dist = os.path.join(root, os.environ.get("UNIBLOX_DIST", "dist"))
    chromium = os.environ["UNIBLOX_CHROMIUM"]
    chromedriver = os.environ["CHROMEDRIVER"]
    if not os.path.isfile(os.path.join(dist, "index.html")):
        sys.exit(f"{dist}/index.html is missing: run `just wasm` first")

    procs, failures = [], []
    tmp = tempfile.mkdtemp(prefix="uniblox-e2e-")
    try:
        services = os.environ.get("UNIBLOX_SERVICES")
        cmd = (
            [services]
            if services
            else ["cargo", "run", "--quiet", "--locked", "-p", "services"]
        )
        procs.append(
            subprocess.Popen(
                cmd,
                cwd=root,
                env={**os.environ, "UNIBLOX_SIGNALING_PORT": str(SIGNALING_PORT)},
            )
        )
        web_port = free_port()
        handler = functools.partial(QuietHandler, directory=dist)
        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", web_port), handler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        driver_port = free_port()
        procs.append(
            subprocess.Popen(
                [chromedriver, f"--port={driver_port}"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        )
        deadline = time.monotonic() + 300  # the first `cargo run` may compile
        wait_for_port(SIGNALING_PORT, deadline)
        wait_for_port(driver_port, deadline)
        driver = WebDriver(f"http://127.0.0.1:{driver_port}")
        url = f"http://127.0.0.1:{web_port}/"

        a = driver.session(chromium, os.path.join(tmp, "a"))
        try:
            a.goto(url)
            a.wait_for(
                marker("[uniblox-metrics] first-frame"),
                "the first frame",
                time.monotonic() + TIMEOUT_S,
            )
            print("PASS first frame")
        except Exception as err:  # keep going: report every path
            failures.append(f"first frame: {err}")

        try:
            a.press("k")
            sid = a.wait_for(saved_id, "a save", time.monotonic() + TIMEOUT_S)
            a.lines.clear()
            a.goto(url)
            a.wait_for(
                marker("[uniblox-metrics] first-frame"),
                "the first frame after reload",
                time.monotonic() + TIMEOUT_S,
            )
            a.press("l")
            a.wait_for(
                marker(f"[uniblox-save] loaded {sid}"),
                f"loading {sid}",
                time.monotonic() + TIMEOUT_S,
            )
            print(f"PASS save, reload, load ({sid[:12]}...)")
        except Exception as err:
            failures.append(f"save and reload: {err}")

        b = driver.session(chromium, os.path.join(tmp, "b"))
        try:
            a.lines.clear()
            a.goto(url)
            b.goto(url)
            want = ("[uniblox-demo][STATE] from", "[uniblox-demo][EVENT] from")
            deadline = time.monotonic() + TIMEOUT_S
            while time.monotonic() < deadline:
                got = [
                    all(any(w in line for line in s.poll_console()) for w in want)
                    for s in (a, b)
                ]
                if all(got):
                    break
                time.sleep(0.25)
            else:
                raise TimeoutError(
                    f"tab A both channels: {got[0]}, tab B both channels: {got[1]}"
                )
            print("PASS two tabs: both received state and events packets")
        except Exception as err:
            failures.append(f"two tabs: {err}")
        finally:
            for s in (a, b):
                s.close()
    finally:
        for p in procs:
            p.terminate()
        for p in procs:
            with contextlib.suppress(Exception):
                p.wait(timeout=10)

    for f in failures:
        print(f"FAIL {f}")
    if failures:
        sys.exit(1)


if __name__ == "__main__":
    main()
