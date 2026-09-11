#!/usr/bin/env python3
"""Execute the changed shell blocks against a bounded loopback HTTP fixture."""

import argparse
from collections import Counter
import hashlib
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import uuid


parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--source-tree", type=Path, required=True)
parser.add_argument("--evidence-dir", type=Path, required=True)
args = parser.parse_args()
runner_path = args.source_tree / "tools/receiving-cluster-journey-run"
source = runner_path.read_text()
committed = subprocess.check_output(
    ["git", "show", "HEAD:tools/receiving-cluster-journey-run"],
    cwd=args.source_tree,
    text=True,
)
prior = Path(__file__).resolve().parents[2] / "wasmcloud-2-9-cutover/live-receiving-009/journey"
args.evidence_dir.mkdir(mode=0o700)


def block(start, end):
    offset = source.index(start)
    return source[offset:source.index(end, offset)]


blocks = [
    block("record_command() {", "assert_can_i() {"),
    block("  postcommit_probe_exit=0\n", "else\nprobe_manifest="),
    block("if [[ $(grep -Fc 'RECEIVING_HTTP_PROBE '", "# Prepare the genuine post-commit trigger."),
    block("materializer_update_body=", 'if [[ -z "$receiving_postcommit" ]]; then\n'),
    block("  materializer_curl_config=$work_dir/materializer-curl.conf\n", 'else\nrun "${kube[@]}" apply -f "$materializer_trigger"'),
    block("sed -n 's/^RECEIVING_MATERIALIZER_UPDATE //p'", "# The materializer phase's inputs"),
]
script = "set -euo pipefail\n" + "\n".join(blocks)
script_path = args.evidence_dir / "extracted-shell.sh"
script_path.write_text(script)
subprocess.run(["bash", "-n", str(script_path)], check=True)
expected = {}
for name, path, suffix in [
    ("update", "/acme/purchase_order/update", "1111111111111111"),
    ("receipt", "/acme/receiving/record_receipt", "2222222222222222"),
]:
    request = re.search(r"--data '(\[.*?\])' \\\n\s*'http://flow-http\.\$namespace\.svc\.cluster\.local" + re.escape(path) + "'", committed)
    assert request is not None, path
    expected[path] = dict(
        body=request[1].encode(),
        trace=f"00-{'1' if name == 'update' else '2'}" + ("1" if name == "update" else "2") * 31 + f"-{suffix}-01",
        response=(prior / f"materializer-{name}.json").read_bytes(),
    )
results = []
for case in ["positive", "update-503-no-retry"]:
    case_dir = args.evidence_dir / case
    case_dir.mkdir()
    requests = []
    errors = []
    token = "synthetic-loopback-only-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix="postcommit-http-fixture-") as scratch:
        private = Path(scratch)
        secret = private / "route-caller.json"
        secret.write_text(json.dumps({"stringData": {"token": token}}))
        secret.chmod(0o600)

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def request(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                receipt = dict(method=self.command, path=self.path, host=self.headers.get("Host"))
                requests.append(receipt)
                try:
                    assert self.headers.get("Host") == "receiving.localhost"
                    if self.command == "GET":
                        assert self.path == "/no-such-route" and body == b""
                        status, response = 404, b'{"error":{"code":"route-not-found"}}'
                    else:
                        assert self.command == "POST" and self.path in expected
                        request = expected[self.path]
                        assert body == request["body"]
                        assert self.headers.get("Content-Type") == "application/json"
                        assert self.headers.get("Authorization") == "Bearer " + token
                        assert self.headers.get("traceparent") == request["trace"]
                        mode = (private / "materializer-curl.conf").stat().st_mode & 0o777
                        assert mode == 0o600
                        receipt.update(
                            exact_body=True,
                            exact_trace=True,
                            authorization_matches_synthetic_token=True,
                            private_config_mode=oct(mode),
                        )
                        status, response = 200, request["response"]
                        if case == "update-503-no-retry":
                            assert self.path == "/acme/purchase_order/update"
                            status, response = 503, b'{"error":"fixture-unavailable"}'
                    receipt["response_status"] = status
                except AssertionError:
                    errors.append(dict(method=self.command, path=self.path))
                    status, response = 400, b'{"error":"fixture-request-mismatch"}'
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(response)))
                self.end_headers()
                self.wfile.write(response)

            do_GET = request
            do_POST = request

        server = HTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        environment = dict(
            os.environ,
            evidence_dir=str(case_dir),
            work_dir=str(private),
            commands_record=str(case_dir / "commands.log"),
            postcommit_endpoint=f"http://127.0.0.1:{server.server_port}",
            route_host="receiving.localhost",
            route_caller_secret=str(secret),
            materializer_update_trace="1" * 32,
            materializer_receipt_trace="2" * 32,
        )
        try:
            process = subprocess.run(
                ["bash", str(script_path)], env=environment,
                capture_output=True, text=True, timeout=20, check=False,
            )
        finally:
            server.shutdown()
            server.server_close()
            thread.join()
        (case_dir / "stdout.log").write_text(process.stdout)
        (case_dir / "stderr.log").write_text(process.stderr)
        assert errors == [], errors
        counts = Counter((r["method"], r["path"]) for r in requests)
        wanted = {("GET", "/no-such-route"): 1, ("POST", "/acme/purchase_order/update"): 1}
        if case == "positive":
            wanted[("POST", "/acme/receiving/record_receipt")] = 1
            assert process.returncode == 0, process.stderr
        else:
            assert process.returncode != 0
        assert dict(counts) == wanted, dict(counts)
        assert all(token.encode() not in path.read_bytes() for path in case_dir.iterdir() if path.is_file())
        result = dict(
            case=case, exit_code=process.returncode, requests=requests,
            retained_artifacts_exclude_synthetic_token=True,
            no_post_retries=True, verdict="pass",
        )
        (case_dir / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        results.append(result)
receipt = dict(
    scope="loopback synthetic HTTP transport fixture; no cluster, image, or build",
    source=str(runner_path), source_sha256=hashlib.sha256(runner_path.read_bytes()).hexdigest(),
    extracted_shell_sha256=hashlib.sha256(script.encode()).hexdigest(),
    expected_requests_from="HEAD:tools/receiving-cluster-journey-run legacy Job payloads",
    responses_from=str(prior), cases=results, verdict="pass",
)
(args.evidence_dir / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps(receipt))
