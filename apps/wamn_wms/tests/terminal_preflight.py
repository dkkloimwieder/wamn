#!/usr/bin/env python3
"""Exercise the WMS terminal driver against HTTP and database fixtures.

This preflight drives the actual example binary through both response modes.
It does not test platform execution, database commits, or object-store writes.
"""

import argparse
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import sys
import threading
from types import SimpleNamespace

sys.dont_write_bytecode = True


class HttpFixture:
    def __init__(self, ids, mode, partial_failure):
        self.errors = []
        fixture = self
        self.movement = {"movement_id": "44444444-0000-0000-0000-000000000009",
                         "pallet_id": ids.pallet, "location_id": ids.destination,
                         "pallet_status": "available", "row_version": 2}

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                    if self.path == "/pallet/get":
                        value = {"id": ids.pallet, "location_id": ids.source,
                                 "pallet_code": "WMS-TUI-PREFLIGHT", "row_version": 1,
                                 "status": "available", "created_at": "2026-09-10T10:00:00Z",
                                 "updated_at": "2026-09-10T10:00:00Z"}
                        status, body = 200, [{"request_id": request[0]["request_id"], "value": value}]
                    elif self.path == "/inventory/move":
                        value = dict(fixture.movement)
                        envelope = [{"request_id": request[0]["request_id"], "value": value}]
                        if mode == "success":
                            value.update(zpl="^XA^FO10,10^FDSynthetic WMS preflight^FS^XZ",
                                         stored={"container": "labels", "key": value["movement_id"]})
                            status, body = 200, envelope
                        else:
                            status, body = 500, {"committed_result": envelope,
                                                 "failed_outcome": partial_failure}
                    else:
                        raise ValueError("unexpected fixture route")
                    payload = json.dumps(body).encode()
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except Exception as error:
                    fixture.errors.append(type(error).__name__)
                    self.send_error(500, "HTTP fixture failed")

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.url = "http://127.0.0.1:" + str(self.server.server_port)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)


class DatabaseFixture:
    """Return synthetic observations from the request captured by the relay."""

    def __init__(self, relay, evidence, ids, movement):
        self.relay, self.evidence, self.ids, self.movement = relay, evidence, ids, movement

    def sql(self, name, _sql, parse=False):
        if not parse:
            raise ValueError("the preflight database only supplies JSON observations")
        records = [record for record in self.relay.snapshot() if record["path"] == "/inventory/move"]
        command = json.loads(records[0]["request_body"])[0]["value"]
        ids = self.ids
        observation = {
            "claims": len(records), "movements": len(records),
            "command": {"idempotency_key": command["idempotency_key"], "movement_id": self.movement["movement_id"],
                        "pallet_id": command["pallet_id"], "pallet_status": "available", "row_version": 2},
            "movement": {"idempotency_key": command["idempotency_key"], "pallet_id": command["pallet_id"],
                         "product_id": ids.product, "from_location_id": ids.source,
                         "to_location_id": command["to_location_id"], "kind": "move", "quantity": "10.0000"},
            "pallet": {"location_id": command["to_location_id"], "status": "available", "row_version": 2},
            "quantity": [{"product_id": ids.product, "quantity": "10.0000", "status": "available"}],
        }
        self.evidence.json(name + ".synthetic.json", {
            "source": "synthetic database fixture; no SQL executed", "observation": observation})
        return observation


def run_mode(driver, tree, binary, directory, mode, failure, timeout):
    token, host = "wms-preflight-fixture-token", "wms-preflight.localhost"
    evidence = driver.support.Evidence(directory, [token])
    helper, _ = driver.support.load_terminal(tree)
    helper.TIMEOUT = timeout
    helper.ROWS, helper.COLUMNS = 60, 260
    ids = SimpleNamespace(pallet="44444444-0000-0000-0000-000000000001",
                          product="44444444-0000-0000-0000-000000000002",
                          source="44444444-0000-0000-0000-000000000003",
                          destination="44444444-0000-0000-0000-000000000004")
    result = {"passed": False, "mode": mode, "scope": "synthetic terminal preflight", "platform_test": False}
    fixture = relay = session = None
    try:
        fixture = HttpFixture(ids, mode, failure)
        relay = driver.Relay(fixture.url, host, token, timeout)
        database = DatabaseFixture(relay, evidence, ids, fixture.movement)
        session = helper.Session(binary, tree, relay, "synthetic-wms-preflight", host, token)
        result["driver_assertions"] = driver.drive(session, relay, database, evidence, ids, mode)
        driver.require(not fixture.errors, "the HTTP fixture reported an error")
        result["passed"] = True
    except (Exception, KeyboardInterrupt) as error:
        safe_types = (driver.support.TestError, helper.TestError)
        result["error"] = str(error) if isinstance(error, safe_types) else type(error).__name__
    finally:
        if session is not None:
            evidence.write("terminal.ansi", bytes(session.output).decode("utf-8", "replace"))
            evidence.write("last-frame.txt", session.display.text() + "\n")
        for resource in (session, relay, fixture):
            if resource is not None:
                try:
                    resource.close()
                except Exception as error:
                    result["cleanup_error"] = type(error).__name__
                    result["passed"] = False
        if relay is not None:
            evidence.json("http-fixture.json", relay.snapshot())
        evidence.json("result.json", result)
        evidence.sums()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--timeout", type=float, default=15.0)
    args = parser.parse_args()
    tree = args.tree.resolve(strict=True)
    binary = (args.binary or tree / "target/debug/examples/wms_move").resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK) or args.timeout <= 0:
        parser.error("the binary must be executable and the timeout must be positive")
    driver_path = tree / "apps/wamn_wms/tests/wms_pty.py"
    spec = importlib.util.spec_from_file_location("wms_live_driver", driver_path)
    driver = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(driver)
    partial_path = tree / "evidence/perf/2026.09/effects-response/live-002/journey/wms-partial-http.json"
    partial = json.loads(json.loads(partial_path.read_text())["body"])
    evidence = driver.support.Evidence(args.evidence_dir, [])
    evidence.json("inputs.json", {
        "scope": "synthetic terminal preflight", "platform_test": False, "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "driver_sha256": hashlib.sha256(driver_path.read_bytes()).hexdigest(),
        "preflight_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "partial_failure_source": str(partial_path.relative_to(tree)),
        "partial_failure_source_sha256": hashlib.sha256(partial_path.read_bytes()).hexdigest(),
    })
    modes = [run_mode(driver, tree, binary, args.evidence_dir / mode, mode, partial["failed_outcome"], args.timeout)
             for mode in ("success", "partial")]
    result = {"passed": all(mode["passed"] for mode in modes), "platform_test": False,
              "scope": "synthetic terminal preflight", "modes": modes}
    evidence.json("result.json", result)
    evidence.sums()
    print(json.dumps(result, sort_keys=True))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
