#!/usr/bin/env python3
"""Check the proof's workload ownership assumptions against retained live objects."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
objects = {}
hashes = {}
for name in ("deployment", "replicaset", "workload"):
    path = root / f"materializer-{name}.json"
    data = path.read_bytes()
    objects[name] = json.loads(data)
    hashes[str(path)] = hashlib.sha256(data).hexdigest()
deployment, replica, workload = (objects[name] for name in objects)
assert deployment["spec"]["replicas"] == 1
assert any(row["type"] == "Ready" and row["status"] == "True"
           for row in deployment["status"]["conditions"])
assert deployment["status"]["currentReplicaSet"]["name"] == replica["metadata"]["name"]
assert any(owner["uid"] == deployment["metadata"]["uid"]
           for owner in replica["metadata"]["ownerReferences"])
assert any(owner["uid"] == replica["metadata"]["uid"]
           for owner in workload["metadata"]["ownerReferences"])
print(json.dumps({"result": "pass", "source_sha256": hashes,
                  "scope": "retained object shape and ownership only; no current live result"}, indent=2))
