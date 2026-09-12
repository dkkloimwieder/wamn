#!/usr/bin/env python3
"""Exercise the actual kubectl creation/readiness wait against a local API fixture."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import json
import subprocess
import threading
import time
import urllib.parse

root = Path(__file__).parent
results = []
for make_ready in (True, False):
    observations = []
    pod = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "sampler", "namespace": "proof", "uid": "sampler", "resourceVersion": "1", "labels": {"job-name": "sampler"}}, "spec": {"containers": [{"name": "probe", "image": "fixture"}]}, "status": {"phase": "Pending", "conditions": [{"type": "Ready", "status": "False"}]}}
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass
        def do_GET(self):
            request = urllib.parse.urlparse(self.path)
            query = urllib.parse.parse_qs(request.query)
            observations.append({"path": request.path, "query": query})
            if request.path == "/api":
                value = {"kind": "APIVersions", "apiVersion": "v1", "versions": ["v1"], "serverAddressByClientCIDRs": []}
            elif request.path == "/apis":
                value = {"kind": "APIGroupList", "apiVersion": "v1", "groups": []}
            elif request.path == "/api/v1":
                value = {"kind": "APIResourceList", "apiVersion": "v1", "groupVersion": "v1", "resources": [{"name": "pods", "singularName": "pod", "namespaced": True, "kind": "Pod", "verbs": ["get", "list", "watch"]}]}
            elif request.path.endswith("/pods/sampler"):
                if make_ready:
                    pod["status"]["phase"] = "Running"
                    pod["status"]["conditions"][0]["status"] = "True"
                    pod["metadata"]["resourceVersion"] = "3"
                value = pod
            elif request.path.endswith("/pods"):
                count = sum(x["path"].endswith("/pods") for x in observations)
                if count == 1:
                    items = []
                else:
                    if make_ready and count >= 3:
                        pod["status"]["phase"] = "Running"
                        pod["status"]["conditions"][0]["status"] = "True"
                        pod["metadata"]["resourceVersion"] = str(count)
                    items = [pod]
                value = {"kind": "PodList", "apiVersion": "v1", "metadata": {"resourceVersion": str(count)}, "items": items}
                if query.get("watch") == ["true"]:
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.end_headers()
                    self.wfile.write((json.dumps({"type": "ADDED", "object": pod}) + "\n").encode())
                    bookmark = {"apiVersion": "v1", "kind": "Pod", "metadata": {"resourceVersion": pod["metadata"]["resourceVersion"], "annotations": {"k8s.io/initial-events-end": "true"}}}
                    self.wfile.write((json.dumps({"type": "BOOKMARK", "object": bookmark}) + "\n").encode())
                    self.wfile.flush()
                    if not make_ready:
                        time.sleep(4)
                    return
            else:
                self.send_error(404)
                return
            data = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    command = ["kubectl", "--kubeconfig=/dev/null", "--server=http://127.0.0.1:" + str(server.server_port), "--request-timeout=5s", "-n", "proof", "wait", "--for=create", "--for=condition=Ready", "pod", "-l", "job-name=sampler", "--timeout=2s"]
    result = subprocess.run(command, capture_output=True, text=True, timeout=10)
    server.shutdown()
    server.server_close()
    row = {"ready": make_ready, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr, "command": command, "requests": observations}
    results.append(row)
    (root / ("ready.json" if make_ready else "not-ready.json")).write_text(json.dumps(row, indent=2) + "\n")
    assert (result.returncode == 0) == make_ready, result.stderr
(root / "receipt.json").write_text(json.dumps({"result": "pass", "scope": "actual kubectl against synthetic API transitions, not a cluster recovery proof", "cases": results}, indent=2) + "\n")
print(json.dumps({"result": "pass", "actual_kubectl_cases": len(results)}))
