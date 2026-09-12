#!/usr/bin/env python3
"""Test the Receiving operator's live launch, native restart, and cleanup.

Consumes an already running disposable `wamn dev up` environment. This command
runs the real development stages, including builds; reserve the machine first.
It does not provision infrastructure, mutate Git, or stop the environment Gate.
"""

import argparse
import base64
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import re
import signal
import socket
import stat
import struct
import subprocess
import sys
import termios
import time
from urllib.error import HTTPError
from urllib.parse import unquote, urlencode, urlsplit
from urllib.request import Request, urlopen
import uuid

REPOSITORY = Path(__file__).resolve().parents[3]
HARNESS = REPOSITORY / "apps/wamn_receiving/tests/operator_pty.py"
MODULE = importlib.util.spec_from_file_location("receiving_operator_pty", HARNESS)
receiving = importlib.util.module_from_spec(MODULE)
sys.modules[MODULE.name] = receiving
# Import the app assertions and shared terminal mechanics without writing
# __pycache__ into a watched source tree or starting the HTTP fixture.
sys.dont_write_bytecode = True
MODULE.loader.exec_module(receiving)
terminal = receiving.terminal
require, TestError = terminal.require, terminal.TestError

STARTUP_TIMEOUT = 600.0
UI_TIMEOUT = 20.0
MARKER = b"// generated-operator-live restart "
DRAFT_REFERENCE = "restart-reference-" + uuid.uuid4().hex
ORDER_ID = str(uuid.uuid4())
LOCATION_ID = str(uuid.uuid4())
ORDER_NUMBER = "RESTART-" + uuid.uuid4().hex[:12]
BINDING_KEYS = {b"WAMN_BASE_URL", b"WAMN_HOST", b"WAMN_TARGET_INSTANCE"}
CHILD_SETUP = (
    "import fcntl, os, sys, termios; "
    "fcntl.ioctl(0, termios.TIOCSCTTY, 0); "
    "os.execv(sys.argv[1], sys.argv[1:])"
)


class Redactor:
    def __init__(self, config):
        self.secrets = set()
        self.collect(config)
        registry = config.get("registry_auth_file")
        if registry:
            self.collect(json.loads(Path(registry).read_text()))
        self.collect({key: value for key, value in os.environ.items()
                      if re.search(r"TOKEN|PASSWORD|SECRET|ACCESS_KEY", key)})
        self.secrets.discard("")

    def collect(self, value, key=""):
        if isinstance(value, dict):
            if isinstance(value.get("username"), str) and isinstance(value.get("password"), str):
                pair = f"{value['username']}:{value['password']}"
                self.secrets.add(base64.b64encode(pair.encode()).decode())
            for name, child in value.items():
                self.collect(child, name)
        elif isinstance(value, list):
            for child in value:
                self.collect(child, key)
        elif isinstance(value, str) and value:
            if re.search(r"token|password|secret|access_key|^auth$", key, re.I):
                self.secrets.add(value)
            if key == "auth":
                decoded = base64.b64decode(value, validate=True).decode()
                if ":" in decoded:
                    self.secrets.update((decoded, decoded.split(":", 1)[1]))
            if "://" in value:
                parsed = urlsplit(value)
                if parsed.password:
                    self.secrets.update((value, parsed.password, unquote(parsed.password)))

    def exposed(self, text):
        plain = terminal.CSI.sub("", text)
        return any(secret in text or secret in plain for secret in self.secrets)

    def clean(self, text):
        for secret in sorted(self.secrets, key=len, reverse=True):
            text = text.replace(secret, "[redacted]")
        text = re.sub(r"(?i)(Bearer\s+)[^\s\"'<>]+", r"\1[redacted]", text)
        return re.sub(r"(://)[^/@\s]+:[^/@\s]+@", r"\1[redacted]@", text)


def process_identity(pid):
    """A PID plus Linux start time protects checks and cleanup from PID reuse."""
    try:
        process = Path(f"/proc/{pid}")
        fields = (process / "stat").read_text().rpartition(") ")[2].split()
        executable = os.readlink(process / "exe").removesuffix(" (deleted)")
        return {"pid": pid, "started": fields[19], "executable": executable}
    except (FileNotFoundError, ProcessLookupError):
        return None


