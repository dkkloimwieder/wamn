#!/usr/bin/env python3
"""Drive the direct WMS inventory route on a disposable installation."""

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
FIXTURE_PRINCIPAL = "00000000-0000-4000-8000-0000000000f1"


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
                record = {"method": self.command, "path": urlsplit(self.path).path,
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
                    connection.request(self.command, upstream.path.rstrip("/") + self.path, body=body,
                                       headers={"Host": self.headers["Host"],
                                                "Authorization": self.headers["Authorization"],
                                                "Content-Type": self.headers.get("Content-Type", "application/json")})
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

            do_GET = do_POST

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
    # The fixture writes as its test principal, so the stamp trigger has an actor.
    db.sql("01-seed", f"""BEGIN;
SET LOCAL app.user_id = '{FIXTURE_PRINCIPAL}';
INSERT INTO wms.product (id, product_code) VALUES ('{ids.product}', '{prefix}-PRODUCT');
INSERT INTO wms.location (id, location_code) VALUES
  ('{ids.source}', '{prefix}-FROM'), ('{ids.destination}', '{prefix}-TO');
INSERT INTO wms.packaging (id, type, code, location_id) VALUES
  ('{ids.packaging_source}', 'tote', '{prefix}-FROM', '{ids.source}'),
  ('{ids.packaging_destination}', 'carton', '{prefix}-TO', '{ids.destination}');
INSERT INTO wms.inventory (id, packaging_id, location_id, product_id, quantity, disposition)
  VALUES ('{ids.inventory}', '{ids.packaging_source}', '{ids.source}', '{ids.product}', 10.0000, 'available');
COMMIT;""")


def snapshot(db, name, ids):
    return db.sql(name, f"""SELECT json_build_object(
  'claims', (SELECT count(*) FROM wms.inventory_move_command WHERE result::jsonb->>'inventory_id' = '{ids.inventory}'),
  'movements', (SELECT count(*) FROM wms.inventory_transaction WHERE inventory_id = '{ids.inventory}'),
  'command', (SELECT result::json FROM wms.inventory_move_command WHERE result::jsonb->>'inventory_id' = '{ids.inventory}'),
  'movement', (SELECT json_build_object('operation_id', operation_id, 'inventory_id', inventory_id,
      'from_location_id', from_location_id, 'to_location_id', to_location_id,
      'from_packaging_id', from_packaging_id, 'to_packaging_id', to_packaging_id,
      'type', type, 'from_quantity', from_quantity::text, 'to_quantity', to_quantity::text)
    FROM wms.inventory_transaction WHERE inventory_id = '{ids.inventory}'),
  'inventory', (SELECT json_build_object('location_id', location_id, 'packaging_id', packaging_id,
      'disposition', disposition, 'lifecycle', lifecycle, 'quantity', quantity::text, 'row_version', row_version)
    FROM wms.inventory WHERE id = '{ids.inventory}'));""", parse=True)


def cleanup(db, ids):
    # The disposable fixture owner uses administrative authority for cleanup.
    remaining = db.sql("90-cleanup", f"""BEGIN;
DELETE FROM wms.inventory_transaction WHERE inventory_id = '{ids.inventory}';
DELETE FROM wms.inventory_move_command WHERE result::jsonb->>'inventory_id' = '{ids.inventory}';
DELETE FROM wms.inventory WHERE id = '{ids.inventory}';
DELETE FROM wms.packaging WHERE id IN ('{ids.packaging_source}', '{ids.packaging_destination}');
DELETE FROM wms.location WHERE id IN ('{ids.source}', '{ids.destination}');
DELETE FROM wms.product WHERE id = '{ids.product}';
COMMIT;
SELECT count(*) FROM wms.inventory WHERE id = '{ids.inventory}';""")
    require(remaining == "0", "owned WMS fixture cleanup left rows")


def drive(session, relay, db, evidence, ids, mode):
    def edit(pointer, value):
        if "Enter saves; Esc cancels" not in session.display.text():
            session.text(pointer + " (")
            rows = session.display.text().splitlines()
            position = next(i for i, row in enumerate(rows) if pointer + " (" in row)
            selected = next(i for i, row in enumerate(rows) if "> /" in row)
            distance = position - selected
            session.send((b"\x1b[B" if distance >= 0 else b"\x1b[A") * abs(distance))
            session.until(lambda: any("> " + pointer + " (" in row for row in session.display.text().splitlines()), "selected input")
            session.send(b"\r")
        session.text(pointer)
        session.send(value.encode() + b"\r")
        session.until(lambda: "Enter saves; Esc cancels" not in session.display.text(), "saved input")

    session.text("inventory / get")
    require(termios.tcgetattr(session.slave) != session.original, "operator did not enter raw mode")
    edit("/id", ids.inventory)
    session.send(b"\x13")
    session.text("inventory / move")
    edit("/value/to_location_id", ids.destination)
    edit("/value/to_packaging_id", ids.packaging_destination)
    session.send(b"\x13")
    session.text("Succeeded." if mode == "success" else "Refused: invalid_input")
    records = relay.snapshot()
    require([(r["method"], r["path"]) for r in records] == [("GET", "/inventory/get"), ("POST", "/inventory/move")], "one read and one command are required")
    request = json.loads(records[1]["request_body"])
    command = request[0]["value"]
    require(set(command) == {"idempotency_key","occurred_at","inventory_id","to_packaging_id","to_location_id","expected_row_version"}, "unexpected command fields")
    require(command["inventory_id"] == ids.inventory and command["expected_row_version"] == 1
            and command["to_packaging_id"] == ids.packaging_destination and command["to_location_id"] == ids.destination, "command differs from selected inventory and destination")
    response = json.loads(records[1]["response_body"])[0]
    require(response["request_id"] == request[0]["request_id"], "response identity differs")
    observed = snapshot(db, "committed-db", ids)
    if mode == "success":
        value = response["value"]
        expected = {"operation_id":str(uuid.UUID(value["operation_id"])),"inventory_id":ids.inventory,
                    "product_id":ids.product,"packaging_id":ids.packaging_destination,"location_id":ids.destination,
                    "quantity":"10.0000","disposition":"available","lifecycle":"open","row_version":2}
        require(value == expected and observed["command"] == expected, "original result differs from stored claim")
        require(observed["claims"] == observed["movements"] == 1, "move requires one claim and one transaction")
        require(observed["inventory"]["location_id"] == ids.destination and observed["inventory"]["packaging_id"] == ids.packaging_destination, "inventory did not move explicitly")
        require(observed["movement"]["from_location_id"] == ids.source and observed["movement"]["to_location_id"] == ids.destination
                and observed["movement"]["from_quantity"] == observed["movement"]["to_quantity"] == "10.0000", "move history is incomplete")
        session.send(b"\x13")
        session.text("This intent is spent")
        require(len(relay.snapshot()) == 2, "spent command sent another request")
    else:
        require(response["error"]["code"] == "invalid_input", "wrong refusal")
        require(observed["claims"] == observed["movements"] == 0 and observed["inventory"]["location_id"] == ids.source, "refusal mutated business state")
    evidence.write("result-frame.txt", session.display.text() + "\n")
    return {"mode":mode,"request_count":len(records),"inventory_id":ids.inventory,"row_version":observed["inventory"]["row_version"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "operator-pat-file", "target-postgres-url-file", "evidence-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("endpoint", "host", "target-instance"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--mode", choices=("success", "refusal"), required=True)
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
        ids = SimpleNamespace(**{key: str(uuid.uuid4()) for key in ("inventory", "product", "source", "destination", "packaging_source", "packaging_destination")})
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
