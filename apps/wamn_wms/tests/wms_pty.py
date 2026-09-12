#!/usr/bin/env python3
"""Drive the composed WMS form against a disposable live route.

Run each mode with its own evidence directory. The caller owns the labels bucket.
Success requires the bucket. Partial requires the caller to remove that bucket.
Credentials enter through private files. Evidence omits authorization headers.
"""

import argparse
import hashlib
import http.client
import http.server
import importlib.util
import json
import os
from pathlib import Path
import sys
import termios
import threading
from types import SimpleNamespace
from urllib.parse import unquote, urlsplit
import uuid

sys.dont_write_bytecode = True
SUPPORT_PATH = Path(__file__).resolve().parents[3] / "crates/client/terminal/tests/live_support.py"
SUPPORT_SPEC = importlib.util.spec_from_file_location("wms_live_support", SUPPORT_PATH)
support = importlib.util.module_from_spec(SUPPORT_SPEC)
SUPPORT_SPEC.loader.exec_module(support)
require = support.require


class Relay:
    """Record attempts while forwarding their exact bodies to the live route."""

    def __init__(self, endpoint, host, token, timeout):
        self.records, self.lock = [], threading.Lock()
        relay = self
        upstream = urlsplit(endpoint)

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                record = {"method": "POST", "path": self.path,
                          "host": self.headers.get("Host"),
                          "content_type": self.headers.get("Content-Type"),
                          "request_body": body.decode("utf-8", "replace"),
                          "request_sha256": hashlib.sha256(body).hexdigest(),
                          "authorization_matches": self.headers.get("Authorization") == "Bearer " + token}
                connection = None
                try:
                    require(record["host"] == host, "operator used the wrong routing host")
                    require(record["authorization_matches"], "operator used the wrong credential")
                    constructor = http.client.HTTPSConnection if upstream.scheme == "https" else http.client.HTTPConnection
                    connection = constructor(upstream.hostname, upstream.port, timeout=timeout)
                    connection.request("POST", upstream.path.rstrip("/") + self.path, body=body,
                                       headers={"Host": self.headers["Host"],
                                                "Authorization": self.headers["Authorization"],
                                                "Content-Type": self.headers["Content-Type"]})
                    response = connection.getresponse()
                    answer = response.read()
                    record.update(status=response.status, response_body=answer.decode("utf-8", "replace"),
                                  response_sha256=hashlib.sha256(answer).hexdigest())
                    with relay.lock:
                        relay.records.append(record)
                    self.send_response(response.status)
                    self.send_header("Content-Type", response.getheader("Content-Type", "application/json"))
                    self.send_header("Content-Length", str(len(answer)))
                    self.end_headers()
                    self.wfile.write(answer)
                except Exception as error:
                    record["relay_error"] = type(error).__name__
                    with relay.lock:
                        if record not in relay.records:
                            relay.records.append(record)
                    self.send_error(502, "live route forwarding failed")
                finally:
                    if connection is not None:
                        connection.close()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def snapshot(self):
        with self.lock:
            return list(self.records)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


def seed(db, ids, prefix):
    db.sql("01-seed", f"""BEGIN;
INSERT INTO wms.product (id, product_code) VALUES ('{ids.product}', '{prefix}-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('{ids.source}', '{prefix}-FROM'), ('{ids.destination}', '{prefix}-TO');
INSERT INTO wms.pallet (id, pallet_code, location_id, status)
  VALUES ('{ids.pallet}', '{prefix}-PALLET', '{ids.source}', 'available');
INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status)
  VALUES ('{ids.pallet}', '{ids.product}', 10.0000, 'available');
COMMIT;""")


