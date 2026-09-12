#!/usr/bin/env python3
"""Drive the Receiving composition against an activated disposable environment.

Supply the operator PAT and target PostgreSQL URL through private files.
The driver seeds unique rows, runs the real binary, and removes its owned rows.
It imports the existing PTY driver without starting that driver's HTTP fixture.
Database counts show committed effects. They do not count HTTP attempts.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import termios
import time
from types import SimpleNamespace
from urllib.parse import unquote, urlsplit
import uuid


sys.dont_write_bytecode = True
SUPPORT_PATH = Path(__file__).resolve().parents[3] / "crates/client/terminal/tests/live_support.py"
SUPPORT_SPEC = importlib.util.spec_from_file_location("receiving_live_support", SUPPORT_PATH)
support = importlib.util.module_from_spec(SUPPORT_SPEC)
SUPPORT_SPEC.loader.exec_module(support)
TestError, require = support.TestError, support.require
load_terminal, Evidence, Database = support.load_terminal, support.Evidence, support.Database


def seed(db, ids, prefix):
    db.sql("01-seed", f"""BEGIN;
INSERT INTO receiving.item (id, item_number) VALUES
  ('{ids.item}', '{prefix}-ITEM');
INSERT INTO receiving.location (id, location_code) VALUES
  ('{ids.dock1}', '{prefix}-A'), ('{ids.dock2}', '{prefix}-B');
INSERT INTO receiving.purchase_order
  (id, purchase_order_number, supplier_id, created_at, updated_at)
SELECT '{ids.order}', '{prefix}-PO', '{ids.supplier}',
  COALESCE(MIN(created_at), CURRENT_TIMESTAMP) - interval '1 second', CURRENT_TIMESTAMP
