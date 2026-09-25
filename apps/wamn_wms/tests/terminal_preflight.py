#!/usr/bin/env python3
"""Exercise the WMS terminal driver against direct-route HTTP and database fixtures.

This preflight drives the actual example binary through success and refusal modes.
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
    def __init__(self, ids, mode):
        self.errors = []
        fixture = self
        self.movement = {"operation_id": "44444444-0000-0000-0000-000000000009",
                         "inventory_id": ids.inventory, "product_id":ids.product,"packaging_id":ids.packaging_destination,
                         "location_id":ids.destination,"quantity":"10.0000","disposition":"available","lifecycle":"open","row_version":2}

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                try:
                    request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))) or b"[]")
                    if self.path.startswith("/inventory/get?"):
                        value = {"id":ids.inventory,"product_id":ids.product,"packaging_id":ids.packaging_source,
                                 "location_id":ids.source,"quantity":"10.0000","disposition":"available",
                                 "lifecycle":"open","row_version":1,"created_at":"2026-09-10T10:00:00Z"}
                        status, body = 200, [{"value": value}]
                    elif self.path == "/inventory/move":
                        value = dict(fixture.movement)
                        envelope = [{"request_id": request[0]["request_id"], "value": value}]
                        if mode == "success":
                            status, body = 200, envelope
                        else:
                            status, body = 200, [{"request_id":request[0]["request_id"],
                                "error":{"code":"invalid_input","detail":{"field":"value.to_packaging_id"}}}]
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

            do_GET = do_POST

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

    def __init__(self, relay, evidence, ids, movement, mode):
        self.relay, self.evidence, self.ids, self.movement = relay, evidence, ids, movement
        self.mode = mode

    def sql(self, name, _sql, parse=False):
        if not parse:
            raise ValueError("the preflight database only supplies JSON observations")
        records = [record for record in self.relay.snapshot() if record["path"] == "/inventory/move"]
        command = json.loads(records[0]["request_body"])[0]["value"]
        ids = self.ids
        observation = {
            "claims": len(records), "movements": len(records),
            "command": self.movement,
            "movement": {"operation_id":self.movement["operation_id"],"inventory_id":ids.inventory,
                         "from_location_id":ids.source,"to_location_id":ids.destination,
                         "from_packaging_id":ids.packaging_source,"to_packaging_id":ids.packaging_destination,
                         "type":"move","from_quantity":"10.0000","to_quantity":"10.0000"},
            "inventory": {"location_id":ids.destination,"packaging_id":ids.packaging_destination,
                          "disposition":"available","lifecycle":"open","quantity":"10.0000","row_version":2},
        }
        if self.mode == "refusal":
            observation.update(claims=0, movements=0, command=None, movement=None)
            observation["inventory"].update(location_id=ids.source, packaging_id=ids.packaging_source, row_version=1)
        self.evidence.json(name + ".synthetic.json", {
            "source": "synthetic database fixture; no SQL executed", "observation": observation})
        return observation


def run_mode(driver, tree, binary, directory, mode, timeout):
    token, host = "wms-preflight-fixture-token", "wms-preflight.localhost"
    evidence = driver.support.Evidence(directory, [token])
    helper, _ = driver.support.load_terminal(tree)
    helper.TIMEOUT = timeout
    helper.ROWS, helper.COLUMNS = 60, 260
    ids = SimpleNamespace(inventory="44444444-0000-0000-0000-000000000001",
                          product="44444444-0000-0000-0000-000000000002",
                          source="44444444-0000-0000-0000-000000000003",
                          destination="44444444-0000-0000-0000-000000000004",
                          packaging_source="44444444-0000-0000-0000-000000000005",
                          packaging_destination="44444444-0000-0000-0000-000000000006")
    result = {"passed": False, "mode": mode, "scope": "synthetic terminal preflight", "platform_test": False}
    fixture = relay = session = None
    try:
        fixture = HttpFixture(ids, mode)
        relay = driver.Relay(fixture.url, host, token, timeout)
        database = DatabaseFixture(relay, evidence, ids, fixture.movement, mode)
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
    evidence = driver.support.Evidence(args.evidence_dir, [])
    evidence.json("inputs.json", {
        "scope": "synthetic terminal preflight", "platform_test": False, "binary": str(binary),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "driver_sha256": hashlib.sha256(driver_path.read_bytes()).hexdigest(),
        "preflight_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    })
    modes = [run_mode(driver, tree, binary, args.evidence_dir / mode, mode, args.timeout)
             for mode in ("success", "refusal")]
    result = {"passed": all(mode["passed"] for mode in modes), "platform_test": False,
              "scope": "synthetic terminal preflight", "modes": modes}
    evidence.json("result.json", result)
    evidence.sums()
    print(json.dumps(result, sort_keys=True))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
