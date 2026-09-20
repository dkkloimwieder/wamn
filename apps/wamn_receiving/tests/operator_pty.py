#!/usr/bin/env python3
"""Drive the composed Receiving operator through a real PTY and disposable HTTP fixture.

Run: python3 apps/wamn_receiving/tests/operator_pty.py --binary /path/to/wamn-receiving
Uses only the Python standard library. It does not build or contact a platform.
"""

import argparse
import dataclasses
import http.server
import importlib.util
import json
import os
from pathlib import Path
import signal
import sys
import termios
import threading
import time

sys.dont_write_bytecode = True
TERMINAL_PATH = Path(__file__).resolve().parents[3] / "crates/client/terminal/tests/operator_pty.py"
TERMINAL_SPEC = importlib.util.spec_from_file_location("receiving_terminal", TERMINAL_PATH)
terminal = importlib.util.module_from_spec(TERMINAL_SPEC)
sys.modules[TERMINAL_SPEC.name] = terminal
TERMINAL_SPEC.loader.exec_module(terminal)
require, TestError = terminal.require, terminal.TestError

TIMEOUT = 15.0
HOST = "receiving-pty.localhost"
TOKEN = "operator-pty-fixture-token"
ORDER_ID = "00000000-0000-0000-0000-000000000041"


@dataclasses.dataclass
class Request:
    method: str
    path: str
    headers: dict = dataclasses.field(repr=False)
    body: bytes = dataclasses.field(repr=False)
    label: str
    release: threading.Event = dataclasses.field(default_factory=threading.Event)
    finished: threading.Event = dataclasses.field(default_factory=threading.Event)


class Fixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.errors = []
        self.hold = False
        self.label = "PTY-FIRST-ORDER"
        self.closing = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass  # Never print headers, bodies, credentials, or access logs.

            def do_POST(self):
                request = None
                try:
                    self.connection.settimeout(2.0)
                    size = int(self.headers.get("Content-Length", "0"))
                    require(0 < size <= 8192, "unexpected request body length")
                    body = self.rfile.read(size)
                    with fixture.lock:
                        request = Request(self.command, self.path, {key.lower(): value for key, value in self.headers.items()}, body, fixture.label)
                        if not fixture.hold:
                            request.release.set()
                        fixture.requests.append(request)
                    item = json.loads(body)[0]
                    deadline = time.monotonic() + 4 * TIMEOUT
                    while not request.release.wait(0.1):
                        if fixture.closing.is_set():
                            return
                        require(time.monotonic() < deadline, "held HTTP response timed out")
                    response = json.dumps([{
                        "request_id": item["request_id"],
                        "value": {"item": [{
                            "id": ORDER_ID,
                            "purchase_order_number": request.label,
                            "status": "open",
                            "row_version": "1",
                            "supplier_id": "00000000-0000-0000-0000-000000000042",
                            "created_at": "2026-09-03T00:00:00Z",
                            "updated_at": "2026-09-03T00:00:00Z",
                            "created_by": "00000000-0000-0000-0000-000000000043",
                            "updated_by": "00000000-0000-0000-0000-000000000043",
                        }], "next_cursor": None},
                    }], separators=(",", ":")).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except (BrokenPipeError, ConnectionResetError):
                    pass  # The old client may already have exited when released.
                except Exception:
                    with fixture.lock:
                        fixture.errors.append("HTTP fixture could not complete a request")
                finally:
                    if request is not None:
                        request.finished.set()

        # Concurrent handlers reveal a second send while the first is held.
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = False  # server_close joins every owned handler.
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def configure(self, hold, label):
        with self.lock:
            self.hold, self.label = hold, label

    def snapshot(self):
        with self.lock:
            require(not self.errors, "HTTP fixture failed")
            return list(self.requests)

    def close(self):
        self.closing.set()
        with self.lock:
            for request in self.requests:
                request.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=1.0)
        require(not self.thread.is_alive(), "HTTP fixture thread did not stop")


class Session(terminal.Session):
    def __init__(self, binary, root, fixture, instance):
        super().__init__(binary, root, fixture, instance, HOST, TOKEN)

    def wait_for_orders(self):
        self.text("purchase_order / query (query)")
        require(termios.tcgetattr(self.slave) != self.original, "operator did not enter raw terminal mode")


def assert_operator(binary, root):
    fixture = Fixture()
    try:
        with Session(binary, root, fixture, "pty-target-first") as session:
            session.wait_for_orders()
            session.until(lambda: len(fixture.snapshot()) >= 1, "automatic first HTTP request")
            session.text("PTY-FIRST-ORDER")
            session.text("Succeeded.")
            session.quiet(1)
            session.send(b"q")
            session.finish()
        require(len(fixture.snapshot()) == 1, "successful query sent more than one request")

        fixture.configure(True, "PTY-LATE-OLD")
        with Session(binary, root, fixture, "pty-target-pending") as session:
            session.wait_for_orders()
            session.until(lambda: len(fixture.snapshot()) >= 2, "held HTTP request")
            old = fixture.snapshot()[1]
            session.text("Pending.")
            session.send(b"\x13")  # Ctrl-S must not duplicate the automatic read.
            session.text("A request is pending. Wait for its outcome.")
            session.send(b"q")
            session.text("A request is pending. Wait, or Ctrl-C")
            session.quiet(2)
            require(not old.release.is_set() and not old.finished.is_set(), "held response completed before SIGTERM")
            session.process.send_signal(signal.SIGTERM)
            session.finish(unresolved=True, exit_code=143)

        fixture.configure(False, "PTY-INTERRUPT")
        for count, (target, interrupt) in enumerate([("pty-sigint", True), ("pty-ctrl-c", False)], 3):
            with Session(binary, root, fixture, target) as session:
                session.wait_for_orders()
                session.until(lambda: len(fixture.snapshot()) >= count, "automatic interrupt-session request")
                session.text("PTY-INTERRUPT")
                session.text("Succeeded.")
                session.quiet(count)
                if interrupt:
                    session.process.send_signal(signal.SIGINT)
                else:
                    session.send(b"\x03")
                session.finish()
            require(len(fixture.snapshot()) == count, "operator interruption submitted an extra request")
    finally:
        fixture.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="built composed Receiving operator executable (wamn-receiving)")
    args = parser.parse_args()
    try:
        binary = args.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "--binary must name an executable file")
        assert_operator(binary, Path(__file__).resolve().parents[3])
    except TestError as error:
        print(f"operator PTY test failed: {error}", file=sys.stderr)
        return 1
    except (Exception, KeyboardInterrupt) as error:
        # Do not dump PTY output, HTTP headers, the launch environment, or a traceback.
        print(f"operator PTY test failed: {type(error).__name__}", file=sys.stderr)
        return 1
    print("operator PTY test passed: pending barriers, terminal restoration, SIGTERM, SIGINT, and Ctrl-C")
    return 0


if __name__ == "__main__":
    sys.exit(main())
