#!/usr/bin/env python3
"""Drive the composed Receiving operator through a real PTY and disposable HTTP fixture.

Run: python3 crates/client/terminal/tests/operator_pty.py --binary /path/to/wamn-receiving
Uses only the Python standard library. It does not build or contact a platform.
"""

import argparse
import codecs
import dataclasses
import errno
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import threading
import time
import uuid

TIMEOUT = 15.0
ROWS, COLUMNS = 40, 160
HOST = "receiving-pty.localhost"
TOKEN = "operator-pty-fixture-token"
ORDER_ID = "00000000-0000-0000-0000-000000000041"
WARNING = b"server request was not cancelled"
CSI = re.compile(r"\x1b\[([0-?]*)[ -/]*([@-~])")
CHILD_SETUP = (
    "import fcntl, os, sys, termios; "
    "fcntl.ioctl(0, termios.TIOCSCTTY, 0); "
    "os.execv(sys.argv[1], [sys.argv[1]])"
)


class ProofError(Exception):
    """A failure message that contains no captured credentials or environment."""


def require(condition, message):
    if not condition:
        raise ProofError(message)


def check_descriptors(root):
    package = root / "apps/wamn_receiving"
    contracts = package / "generated/contracts/location"
    operation = json.loads((contracts / "list.operation.json").read_text())
    result = json.loads((contracts / "list.result.json").read_text())
    require(operation["result"] == result["class"] == "bounded_list", "location.list cardinality changed; update the fixture")
    fields = {(field["path"], field["type"], field["nullable"]) for field in result["fields"]}
    require(fields == {("id", "uuid", False), ("location_code", "text", False)}, "location.list result descriptors changed; update the fixture")
    attachments = json.loads((package / "publication/attachments.json").read_text())
    attachment = attachments["location-list-http"]
    require(attachment["registered-operation"] == operation["operation"], "location.list attachment changed")
    require(attachment["definition"]["route"] == {"path": "/location/list", "method": "POST"}, "location.list route changed; update the fixture")
    wirings = [json.loads(path.read_text()) for path in (package / "publication/wirings").glob("*.json")]
    selected = [wiring for wiring in wirings if wiring["wiring-id"] == attachment["wiring-id"] and wiring["version"] == attachment["wiring-version"]]
    require(len(selected) == 1, "location.list must have one selected wiring")
    wiring = selected[0]
    nodes = list(wiring["nodes"].values())
    require(len(nodes) == 1 and not wiring.get("edges") and nodes[0]["operation"] == operation["operation"] and nodes[0].get("terminal") == "respond", "location.list is no longer a direct served result; update the fixture")


def check_query_descriptors(root):
    package = root / "apps/wamn_receiving"
    contracts = package / "generated/contracts/purchase_order"
    operation = json.loads((contracts / "query.operation.json").read_text())
    result = json.loads((contracts / "query.result.json").read_text())
    require(operation["result"] == result["class"] == "page", "purchase_order.query cardinality changed; update the fixture")
    fields = {(field["path"], field["type"], field["nullable"]) for field in result["fields"]}
    require(fields == {
        ("id", "uuid", False), ("purchase_order_number", "text", False),
        ("status", "text", False), ("row_version", "int64", False),
        ("supplier_id", "uuid", False), ("created_at", "timestamptz", False),
        ("updated_at", "timestamptz", False),
    }, "purchase_order.query result descriptors changed; update the fixture")
    attachments = json.loads((package / "publication/attachments.json").read_text())
    attachment = attachments["purchase-order-query-http"]
    require(attachment["registered-operation"] == operation["operation"], "purchase_order.query attachment changed")
    require(attachment["definition"]["route"] == {"path": "/purchase_order/query", "method": "POST"}, "purchase_order.query route changed; update the fixture")
    wirings = [json.loads(path.read_text()) for path in (package / "publication/wirings").glob("*.json")]
    selected = [wiring for wiring in wirings if wiring["wiring-id"] == attachment["wiring-id"] and wiring["version"] == attachment["wiring-version"]]
    require(len(selected) == 1, "purchase_order.query must have one selected wiring")
    wiring = selected[0]
    nodes = list(wiring["nodes"].values())
    require(len(nodes) == 1 and not wiring.get("edges") and nodes[0]["operation"] == operation["operation"] and nodes[0].get("terminal") == "respond", "purchase_order.query is no longer a direct served result; update the fixture")


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


