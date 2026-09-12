#!/usr/bin/env python3
"""Record one native C command and its source identity."""
import hashlib
import json
import pathlib
import subprocess
import sys
import time

worktree = pathlib.Path(sys.argv[1]).resolve()
out = pathlib.Path(sys.argv[2]).resolve()
command = sys.argv[3:]
out.mkdir(parents=True, exist_ok=True)
assert not (out / "command.json").exists(), "each capture requires a fresh directory"

def source():
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=worktree, text=True).strip()
    paths = subprocess.check_output(["git", "ls-files", "-co", "--exclude-standard"], cwd=worktree, text=True).splitlines()
    owned = [p for p in paths if p.startswith(("components/execution/materializer/", "crates/platform/runtime/src/plugins/wamn_jetstream", "crates/platform/runtime/src/plugins/effect_span", "crates/platform/runtime/wit/", "components/events/wire/", "services/cdc-reader/", "services/ctl/src/event_advisories", "services/ctl/src/lib.rs", "services/ctl/src/bin/wamn-ctl-ops.rs", "crates/platform/runtime/src/native_nats", "crates/platform/runtime/tests/native_nats", "crates/platform/runtime/tests/jetstream_wit_coherence", "tests/integration/src/route_authentication_live", "tests/conformance/tests/effect_spans", "tools/receiving-postcommit-proof", "tools/receiving-cluster-journey-run", "tools/wms-cluster-journey-run", "services/ctl/tests/verb_surface.rs", "deploy/infra/nats-jetstream.yaml", "deploy/platform/materializer.example.yaml")) or p.endswith(("Cargo.toml", "Cargo.lock"))]
    hashes = {p: hashlib.sha256((worktree / p).read_bytes()).hexdigest() for p in sorted(set(owned)) if (worktree / p).is_file()}
    return {"head": head, "files": hashes}

before = source()
(out / "source-before.json").write_text(json.dumps(before, indent=2) + "\n")
(out / "command.json").write_text(json.dumps({"cwd": str(worktree), "argv": command, "capture_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest()}, indent=2) + "\n")
start = time.monotonic()
with (out / "output.log").open("w") as log:
    result = subprocess.run(command, cwd=worktree, stdout=log, stderr=subprocess.STDOUT)
after = source()
(out / "source-after.json").write_text(json.dumps(after, indent=2) + "\n")
receipt = {"exit_code": result.returncode, "elapsed_seconds": time.monotonic() - start, "source_unchanged": before == after}
(out / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps(receipt), flush=True)
sys.exit(result.returncode)