def same_process(identity):
    try:
        fields = Path(f"/proc/{identity['pid']}/stat").read_text().rpartition(") ")[2].split()
        return fields[19] == identity["started"]
    except (FileNotFoundError, ProcessLookupError):
        return False


def descendants(pid):
    found, pending, seen = [], [pid], {pid}
    while pending:
        parent = pending.pop()
        try:
            tasks = list(Path(f"/proc/{parent}/task").glob("*/children"))
        except FileNotFoundError:
            continue
        for task in tasks:
            try:
                children = [int(child) for child in task.read_text().split()]
            except (FileNotFoundError, ProcessLookupError):
                continue
            for child in children:
                if child in seen:
                    continue
                seen.add(child)
                identity = process_identity(child)
                if identity:
                    found.append(identity)
                    pending.append(child)
    return found


def binding(identity):
    selected = {}
    for entry in Path(f"/proc/{identity['pid']}/environ").read_bytes().split(b"\0"):
        key, _, value = entry.partition(b"=")
        if key in BINDING_KEYS:
            selected[key.decode()] = value.decode()
    require(set(selected) == {key.decode() for key in BINDING_KEYS}, "operator omitted a nonsecret session binding")
    require(all(selected.values()), "operator supplied an empty session binding")
    url = urlsplit(selected["WAMN_BASE_URL"])
    require(url.scheme == "http" and url.hostname == "127.0.0.1" and url.port
            and not url.username and not url.password, "operator did not bind a disposable loopback endpoint")
    return selected


def socket_open(values):
    url = urlsplit(values["WAMN_BASE_URL"])
    try:
        with socket.create_connection((url.hostname, url.port), timeout=0.05):
            return True
    except OSError:
        return False