class Display:
    """Apply cursor moves and erases, since terminal diffs omit unchanged text."""

    def __init__(self):
        self.cells = [[" "] * COLUMNS for _ in range(ROWS)]
        self.row = self.column = 0
        self.buffer = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.reports = 0

    def feed(self, data):
        self.buffer += self.decoder.decode(data)
        while self.buffer:
            if self.buffer.startswith("\x1b["):
                match = CSI.match(self.buffer)
                if not match:
                    return
                self.csi(*match.groups())
                self.buffer = self.buffer[match.end():]
                continue
            if self.buffer[0] == "\x1b":
                if len(self.buffer) < 2:
                    return
                # Crossterm emits CSI for the screen operations used here.
                self.buffer = self.buffer[2:]
                continue
            character, self.buffer = self.buffer[0], self.buffer[1:]
            if character == "\r":
                self.column = 0
            elif character == "\n":
                self.row = min(self.row + 1, ROWS - 1)
            elif character == "\b":
                self.column = max(0, self.column - 1)
            elif character >= " ":
                if self.column >= COLUMNS:
                    self.row, self.column = min(self.row + 1, ROWS - 1), 0
                self.cells[self.row][self.column] = character
                self.column += 1

    def csi(self, parameters, command):
        if parameters.startswith("?"):
            if parameters == "?1049" and command == "h":
                self.cells = [[" "] * COLUMNS for _ in range(ROWS)]
                self.row = self.column = 0
            return
        values = [int(value) if value else 0 for value in parameters.split(";")]
        amount = values[0] or 1
        if command in "Hf":
            self.row = min(ROWS - 1, amount - 1)
            self.column = min(COLUMNS - 1, (values[1] or 1) - 1 if len(values) > 1 else 0)
        elif command in "ABCD":
            if command == "A":
                self.row = max(0, self.row - amount)
            elif command == "B":
                self.row = min(ROWS - 1, self.row + amount)
            elif command == "C":
                self.column = min(COLUMNS - 1, self.column + amount)
            else:
                self.column = max(0, self.column - amount)
        elif command == "G":
            self.column = min(COLUMNS - 1, amount - 1)
        elif command == "d":
            self.row = min(ROWS - 1, amount - 1)
        elif command == "J":
            position = self.row * COLUMNS + self.column
            for row in range(ROWS):
                for column in range(COLUMNS):
                    cell = row * COLUMNS + column
                    if values[0] in (2, 3) or (values[0] == 0 and cell >= position) or (values[0] == 1 and cell <= position):
                        self.cells[row][column] = " "
        elif command == "K":
            for column in range(COLUMNS):
                if values[0] == 2 or (values[0] == 0 and column >= self.column) or (values[0] == 1 and column <= self.column):
                    self.cells[self.row][column] = " "
        elif command == "n" and values[0] == 6:
            self.reports += 1

    def text(self):
        return "\n".join("".join(row) for row in self.cells)


class Session:
    def __init__(self, binary, root, fixture, instance):
        self.master, self.slave = pty.openpty()
        self.process = None
        self.fixture = fixture
        self.output = bytearray()
        self.eof = False
        self.display = Display()
        try:
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
            self.original = termios.tcgetattr(self.slave)
            environment = {
                "PATH": os.defpath, "TERM": "xterm-256color", "LANG": "C.UTF-8",
                "NO_PROXY": "127.0.0.1,localhost",
                "WAMN_BASE_URL": fixture.url, "WAMN_HOST": HOST,
                "WAMN_TOKEN": TOKEN, "WAMN_TARGET_INSTANCE": instance,
            }
            # Exec a small child helper to acquire the controlling terminal.
            # No preexec_fn runs Python after fork in this threaded parent.
            self.process = subprocess.Popen(
                [sys.executable, "-c", CHILD_SETUP, str(binary)],
                stdin=self.slave, stdout=self.slave, stderr=self.slave,
                cwd=root, env=environment, start_new_session=True,
            )
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def pump(self, timeout=0.05):
        if not select.select([self.master], [], [], timeout)[0]:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:
                self.eof = True
                return
            raise
        if not data:
            self.eof = True
            return
        self.output.extend(data)
        require(len(self.output) <= 2_000_000, "operator emitted excessive terminal output")
        self.display.feed(data)
        while self.display.reports:
            self.send(f"\x1b[{self.display.row + 1};{self.display.column + 1}R".encode())
            self.display.reports -= 1

    def until(self, predicate, description, allow_exit=False):
        deadline = time.monotonic() + TIMEOUT
        while True:
            self.pump()
            self.fixture.snapshot()
            if predicate():
                return
            require(allow_exit or self.process.poll() is None, f"operator exited while waiting for {description}")
            require(time.monotonic() < deadline, f"timed out waiting for {description}")

    def text(self, expected):
        self.until(lambda: expected in self.display.text(), expected)

    def send(self, keys):
        os.write(self.master, keys)

    def wait_for_orders(self):
        self.text("purchase_order / query (query)")
        require(termios.tcgetattr(self.slave) != self.original, "operator did not enter raw terminal mode")

    def quiet(self, count):
        deadline = time.monotonic() + 0.5
        while time.monotonic() < deadline:
            self.pump()
            require(self.process.poll() is None, "operator exited during a quiet request boundary")
            require(len(self.fixture.snapshot()) == count, "an extra or replayed HTTP request was sent")

    def finish(self, unresolved=False, exit_code=0):
        self.until(lambda: self.process.poll() is not None, "operator shutdown", allow_exit=True)
        while not self.eof and select.select([self.master], [], [], 0)[0]:
            self.pump(0)
        require(self.process.returncode == exit_code, "operator reported the wrong exit reason")
        require(termios.tcgetattr(self.slave) == self.original, "operator did not restore the original terminal attributes")
        entered = self.output.find(b"\x1b[?1049h")
        left = self.output.rfind(b"\x1b[?1049l")
        shown = self.output.rfind(b"\x1b[?25h")
        require(entered >= 0 and left > entered and shown > entered, "operator did not leave the alternate screen and restore the cursor")
        require((WARNING in self.output) == unresolved, "shutdown warning did not match unresolved server work")
        if unresolved:
            require(self.output.find(WARNING) > left, "unresolved warning was printed before terminal restoration")
        require(TOKEN.encode() not in self.output, "operator output exposed the fixture credential")

    def close(self):
        try:
            if self.process is not None and self.process.poll() is None:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=1.0)
        finally:
            os.close(self.master)
            os.close(self.slave)

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        self.close()