FROM receiving.purchase_order;
INSERT INTO receiving.purchase_order_line
  (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
VALUES
  ('{ids.line1}', '{ids.order}', 1, '{ids.item}', 5.0000, 0.0000),
  ('{ids.line2}', '{ids.order}', 2, '{ids.item}', 7.0000, 0.0000);
COMMIT;""")
    navigation = db.sql("02-navigation", f"""SELECT json_build_object(
  'first_order', (SELECT id FROM receiving.purchase_order ORDER BY created_at, id LIMIT 1),
  'location_index', (SELECT position FROM (
    SELECT id, row_number() OVER (ORDER BY location_code, id) - 1 AS position
    FROM receiving.location) AS location WHERE id = '{ids.dock2}'),
  'location_count', (SELECT count(*) FROM receiving.location));""", parse=True)
    require(navigation["first_order"] == ids.order, "owned order is not first under the default query sort")
    require(navigation["location_count"] <= 100, "live test needs at most 100 locations")
    return navigation


def snapshot(db, name, ids):
    return db.sql(name, f"""SELECT json_build_object(
  'claims', (SELECT count(*) FROM receiving.record_receipt_command WHERE purchase_order_id = '{ids.order}'),
  'receipts', (SELECT count(*) FROM receiving.receipt WHERE purchase_order_id = '{ids.order}'),
  'order', (SELECT json_build_object('status', status, 'row_version', row_version)
    FROM receiving.purchase_order WHERE id = '{ids.order}'),
  'lines', (SELECT json_agg(json_build_object('id', id, 'received', received_quantity::text) ORDER BY line_number)
    FROM receiving.purchase_order_line WHERE purchase_order_id = '{ids.order}'),
  'receipt_lines', (SELECT COALESCE(json_agg(json_build_object(
    'line', line.purchase_order_line_id, 'quantity', line.quantity::text,
    'location', line.location_id)), '[]'::json)
    FROM receiving.receipt_line AS line JOIN receiving.receipt AS receipt ON receipt.id = line.receipt_id
    WHERE receipt.purchase_order_id = '{ids.order}'),
  'canonical', (SELECT COALESCE(json_agg(convert_from(canonical_command, 'UTF8')::json), '[]'::json)
    FROM receiving.record_receipt_command WHERE purchase_order_id = '{ids.order}'));""", parse=True)


def cleanup(db, ids):
    db.sql("90-cleanup", f"""BEGIN;
DELETE FROM receiving.receipt_line WHERE receipt_id IN
  (SELECT id FROM receiving.receipt WHERE purchase_order_id = '{ids.order}');
DELETE FROM receiving.receipt WHERE purchase_order_id = '{ids.order}';
DELETE FROM receiving.record_receipt_command WHERE purchase_order_id = '{ids.order}';
DELETE FROM receiving.purchase_order_line WHERE purchase_order_id = '{ids.order}';
DELETE FROM receiving.purchase_order WHERE id = '{ids.order}';
DELETE FROM receiving.location WHERE id IN ('{ids.dock1}', '{ids.dock2}');
DELETE FROM receiving.item WHERE id = '{ids.item}';
COMMIT;
SELECT (SELECT count(*) FROM receiving.purchase_order WHERE id = '{ids.order}')
  + (SELECT count(*) FROM receiving.location WHERE id IN ('{ids.dock1}', '{ids.dock2}'))
  + (SELECT count(*) FROM receiving.item WHERE id = '{ids.item}');""")
    require((db.evidence.directory / "90-cleanup.stdout").read_text().strip() == "0", "owned fixture cleanup left rows")


def drive(session, db, evidence, ids, prefix, navigation):
    def frame(name):
        evidence.write(name + ".txt", session.display.text() + "\n")

    def keys(description, data):
        evidence.event("keys", description=description, bytes=data.hex())
        session.send(data)

    def saved():
        session.until(lambda: "Enter saves; Esc cancels" not in session.display.text(), "editor save")

    def empty_modal(path):
        session.text(path)
        session.text("Enter saves; Esc cancels")
        rows = session.display.text().splitlines()
        index = next(index for index, row in enumerate(rows) if path in row)
        require(rows[index + 1].strip(" │") == "_", "new entry retained a submitted field")

    def idle():
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline:
            session.pump()
            require(session.process.poll() is None, "operator exited during idle observation")

    def projection():
        session.text("receiving / load_receipt_screen")
        session.text("Receive into ")
        session.text(prefix + "-ITEM")

    session.text(prefix + "-PO")
    require(termios.tcgetattr(session.slave) != session.original, "operator did not enter raw terminal mode")
    frame("10-orders")
    keys("open owned order", b"\r")
    projection()
    frame("11-projection")
    keys("edit first line", b"\r")
    session.text("/value/line/0/quantity")
    keys("enter excessive quantity", b"9.0000\r")
    saved()
    keys("open second line", b"\x1bOQ\x1b[B\r")  # F2, Down, Enter
    session.text("/value/line/1/quantity")
    keys("leave second line blank", b"\r")
    saved()
    keys("edit receipt reference", b"\x1bOR")  # F3
    session.text("/value/receipt_reference")
    reference = prefix + "-REF"
    keys("set receipt reference", reference.encode() + b"\r")
    saved()
    keys("open locations", b"\x1bOS")  # F4
    session.text("location / list")
    keys("select owned second location", b"\x1b[B" * navigation["location_index"])
    session.until(lambda: any(
        ">" in row and prefix + "-B" in row for row in session.display.text().splitlines()
    ), "owned second location selection")
    frame("12-location")
    keys("save location", b"\r")
    session.text("Receive into " + prefix + "-B")
    keys("submit excessive quantity", b"\x13")
    session.text("quantity_exceeds_remaining")
    frame("13-refusal")
    refused = snapshot(db, "14-refused-db", ids)
    require(refused["claims"] == refused["receipts"] == 0, "refused command committed a claim or receipt")
    require([row["received"] for row in refused["lines"]] == ["0.0000", "0.0000"], "refused command changed received quantities")
    idle()
    require(snapshot(db, "15-refused-idle-db", ids) == refused, "database changed after refusal without operator action")
    keys("inspect retained reference", b"\x1bOR")
    session.text(reference + "_")
    frame("16-retained-reference")
    keys("cancel reference editor", b"\x1b")
    saved()
    keys("reopen first quantity", b"\x1bOQ\x1b[A\r")
    session.text("9.0000_")
    frame("17-retained-quantity")
    keys("correct quantity without rebuilding entry", b"\x7f" * 6 + b"2.5000\r")
    saved()
    keys("submit corrected entry", b"\x13")
    session.text("Recorded receipt ")
    session.text("Enter receive order")
    frame("18-success")
    committed = snapshot(db, "19-committed-db", ids)
    expected_line = {"line": ids.line1, "quantity": "2.5000", "location": ids.dock2}
    require(committed["claims"] == committed["receipts"] == 1, "successful entry did not produce exactly one claim and receipt")
    require(committed["receipt_lines"] == [expected_line], "receipt differs in quantity, location, or blank-line omission")
    require([row["received"] for row in committed["lines"]] == ["2.5000", "0.0000"], "received quantities differ from the entry")
    require(committed["order"] == {"status": "open", "row_version": 2}, "purchase order status or revision differs")
    canonical = committed["canonical"]
    require(len(canonical) == 1 and canonical[0]["purchase_order_id"] == ids.order
            and canonical[0]["receipt_reference"] == reference
            and canonical[0]["line"] == [{"purchase_order_line_id": ids.line1, "quantity": "2.5000", "location_id": ids.dock2}],
            "stored command differs from retained reference, numeric scale, or selected lines")
    idle()
    require(snapshot(db, "20-success-idle-db", ids) == committed, "database changed after success without operator action")
    keys("reopen order after success", b"\r")
    projection()
    keys("inspect new entry reference", b"\x1bOR")
    empty_modal("/value/receipt_reference")
    frame("21-new-entry")
    keys("cancel new reference", b"\x1b")
    saved()
    keys("inspect new entry quantity", b"\x1bOQ\r")
    empty_modal("/value/line/0/quantity")
    frame("22-new-quantity")
    keys("cancel new quantity", b"\x1b")
    saved()
    keys("leave new entry", b"\x1b")
    session.text("Discard draft")
    keys("discard new draft", b"y")
    session.text("Enter receive order")
    keys("quit operator", b"q")
    session.finish()
    frame("23-restored-terminal")
    return {"exit_code": session.process.returncode, "terminal_restored": True,
            "refusal": "quantity_exceeds_remaining", "retained_entry_corrected": True,
            "numeric_scale_preserved": True, "blank_line_omitted": True,
            "selected_location_persisted": True, "success_spent_entry": True,
            "committed_claims": 1, "committed_receipts": 1,
            "http_attempt_count": "not measured; idempotent replay can reuse the same database claim"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("binary", "operator-pat-file", "target-postgres-url-file", "evidence-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    for name in ("endpoint", "host", "target-instance"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args()
    evidence = session = db = ids = helper = None
    result = {"passed": False, "cleanup": False}
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
        helper, helper_path = load_terminal(root)
        helper.TIMEOUT = args.timeout
        helper.ROWS, helper.COLUMNS = 50, 260
        evidence = Evidence(args.evidence_dir, [token, database_url, unquote(urlsplit(database_url).password or "")])
        ids = SimpleNamespace(**{key: str(uuid.uuid4()) for key in ("order", "supplier", "item", "dock1", "dock2", "line1", "line2")})
        prefix = "TUI-" + uuid.uuid4().hex[:8]
        evidence.json("inputs.json", {
            "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
            "terminal_helper_sha256": hashlib.sha256(helper_path.read_bytes()).hexdigest(),
            "endpoint": args.endpoint, "host": args.host, "target_instance": args.target_instance,
            "terminal_rows": helper.ROWS, "terminal_columns": helper.COLUMNS,
            "fixture_prefix": prefix, "fixture_ids": vars(ids), "timeout_seconds": args.timeout,
        })
        db = Database(database_url, evidence)
        navigation = seed(db, ids, prefix)
        fixture = SimpleNamespace(url=args.endpoint, snapshot=lambda: [])
        evidence.event("launch", binary=str(binary), credential="private PAT file via WAMN_TOKEN")
        session = helper.Session(binary, root, fixture, args.target_instance, args.host, token)
        result.update(drive(session, db, evidence, ids, prefix, navigation))
        result["passed"] = True
    except (Exception, KeyboardInterrupt) as error:
        safe_types = (TestError, helper.TestError) if helper is not None else (TestError,)
        result["error"] = str(error) if isinstance(error, safe_types) else type(error).__name__
    finally:
        if session is not None:
            if evidence is not None:
                evidence.write("terminal.ansi", bytes(session.output).decode("utf-8", "replace"))
                evidence.write("last-frame.txt", session.display.text() + "\n")
            try:
                session.close()
            except Exception as error:
                result["terminal_cleanup_error"] = type(error).__name__
                result["passed"] = False
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
    print("Receiving live PTY test " + ("passed" if result["passed"] else "failed"))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