class LiveSession(terminal.Session):
    def __init__(self, wamn, config_path, overlay, host_binary):
        self.master, self.slave = pty.openpty()
        self.process = None
        self.token = receiving.TOKEN
        self.output = bytearray()
        self.eof = False
        self.display = terminal.Display()
        self.host_binary = str(host_binary)
        self.owned = {}
        try:
            fcntl.ioctl(self.slave, termios.TIOCSWINSZ,
                        struct.pack("HHHH", terminal.ROWS, terminal.COLUMNS, 0, 0))
            self.original = termios.tcgetattr(self.slave)
            environment = os.environ.copy()
            environment.update(TERM="xterm-256color", LANG="C.UTF-8",
                               RUSTC_WRAPPER="", PYTHONDONTWRITEBYTECODE="1")
            self.process = subprocess.Popen(
                [sys.executable, "-c", CHILD_SETUP, str(wamn), "dev", "--config",
                 str(config_path), "--overlay-root", str(overlay), "--watch", "--tui", "receiving"],
                stdin=self.slave, stdout=self.slave, stderr=self.slave,
                cwd=REPOSITORY, env=environment, start_new_session=True,
            )
        except BaseException:
            os.close(self.master)
            os.close(self.slave)
            raise

    def children(self):
        children = descendants(self.process.pid)
        self.owned.update(((child["pid"], child["started"]), child) for child in children)
        return children

    def activation(self):
        children = self.children()
        operators = [child for child in children
                     if Path(child["executable"]).name == "wamn-receiving"]
        hosts = [child for child in children if child["executable"] == self.host_binary]
        require(len(operators) <= 1, "two Receiving operator processes overlapped")
        require(len(hosts) <= 1, "two activation hosts overlapped")
        if len(operators) == len(hosts) == 1:
            try:
                values = binding(operators[0])
            except (FileNotFoundError, ProcessLookupError):
                return None  # The supervisor can retire this child during observation.
            return {"operator": operators[0], "host": hosts[0], "binding": values}
        return None

    def until(self, predicate, description, allow_exit=False, timeout=UI_TIMEOUT):
        deadline = time.monotonic() + timeout
        while True:
            self.pump(0.05)
            self.children()
            if predicate():
                return
            require(allow_exit or self.process.poll() is None,
                    f"development session exited while waiting for {description}")
            require(time.monotonic() < deadline, f"timed out waiting for {description}")

    def stable(self, activation, seconds):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.pump(0.05)
            require(self.process.poll() is None, "development session exited at an idle boundary")
            require(self.activation() == activation, "activation restarted without an input edit")

    def close(self):
        """Stop only this process session; leave the independent dev-up Gate alone."""
        try:
            self.children()
            if self.process.poll() is None:
                self.process.send_signal(signal.SIGINT)
                deadline = time.monotonic() + 45.0
                while self.process.poll() is None and time.monotonic() < deadline:
                    try:
                        self.pump(0.05)
                        self.children()
                    except Exception:
                        time.sleep(0.05)
            try:
                os.killpg(self.process.pid, 0)
                group_exists = True
            except ProcessLookupError:
                group_exists = False
            if group_exists:
                try:
                    os.killpg(self.process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                deadline = time.monotonic() + 5.0
                while time.monotonic() < deadline:
                    if self.process.poll() is not None and not any(same_process(child) for child in self.owned.values()):
                        break
                    try:
                        self.pump(0.05)
                    except Exception:
                        time.sleep(0.05)
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            self.process.wait(timeout=5.0)
            for child in self.owned.values():
                if same_process(child):
                    try:
                        os.kill(child["pid"], signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            deadline = time.monotonic() + 5.0
            while any(same_process(child) for child in self.owned.values()) and time.monotonic() < deadline:
                time.sleep(0.05)
            require(not any(same_process(child) for child in self.owned.values()), "an owned child survived cleanup")
        finally:
            os.close(self.master)
            os.close(self.slave)


class SourceEdit:
    def __init__(self):
        self.path = REPOSITORY / "crates/client/tui/src/lib.rs"
        require(not self.path.is_symlink(), "native source path must not be a symlink")
        self.original = self.path.read_bytes()
        require(MARKER not in self.original, "native source already contains a live-proof marker")
        self.addition = b"\n" + MARKER + str(uuid.uuid4()).encode() + b"\n"
        self.applied = False

    def apply(self):
        require(not self.path.is_symlink(), "native source became a symlink before the reserved edit")
        require(self.path.read_bytes() == self.original, "native source changed before the reserved edit")
        self.applied = True
        with self.path.open("ab") as source:
            source.write(self.addition)
            source.flush()
            os.fsync(source.fileno())

    def restore(self):
        if not self.applied:
            return
        require(not self.path.is_symlink(), "native source became a symlink; restoration requires review")
        current = self.path.read_bytes()
        if current == self.original:
            return
        require(current.count(self.addition) == 1, "native source marker changed; source restoration requires review")
        restored = current.replace(self.addition, b"", 1)
        self.path.write_bytes(restored)
        require(restored == self.original, "preserved concurrent native edits while removing only the proof marker")


def owned_rows(config, remove=False):
    if remove:
        sql = f"""BEGIN;
DELETE FROM receiving.purchase_order WHERE id = '{ORDER_ID}';
DELETE FROM receiving.location WHERE id = '{LOCATION_ID}';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = '{ORDER_ID}')
     + (SELECT count(*) FROM receiving.location WHERE id = '{LOCATION_ID}');"""
    else:
        sql = f"""BEGIN;
INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id, created_at)
SELECT '{ORDER_ID}', '{ORDER_NUMBER}', '{ORDER_ID}',
       COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second'
FROM receiving.purchase_order WHERE true
ON CONFLICT ON CONSTRAINT purchase_order_id_pkey DO NOTHING;
INSERT INTO receiving.location (id, location_code)
VALUES ('{LOCATION_ID}', '{ORDER_NUMBER}-LOC')
ON CONFLICT ON CONSTRAINT location_id_pkey DO NOTHING;
COMMIT;
SELECT id FROM receiving.purchase_order ORDER BY created_at, id LIMIT 1;"""
    result = subprocess.run(
        ["psql", "-X", "-A", "-t", "-q", "-v", "ON_ERROR_STOP=1",
         "--dbname", config["target_database_url"]],
        input=sql, text=True, capture_output=True, timeout=30,
        env=dict(os.environ, PGCONNECT_TIMEOUT="10"),
    )
    require(result.returncode == 0, "owned Receiving rows could not be prepared or removed")
    require(result.stdout.strip() == ("0" if remove else ORDER_ID),
            "owned Receiving rows remain or the owned purchase order is not first")


def open_reference(session, config):
    owned_rows(config)
    session.send(b"\x1b[15~")  # F5 refreshes the composed purchase-order list.
    session.text(ORDER_NUMBER)
    session.send(b"\r")
    session.text("receiving / load_receipt_screen")
    session.text("Receive into ")
    session.send(b"\x1bOR")  # F3 opens the receipt-reference editor.
    session.text("/value/receipt_reference")
    session.text("Enter saves; Esc cancels")


def reference_empty(screen):
    rows = screen.splitlines()
    for index, row in enumerate(rows[:-1]):
        if "/value/receipt_reference" in row:
            return rows[index + 1].strip(" │") == "_"
    return False


def host_diagnostics(session, config, activation):
    instance = activation["binding"]["WAMN_TARGET_INSTANCE"]
    directory = Path(str(config["wasmtime_cache_dir"]) + ".operator-logs")
    announced = directory / f"operator-host-{session.process.pid}-{instance}.log"
    path = announced if announced.is_absolute() else REPOSITORY / announced
    entered = session.output.rfind(b"\x1b[?1049h")
    announcement = f"Host diagnostics: {announced}".encode()
    require(entered >= 0 and session.output.rfind(announcement, 0, entered) >= 0,
            "host diagnostics path was not announced before operator entry")
    info = path.lstat()
    require(stat.S_ISREG(info.st_mode) and stat.S_IMODE(info.st_mode) == 0o600
            and info.st_uid == os.getuid(), "host diagnostics must be an owned regular mode 0600 file")
    startup = False
    # Inspect only this activation's file; never copy its contents into evidence.
    with path.open(errors="replace") as log:
        for line in log:
            startup |= "wamn-host runtime startup completed" in line
    require(startup, "host diagnostics omitted the runtime startup marker")
    return {"path": str(path), "announced_before_operator": True, "mode_0600": True,
            "startup_retained": startup}


def request_span(document, route_host, started_ns, ended_ns):
    for batch in document.get("batches", []):
        resource = {entry["key"]: entry["value"].get("stringValue")
                    for entry in batch.get("resource", {}).get("attributes", [])}
        for scope in batch.get("scopeSpans", []):
            for span in scope.get("spans", []):
                attributes = {entry["key"]: entry["value"].get("stringValue", entry["value"].get("intValue"))
                              for entry in span.get("attributes", [])}
                if (span.get("name") == "handle_http_request"
                        and attributes.get("http.method") == "POST"
                        and attributes.get("http.uri") == "/purchase_order/query"
                        and attributes.get("http.host") == route_host
                        and str(attributes.get("http.response.status_code")) == "200"
                        and started_ns <= int(span["startTimeUnixNano"]) < int(span["endTimeUnixNano"]) <= ended_ns
                        and resource.get("service.instance.id")):
                    return {"span_id": span["spanId"], "service_instance_id": resource["service.instance.id"],
                            "method": attributes["http.method"], "uri": attributes["http.uri"],
                            "host": attributes["http.host"], "status": 200,
                            "started_ns": span["startTimeUnixNano"], "ended_ns": span["endTimeUnixNano"]}
    return None


def request_trace(config, started_ns, ended_ns):
    endpoint = config["tempo_query_url"].rstrip("/")
    query = ('{ name = "handle_http_request" && span."http.method" = "POST"'
             ' && span."http.uri" = "/purchase_order/query"'
             ' && span."http.host" = ' + json.dumps(config["route_host"]) + ' }')
    search_url = endpoint + "/api/search?" + urlencode({
        "q": query, "start": started_ns // 1_000_000_000,
        "end": ended_ns // 1_000_000_000 + 1, "limit": 20,
    })
    deadline = time.monotonic() + 120
    while True:
        with urlopen(Request(search_url, headers={"Accept": "application/json"}), timeout=5) as response:
            search = json.load(response)
        for candidate in search.get("traces", []):
            trace_id = candidate["traceID"]
            require(re.fullmatch(r"[0-9a-fA-F]{32}", trace_id), "Tempo returned an invalid trace ID")
            try:
                with urlopen(Request(endpoint + "/api/traces/" + trace_id,
                                     headers={"Accept": "application/json"}), timeout=5) as response:
                    document = json.load(response)
            except HTTPError as error:
                if error.code == 404:
                    continue  # Search can find a trace before its spans become readable.
                raise
            match = request_span(document, config["route_host"], started_ns, ended_ns)
            if match:
                return {"trace_id": trace_id, "query": query, "read_after_host_exit": True, **match}
        require(time.monotonic() < deadline, "Tempo did not retain the purchase-order query trace")
        time.sleep(0.5)


def require_clean_frame(screen):
    prefixes = ["handle_http_request{", "wamn.linker.register:", "wamn.component.linker_setup:",
                "HTTP server listening", "Host started", "wamn-host runtime startup completed"]
    require(not any(prefix in screen for prefix in prefixes),
            "host diagnostics contaminated the rendered operator frame")


def assert_operator(session, config, edit, evidence):
    first = None
    first_started_ns = time.time_ns()

    def started():
        nonlocal first
        first = session.activation()
        return first is not None and "purchase_order / query" in session.display.text()

    session.until(started, "first real activation and purchase-order screen", timeout=STARTUP_TIMEOUT)
    require(first["binding"]["WAMN_HOST"] == config["route_host"], "operator host differs from the served configuration")
    require(socket_open(first["binding"]), "first activation socket is not listening")
    evidence["first"] = first
    session.stable(first, 2.0)
    session.text("Succeeded.")
    session.text("0 rows")
    evidence["empty_purchase_order_list"] = session.display.text()
    require_clean_frame(evidence["empty_purchase_order_list"])
    open_reference(session, config)
    evidence["first_host_diagnostics"] = host_diagnostics(session, config, first)
    session.send(DRAFT_REFERENCE.encode() + b"\r")
    session.until(lambda: DRAFT_REFERENCE in session.display.text()
                  and "Enter saves; Esc cancels" not in session.display.text(), "saved receipt reference")
    evidence["draft_before_restart"] = session.display.text()
    restart_offset = len(session.output)
    restart_started_ns = time.time_ns()
    edit.apply()
    observed = {"old_operator_gone": False, "old_host_gone": False, "old_socket_closed": False}
    second = None

    def restarted():
        nonlocal second
        observed["old_operator_gone"] |= not same_process(first["operator"])
        observed["old_host_gone"] |= not same_process(first["host"])
        observed["old_socket_closed"] |= not socket_open(first["binding"])
        candidate = session.activation()
        if candidate is None or candidate["operator"] == first["operator"]:
            return False
        require(all(observed.values()), "replacement launched before old operator, host, and socket were gone")
        second = candidate
        return "purchase_order / query" in session.display.text()

    session.until(restarted, "native rebuild and fresh activation", timeout=STARTUP_TIMEOUT)
    require(second["binding"]["WAMN_TARGET_INSTANCE"] != first["binding"]["WAMN_TARGET_INSTANCE"],
            "restart reused the old target instance")
    require(second["binding"]["WAMN_HOST"] == config["route_host"], "replacement host differs from the configuration")
    require(socket_open(second["binding"]), "replacement activation socket is not listening")
    transitions = bytes(session.output[restart_offset:])
    require(0 <= transitions.find(b"\x1b[?1049l") < transitions.find(b"\x1b[?1049h"),
            "old terminal was not restored before the replacement entered")
    evidence["second"] = second
    evidence["retired_before_replacement"] = observed
    session.text("Succeeded.")
    open_reference(session, config)
    session.until(lambda: reference_empty(session.display.text()), "fresh receipt-reference draft")
    screen = session.display.text()
    require(DRAFT_REFERENCE not in screen and "Succeeded." not in screen and "Pending." not in screen,
            "old draft or submission state reached the replacement")
    require(reference_empty(screen), "replacement retained its old receipt reference")
    require_clean_frame(screen)
    evidence["fresh_draft"] = screen
    session.send(b"\x1b")
    session.text("No result yet.")
    require_clean_frame(session.display.text())
    session.stable(second, 3.0)
    require_clean_frame(session.display.text())
    evidence["second_host_diagnostics"] = host_diagnostics(session, config, second)
    evidence["operator_frames_clean"] = True
    session.send(b"q")
    session.text("Discard draft changes and quit? (y/n)")
    session.send(b"y")
    session.until(lambda: session.process.poll() is not None, "q closes the whole development session",
                  allow_exit=True, timeout=45.0)
    session.finish()
    require(not same_process(second["operator"]) and not same_process(second["host"]),
            "q left the Receiving operator or activation host alive")
    require(not socket_open(second["binding"]), "q left the activation socket listening")
    require(all(Path(evidence[key]["path"]).is_file()
                for key in ["first_host_diagnostics", "second_host_diagnostics"]),
            "host diagnostics were not retained after session cleanup")
    evidence["host_diagnostics_retained_after_exit"] = True
    finished_ns = time.time_ns()
    evidence["first_request_trace"] = request_trace(config, first_started_ns, restart_started_ns)
    evidence["second_request_trace"] = request_trace(config, restart_started_ns, finished_ns)
    require(evidence["first_request_trace"]["service_instance_id"]
            != evidence["second_request_trace"]["service_instance_id"],
            "the replacement request trace came from the retired host")
    evidence["exit_code"] = session.process.returncode
    evidence["terminal_restored"] = True


def interrupted(_signal, _frame):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wamn", required=True, type=Path)
    parser.add_argument("--config", required=True, type=Path)
    parser.add_argument("--overlay-root", required=True, type=Path)
    parser.add_argument("--evidence-dir", required=True, type=Path)
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    session, edit, redactor, directory, config = None, None, None, None, None
    stopped = True
    evidence, failure = {}, None
    try:
        os.umask(0o077)
        wamn, config_path = args.wamn.resolve(strict=True), args.config.resolve(strict=True)
        overlay = args.overlay_root.resolve(strict=True)
        require(wamn.is_file() and os.access(wamn, os.X_OK), "--wamn must name a built executable")
        require(overlay == REPOSITORY / "apps/client_acme_receiving", "--overlay-root must name this worktree's Receiving overlay")
        config = json.loads(config_path.read_text())
        require(config.get("operator_bearer_token") and config.get("target_template_database"),
                "configuration must come from a disposable dev-up environment with an operator token")
        host_binary = Path(config["host_binary"]).resolve(strict=True)
        redactor = Redactor(config)
        require(not args.evidence_dir.is_symlink(), "evidence directory must not be a symlink")
        candidate = args.evidence_dir.resolve()
        candidate.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = candidate.stat()
        require(info.st_uid == os.getuid() and stat.S_IMODE(info.st_mode) == 0o700
                and not any(candidate.iterdir()), "evidence directory must be owned, empty, and mode 0700")
        directory = candidate
        receiving.check_descriptors(REPOSITORY)
        edit = SourceEdit()
        evidence["command"] = [str(wamn), "dev", "--config", str(config_path), "--overlay-root",
                               str(overlay), "--watch", "--tui", "receiving"]
        session = LiveSession(wamn, config_path, overlay, host_binary)
        stopped = False
        assert_operator(session, config, edit, evidence)
    except TestError as error:
        failure = str(error)
    except (Exception, KeyboardInterrupt) as error:
        failure = type(error).__name__
    finally:
        # Finish the bounded owned-process cleanup even if a caller interrupts again.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        if session:
            try:
                session.close()
                stopped = True
            except Exception as error:
                detail = str(error) if isinstance(error, TestError) else type(error).__name__
                failure = failure or f"process cleanup failed: {detail}"
        if session and stopped and config:
            try:
                owned_rows(config, remove=True)
                evidence["owned_rows_removed"] = True
            except Exception as error:
                detail = str(error) if isinstance(error, TestError) else type(error).__name__
                failure = failure or f"owned Receiving row cleanup failed: {detail}"
        if edit and stopped:
            try:
                edit.restore()
                evidence["source_restored"] = True
            except Exception as error:
                detail = str(error) if isinstance(error, TestError) else type(error).__name__
                failure = failure or f"source restoration failed: {detail}"
        elif edit and edit.applied:
            evidence["source_restored"] = False
        if directory and redactor:
            try:
                output = bytes(session.output).decode("utf-8", "replace") if session else ""
                exposed = redactor.exposed(output)
                if exposed:
                    failure = failure or "a credential appeared in process output; transcript withheld"
                evidence["passed"] = failure is None
                evidence["failure"] = failure
                evidence["transcript_withheld"] = exposed
                (directory / "result.json").write_text(redactor.clean(json.dumps(evidence, indent=2)) + "\n")
                if not exposed:
                    (directory / "terminal.log").write_text(redactor.clean(output))
                if failure:
                    snippet = terminal.CSI.sub("", output)[-8000:]
                    (directory / "diagnostic.txt").write_text(redactor.clean(snippet))
            except Exception as error:
                failure = failure or f"evidence write failed: {type(error).__name__}"
    if failure:
        print(f"generated operator live proof failed: {failure}", file=sys.stderr)
        return 1
    print("generated operator live proof passed: live route, native restart, target reset, and terminal cleanup")
    return 0


if __name__ == "__main__":
    sys.exit(main())
