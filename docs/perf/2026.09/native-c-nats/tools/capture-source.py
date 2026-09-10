#!/usr/bin/env python3
"""Record the source behind the native NATS policy checkpoint."""

import hashlib
import json
from pathlib import Path
import subprocess
import sys

root, upstream, output = map(Path, sys.argv[1:])
output.mkdir(parents=True, exist_ok=False)
files = {
    "wamn": [
        "components/execution/materializer/src/main.rs",
        "components/execution/materializer/wit/world.wit",
        "crates/execution/host/src/router_delivery.rs",
        "crates/platform/runtime/src/plugins/wamn_jetstream.rs",
        "docs/architecture/wamn_native_alignment_plan.md",
    ],
    "upstream": [
        "crates/wash-runtime/src/plugin/wasmcloud_nats/mod.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/plugin.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/keys.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/mod.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/pull_consumer.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/message_handle.rs",
        "crates/wash-runtime/src/plugin/wasmcloud_nats/jetstream.rs",
        "wit/nats/wit/world.wit",
        "crates/wash-runtime/Cargo.toml",
        "crates/wash-runtime/src/plugin/component_host/mod.rs",
    ],
}
receipt = {"commands": [], "sources": {}, "tests_executed": 0}
for name, directory in [("wamn", root), ("upstream", upstream)]:
    for command in [
        ["git", "rev-parse", "HEAD"],
        ["git", "status", "--porcelain", "--", *files[name]],
    ]:
        result = subprocess.run(command, cwd=directory, capture_output=True, text=True)
        receipt["commands"].append({
            "cwd": str(directory), "argv": command, "exit_code": result.returncode,
            "stdout": result.stdout, "stderr": result.stderr,
        })
        result.check_returncode()
    receipt["sources"][name] = {
        path: hashlib.sha256((directory / path).read_bytes()).hexdigest()
        for path in files[name]
    }
receipt["capture_tool_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
(output / "source.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps({"commands": len(receipt["commands"]), "source_files": sum(map(len, files.values())), "tests_executed": 0}))