def snapshot(db, name, ids):
    return db.sql(name, f"""SELECT json_build_object(
  'claims', (SELECT count(*) FROM wms.inventory_move_command WHERE pallet_id = '{ids.pallet}'),
  'movements', (SELECT count(*) FROM wms.inventory_movement WHERE pallet_id = '{ids.pallet}'),
  'command', (SELECT row_to_json(command) FROM (
    SELECT idempotency_key, movement_id, pallet_id, pallet_status, row_version
    FROM wms.inventory_move_command WHERE pallet_id = '{ids.pallet}') AS command),
  'movement', (SELECT row_to_json(movement) FROM (
    SELECT idempotency_key, pallet_id, product_id, from_location_id, to_location_id, kind, quantity::text
    FROM wms.inventory_movement WHERE pallet_id = '{ids.pallet}') AS movement),
  'pallet', (SELECT json_build_object('location_id', location_id, 'status', status, 'row_version', row_version)
    FROM wms.pallet WHERE id = '{ids.pallet}'),
  'quantity', (SELECT json_agg(json_build_object('product_id', product_id, 'quantity', quantity::text, 'status', status))
    FROM wms.pallet_quantity WHERE pallet_id = '{ids.pallet}'));""", parse=True)


def cleanup(db, ids):
    remaining = db.sql("90-cleanup", f"""BEGIN;
DELETE FROM wms.inventory_movement WHERE pallet_id = '{ids.pallet}';
DELETE FROM wms.inventory_move_command WHERE pallet_id = '{ids.pallet}';
DELETE FROM wms.pallet_quantity WHERE pallet_id = '{ids.pallet}';
DELETE FROM wms.pallet WHERE id = '{ids.pallet}';
DELETE FROM wms.location WHERE id IN ('{ids.source}', '{ids.destination}');
DELETE FROM wms.product WHERE id = '{ids.product}';
COMMIT;
SELECT (SELECT count(*) FROM wms.pallet WHERE id = '{ids.pallet}')
  + (SELECT count(*) FROM wms.location WHERE id IN ('{ids.source}', '{ids.destination}'))
  + (SELECT count(*) FROM wms.product WHERE id = '{ids.product}');""")
    require(remaining == "0", "owned WMS fixture cleanup left rows")