def verify_request(request):
    require(request.method == "POST", "HTTP request used the wrong method")
    require(request.path == "/purchase_order/query", "HTTP request used the wrong route")
    require(request.headers.get("host") == HOST, "HTTP request used the wrong routing host")
    require(request.headers.get("authorization") == f"Bearer {TOKEN}", "HTTP request did not use the fixture authorization")
    require(request.headers.get("content-type") == "application/json", "HTTP request used the wrong content type")
    envelope = json.loads(request.body)
    require(isinstance(envelope, list) and len(envelope) == 1 and isinstance(envelope[0], dict) and set(envelope[0]) == {"request_id"}, "purchase_order.query startup request did not contain exactly one supplied request_id")
    request_id = envelope[0]["request_id"]
    parsed = uuid.UUID(request_id)
    require(str(parsed) == request_id and parsed.version == 4, "driver did not supply a canonical request UUID")
    require(request.body == json.dumps(envelope, sort_keys=True, separators=(",", ":")).encode(), "actual HTTP body was not canonical JSON")
    return request_id


def prove(binary, root):
    check_query_descriptors(root)
    fixture = Fixture()
    try:
        with Session(binary, root, fixture, "pty-target-first") as session:
            session.wait_for_orders()
            session.until(lambda: len(fixture.snapshot()) >= 1, "automatic first HTTP request")
            verify_request(fixture.snapshot()[0])
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
            old_id = verify_request(old)
            session.text("Pending.")
            session.send(b"\x13")  # Ctrl-S must not duplicate the automatic read.
            session.text("A request is pending. Wait for its outcome.")
            session.send(b"q")
            session.text("A request is pending. Wait, or Ctrl-C")
            session.quiet(2)
            require(not old.release.is_set() and not old.finished.is_set(), "held response completed before SIGTERM")
            session.process.send_signal(signal.SIGTERM)
            session.finish(unresolved=True, exit_code=143)

        # Hold the replacement's own startup read while the old response arrives.
        fixture.configure(True, "PTY-NEW-TARGET")
        with Session(binary, root, fixture, "pty-target-replacement") as session:
            session.wait_for_orders()
            session.until(lambda: len(fixture.snapshot()) >= 3, "automatic new-target request")
            fresh = fixture.snapshot()[2]
            fresh_id = verify_request(fresh)
            require(fresh_id != old_id, "new target replayed the captured old request")
            session.text("Pending.")
            session.text("Results | 0 rows |")
            session.quiet(3)
            old.release.set()
            session.until(old.finished.is_set, "old response release")
            session.quiet(3)
            require(not fresh.release.is_set() and not fresh.finished.is_set(), "replacement response completed before its own release")
            require("PTY-LATE-OLD" not in session.display.text() and "Succeeded." not in session.display.text() and "Pending." in session.display.text(), "old response reached the replacement session")
            fresh.release.set()
            session.text("PTY-NEW-TARGET")
            session.text("Succeeded.")
            require("PTY-LATE-OLD" not in session.display.text() and "Pending." not in session.display.text(), "replacement retained an old result or pending state")
            session.quiet(3)
            session.send(b"q")
            session.finish()
        require(len(fixture.snapshot()) == 3, "replacement session sent an extra request")

        fixture.configure(False, "PTY-INTERRUPT")
        for count, (target, interrupt) in enumerate([("pty-sigint", True), ("pty-ctrl-c", False)], 4):
            with Session(binary, root, fixture, target) as session:
                session.wait_for_orders()
                session.until(lambda: len(fixture.snapshot()) >= count, "automatic interrupt-session request")
                verify_request(fixture.snapshot()[count - 1])
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
        prove(binary, Path(__file__).resolve().parents[4])
    except ProofError as error:
        print(f"operator PTY proof failed: {error}", file=sys.stderr)
        return 1
    except (Exception, KeyboardInterrupt) as error:
        # Do not dump PTY output, HTTP headers, the launch environment, or a traceback.
        print(f"operator PTY proof failed: {type(error).__name__}", file=sys.stderr)
        return 1
    print("operator PTY proof passed: HTTP bytes, pending barriers, terminal restoration, and target replacement")
    return 0


if __name__ == "__main__":
    sys.exit(main())
