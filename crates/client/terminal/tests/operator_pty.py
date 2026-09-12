"""Shared terminal parsing and process sessions for operator tests."""

import codecs
import errno
import fcntl
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import termios
import time

TIMEOUT = 15.0
ROWS, COLUMNS = 40, 160
WARNING = b"server request was not cancelled"
CSI = re.compile(r"\x1b\[([0-?]*)[ -/]*([@-~])")
CHILD_SETUP = (
    "import fcntl, os, sys, termios; "
    "fcntl.ioctl(0, termios.TIOCSCTTY, 0); "
    "os.execv(sys.argv[1], [sys.argv[1]])"
)


class TestError(Exception):
    """A failure message that contains no captured credentials or environment."""


def require(condition, message):
    if not condition:
        raise TestError(message)


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
    def __init__(self, binary, root, fixture, instance, host, token):
        self.master, self.slave = pty.openpty()
        self.process = None
        self.fixture = fixture
        self.token = token
        self.output = bytearray()
        self.eof = False
        self.display = Display()
        try:
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLUMNS, 0, 0))
            self.original = termios.tcgetattr(self.slave)
            environment = {
                "PATH": os.defpath, "TERM": "xterm-256color", "LANG": "C.UTF-8",
                "NO_PROXY": "127.0.0.1,localhost",
                "WAMN_BASE_URL": fixture.url, "WAMN_HOST": host,
                "WAMN_TOKEN": token, "WAMN_TARGET_INSTANCE": instance,
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
        require(self.token.encode() not in self.output, "operator output exposed the fixture credential")

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