def drive(session, relay, db, evidence, ids, mode):
    def frame(name):
        evidence.write(name + ".txt", session.display.text() + "\n")

    def keys(description, data):
        evidence.event("keys", description=description, bytes=data.hex())
        session.send(data)

    def edit(pointer, value):
        if "Enter saves; Esc cancels" in session.display.text():
            session.text(pointer)
        else:
            # The form order comes from the generated input descriptor.
            session.text(pointer + " (")
            rows = session.display.text().splitlines()
            position = next(index for index, row in enumerate(rows) if pointer + " (" in row)
            selected = next(index for index, row in enumerate(rows) if "> /" in row)
            distance = position - selected
            keys("select " + pointer, (b"\x1b[B" if distance >= 0 else b"\x1b[A") * abs(distance))
            session.until(lambda: any(
                "> " + pointer + " (" in row for row in session.display.text().splitlines()
            ), "selected input " + pointer)
            keys("open " + pointer, b"\r")
            session.text("Enter saves; Esc cancels")
        keys("set " + pointer, value.encode() + b"\r")
        session.until(lambda: "Enter saves; Esc cancels" not in session.display.text(), "saved input")

    session.text("pallet / get")
    require(termios.tcgetattr(session.slave) != session.original, "operator did not enter raw terminal mode")
    edit("/id", ids.pallet)
    frame("10-read-input")
    keys("read the owned pallet", b"\x13")
    session.text("inventory / move")
    edit("/value/to_location_id", ids.destination)
    session.text(ids.pallet)
    require(any("/value/expected_row_version" in row and row.rstrip(" │").endswith(": 1")
                for row in session.display.text().splitlines()), "composition did not bind the read revision")
    frame("11-move-input")
    keys("submit the composed move once", b"\x13")
    state = "Succeeded. This intent is spent" if mode == "success" else "Partially completed; committed work remains."
    session.text(state)
    frame("12-completion")
    records = relay.snapshot()
    require([record["path"] for record in records] == ["/pallet/get", "/inventory/move"],
            "the form did not send exactly one read and one move")
    move = records[1]
    request = json.loads(move["request_body"])
    require(len(request) == 1, "move request is not a single-item envelope")
    command = request[0]["value"]
    require(set(command) == {"idempotency_key", "occurred_at", "pallet_id", "to_location_id", "expected_row_version"}
            and command["pallet_id"] == ids.pallet and command["to_location_id"] == ids.destination
            and command["expected_row_version"] == 1, "move body differs from the bound read and entered destination")
    response = json.loads(move["response_body"])
    answer = response if mode == "success" else response["committed_result"]
    require(len(answer) == 1 and answer[0]["request_id"] == request[0]["request_id"],
            "move response does not match the submitted request")
    value = answer[0]["value"]
    movement_id = str(uuid.UUID(value["movement_id"]))
    committed = {"movement_id": movement_id, "pallet_id": ids.pallet,
                 "location_id": ids.destination, "pallet_status": "available", "row_version": 2}
    if mode == "success":
        require(move["status"] == 200 and set(value) == set(committed) | {"zpl", "stored"},
                "successful move did not return the declared enriched result")
        require(all(value[key] == expected for key, expected in committed.items()), "successful movement result differs")
        require(value["stored"]["key"] == movement_id and value["stored"]["container"]
                and value["zpl"], "successful result has no stored label key or label content")
        session.text("stored.key: " + movement_id)
        session.text("stored.container: " + value["stored"]["container"])
        frame("13-stored-label-key")
    else:
        require(move["status"] == 500 and set(response) == {"committed_result", "failed_outcome"}
                and value == committed, "partial response does not preserve the exact committed movement")
        failure = response["failed_outcome"]
        require(set(failure) == {"code", "message", "effect_outcome"}
                and failure["code"] == "write_failed" and failure["effect_outcome"] == "responded"
                and failure["message"], "partial response differs from the missing-bucket failure")
        session.text("Committed result:")
        session.text(movement_id)
        session.text("Failed outcome:")
        session.text("write_failed")
        session.text("responded")
        frame("13-partial-result-and-failure")
    observed = snapshot(db, "14-committed-db", ids)
    require(observed["claims"] == observed["movements"] == 1, "move did not commit exactly one claim and movement")
    require(observed["command"] == {"idempotency_key": command["idempotency_key"], "movement_id": movement_id,
                                    "pallet_id": ids.pallet, "pallet_status": "available", "row_version": 2},
            "database claim differs from the visible committed result")
    require(observed["pallet"] == {"location_id": ids.destination, "status": "available", "row_version": 2},
            "pallet did not retain the committed move")
    require(observed["movement"] == {"idempotency_key": command["idempotency_key"], "pallet_id": ids.pallet,
                                     "product_id": ids.product, "from_location_id": ids.source,
                                     "to_location_id": ids.destination, "kind": "move", "quantity": "10.0000"},
            "movement differs from the seeded quantity and locations")
    require(observed["quantity"] == [{"product_id": ids.product, "quantity": "10.0000", "status": "available"}],
            "move changed the pallet quantity")
    keys("attempt captured retry of the spent move", b"\x1b[18~")  # F7
    session.text("this submission does not permit captured retry")
    frame("15-retry-refused")
    session.quiet(2)
    keys("attempt submit of the spent move", b"\x13")
    session.text("start a new command explicitly")
    frame("16-resubmit-refused")
    session.quiet(2)
    require(snapshot(db, "17-spent-db", ids) == observed, "spent controls changed the committed database state")
    require(len(relay.snapshot()) == 2, "spent controls sent another HTTP request")
    keys("quit the completed operator", b"q")
    session.finish()
    frame("18-restored-terminal")
    return {"exit_code": session.process.returncode, "terminal_restored": True, "mode": mode,
            "movement_id": movement_id, "pallet_id": ids.pallet, "location_id": ids.destination,
            "row_version": 2, "http_requests": 2, "read_requests": 1, "move_requests": 1,
            "committed_claims": 1, "committed_movements": 1, "spent_controls_send_nothing": True,
            "stored": value.get("stored"), "stored_key": "wms/" + movement_id if mode == "success" else None,
            "label_sha256": hashlib.sha256(value["zpl"].encode()).hexdigest() if mode == "success" else None,
            "partial_result_and_failure_visible": mode == "partial"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "operator-pat-file", "target-postgres-url-file", "evidence-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("endpoint", "host", "target-instance"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--mode", choices=("success", "partial"), required=True)
    parser.add_argument("--timeout", type=float, default=60.0)
    args = parser.parse_args()
    evidence = session = relay = db = ids = helper = None
    result = {"passed": False, "cleanup": False, "mode": args.mode}
    try:
        token = args.operator_pat_file.read_text().strip()
        database_url = args.target_postgres_url_file.read_text().strip()
        require(token and database_url, "credential files must not be empty")
        endpoint = urlsplit(args.endpoint)
        require(endpoint.scheme in ("http", "https") and endpoint.hostname and not endpoint.username
                and not endpoint.password and not endpoint.query and not endpoint.fragment,
                "endpoint must be an HTTP URL without credentials, query, or fragment")
        require(args.timeout > 0, "timeout must be positive")
        binary = args.binary.resolve(strict=True)
        require(binary.is_file() and os.access(binary, os.X_OK), "binary must be executable")
        root = Path(__file__).resolve().parents[3]
        helper, helper_path = support.load_terminal(root)
        helper.TIMEOUT = args.timeout
        helper.ROWS, helper.COLUMNS = 60, 260
        evidence = support.Evidence(args.evidence_dir, [token, database_url, unquote(urlsplit(database_url).password or "")])
        ids = SimpleNamespace(**{key: str(uuid.uuid4()) for key in ("pallet", "product", "source", "destination")})
        prefix = "WMS-TUI-" + uuid.uuid4().hex[:8]
        evidence.json("inputs.json", {"binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                      "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      "support_sha256": hashlib.sha256(SUPPORT_PATH.read_bytes()).hexdigest(),
                      "terminal_helper_sha256": hashlib.sha256(helper_path.read_bytes()).hexdigest(),
                      "endpoint": args.endpoint, "host": args.host, "target_instance": args.target_instance,
                      "mode": args.mode, "fixture_ids": vars(ids), "fixture_prefix": prefix,
                      "terminal_rows": helper.ROWS, "terminal_columns": helper.COLUMNS, "timeout_seconds": args.timeout})
        db = support.Database(database_url, evidence)
        seed(db, ids, prefix)
        initial = snapshot(db, "02-initial-db", ids)
        require(initial["claims"] == initial["movements"] == 0, "fixture already has a move")
        relay = Relay(args.endpoint, args.host, token, args.timeout)
        evidence.event("launch", binary=str(binary), credential="private PAT file via WAMN_TOKEN",
                       relay=relay.url, upstream=args.endpoint)
        session = helper.Session(binary, root, relay, args.target_instance, args.host, token)
        result.update(drive(session, relay, db, evidence, ids, args.mode))
        result["passed"] = True
    except (Exception, KeyboardInterrupt) as error:
        safe_types = (support.TestError, helper.TestError) if helper is not None else (support.TestError,)
        result["error"] = str(error) if isinstance(error, safe_types) else type(error).__name__
    finally:
        if session is not None:
            evidence.write("terminal.ansi", bytes(session.output).decode("utf-8", "replace"))
            evidence.write("last-frame.txt", session.display.text() + "\n")
            try:
                session.close()
            except Exception as error:
                result["terminal_cleanup_error"] = type(error).__name__
                result["passed"] = False
        if relay is not None:
            try:
                relay.close()
            except Exception as error:
                result["relay_cleanup_error"] = type(error).__name__
                result["passed"] = False
            evidence.json("http.json", relay.snapshot())
        if db is not None and ids is not None:
            try:
                cleanup(db, ids)
                result["cleanup"] = True
            except Exception as error:
                result["cleanup_error"] = type(error).__name__
                result["passed"] = False
        if evidence is not None:
            evidence.json("result.json", result)
            evidence.sums()
    print("WMS live PTY test " + ("passed" if result["passed"] else "failed"))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
