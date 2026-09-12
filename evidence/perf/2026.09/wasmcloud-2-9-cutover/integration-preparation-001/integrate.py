#!/usr/bin/env python3
"""Fast-forward the cutover after preserving unrelated main changes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--candidate", required=True)
parser.add_argument("--evidence-dir", required=True, type=Path)
parser.add_argument("--apply", action="store_true")
args = parser.parse_args()
main = Path("/home/kaalin/dev/wamn")
lane = Path("/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909")
base = "dfa1c3187fe8cd671688a442b23106046e502cb6"
branch = "work/wasmcloud-2-9-20260909"
def git(path, *command):
    return subprocess.check_output(["git", "-c", "core.hooksPath=/dev/null", "-C", str(path), *command])
assert git(main, "rev-parse", "HEAD").decode().strip() == base
assert git(main, "branch", "--show-current").decode().strip() == "main"
assert git(lane, "rev-parse", "HEAD").decode().strip() == args.candidate
assert git(main, "rev-parse", branch).decode().strip() == args.candidate
assert not git(lane, "status", "--porcelain=v1").strip()
git(main, "merge-base", "--is-ancestor", base, args.candidate)
tree = {}
for row in git(main, "ls-tree", "-r", "-z", args.candidate).split(b"\0"):
    if row:
        metadata, path = row.split(b"\t", 1)
        mode, kind, oid = metadata.decode().split()
        tree[path.decode()] = (mode, kind, oid)
untracked = git(main, "ls-files", "--others", "--exclude-standard", "-z").decode().split("\0")
matching = []
for relative in untracked:
    if relative not in tree:
        continue
    path = main / relative
    data = os.readlink(path).encode() if path.is_symlink() else path.read_bytes()
    oid = hashlib.sha1(b"blob " + str(len(data)).encode() + b"\0" + data).hexdigest()
    assert oid == tree[relative][2], "untracked file differs: " + relative
    matching.append(relative)
# These user changes must remain byte-identical and keep their original index entries.
protected = [".beads/interactions.jsonl", ".beads/issues.jsonl", "docs/poc/wamn_testing_spec.md", "docs/poc/generated-tui-spec.md"]
protected_bytes = {p: (main / p).read_bytes() for p in protected if (main / p).exists()}
protected_index = git(main, "ls-files", "--stage", "-z", "--", *protected)
changed = set(git(main, "diff", "--name-only", "-z", base, args.candidate).decode().split("\0"))
assert not changed.intersection(protected), "candidate intersects unrelated user changes"
receipt = {"base": base, "candidate": args.candidate, "matching_untracked_files": len(matching),
           "protected_sha256": {p: hashlib.sha256(v).hexdigest() for p, v in protected_bytes.items()},
           "applied": False}
args.evidence_dir.mkdir(exist_ok=False)
(args.evidence_dir / "preflight.json").write_text(json.dumps(receipt, indent=2) + "\n")
(args.evidence_dir / "main-status-before.z").write_bytes(git(main, "status", "--porcelain=v1", "-z"))
if args.apply:
    manifest = args.evidence_dir / "matching-paths.z"
    manifest.write_bytes(b"".join(p.encode() + b"\0" for p in matching))
    if matching:
        git(main, "--literal-pathspecs", "add", "--pathspec-from-file=" + str(manifest), "--pathspec-file-nul")
    assert git(main, "ls-files", "--stage", "-z", "--", *protected) == protected_index
    merged = git(main, "merge", "--ff-only", branch)
    (args.evidence_dir / "merge.stdout").write_bytes(merged)
    assert git(main, "rev-parse", "HEAD").decode().strip() == args.candidate
    assert git(main, "ls-files", "--stage", "-z", "--", *protected) == protected_index
    for relative, data in protected_bytes.items():
        assert (main / relative).read_bytes() == data, "user bytes changed: " + relative
    receipt["applied"] = True
    (args.evidence_dir / "main-status-after.z").write_bytes(git(main, "status", "--porcelain=v1", "-z"))
(args.evidence_dir / "result.json").write_text(json.dumps(receipt, indent=2) + "\n")
print(json.dumps(receipt))
