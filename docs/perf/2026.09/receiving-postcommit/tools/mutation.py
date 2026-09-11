#!/usr/bin/env python3
"""Prepare and restore the two Receiving post-commit defect controls."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess

PACKAGE = "packages/client_acme_receiving/"
MANIFEST = PACKAGE + "wamn.json"
REPLAY_SQL = PACKAGE + "command/create_inspection/load_inspection.sql"
TIMEOUT_RUST = "components/application/client-acme-receiving/src/lib.rs"
GENERATED = tuple(PACKAGE + "generated/" + name for name in (
    "wamn/quality_create_inspection.rs",
    "contracts/quality/create_inspection.operation.json",
    "source-map/quality_create_inspection.json",
    "platform-policy/data-access.json",
    "package-weld.json",
))
SQL_BEFORE = b"SELECT receipt_id\nFROM quality_inspection\nWHERE receipt_id = $1;\n"
SQL_AFTER = (b"UPDATE quality_inspection\nSET status = 'pending', row_version = 1\n"
             b"WHERE receipt_id = $1\nRETURNING receipt_id;\n")
TIMEOUT_BEFORE = b"AccessErrorKind::Retry | AccessErrorKind::Timeout => NodeError::Retryable(detail),"
TIMEOUT_AFTER = (b"AccessErrorKind::Retry => NodeError::Retryable(detail),\n"
                 b"        AccessErrorKind::Timeout => NodeError::Terminal(detail),")


def require(condition, detail):
    if not condition:
        raise RuntimeError(detail)


def git(tree, *arguments):
    environment = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    return subprocess.check_output(["git", "-C", str(tree), *arguments], text=True,
                                   env=environment).strip()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def read_file(path):
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode), f"Expected a regular file: {path}")
    data = path.read_bytes()
    return data, {"sha256": digest(data), "mode": stat.S_IMODE(before.st_mode),
                  "atime_ns": before.st_atime_ns, "mtime_ns": before.st_mtime_ns}


def replace_once(data, old, new, label):
    require(data.count(old) == 1, f"{label}: expected exactly one original match")
    return data.replace(old, new, 1)


def mutations(control, originals):
    if control == "timeout-terminal":
        return {TIMEOUT_RUST: replace_once(originals[TIMEOUT_RUST], TIMEOUT_BEFORE,
                                           TIMEOUT_AFTER, "timeout mapping")}
    require(originals[REPLAY_SQL] == SQL_BEFORE, "Replay SQL differs from the exact original statement")
    text = originals[MANIFEST].decode()
    marker = '"quality.create_inspection": '
    require(text.count(marker) == 1, "Manifest must name exactly one create_inspection operation")
    start = text.index(marker) + len(marker)
    operation, end = json.JSONDecoder().raw_decode(text, start)
    relations = operation["relations"]
    require(len(relations) == 1 and relations[0]["schema"] == "receiving"
            and relations[0]["table"] == "quality_inspection"
            and relations[0]["update_fields"] == [], "Replay relation differs from its original declaration")
    body = replace_once(text[start:end].encode(), b'"update_fields": []',
                        b'"update_fields": ["status", "row_version"]', "replay update fields")
    return {REPLAY_SQL: replace_once(originals[REPLAY_SQL], SQL_BEFORE, SQL_AFTER, "replay SQL"),
            MANIFEST: text[:start].encode() + body + text[end:].encode()}


def paths_for(control):
    return ((REPLAY_SQL, MANIFEST) if control == "replay-reset" else (TIMEOUT_RUST,)) + GENERATED


def save(evidence, state):
    temporary = evidence / "state.next.json"
    temporary.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n")
    temporary.replace(evidence / "state.json")


def prepare(tree, evidence, control):
    require(not evidence.exists(), "Preparation requires a new evidence directory")
    require(not git(tree, "status", "--porcelain", "--untracked-files=normal", "--", ".",
                    ":(exclude).beads/issues.jsonl", ":(exclude).beads/interactions.jsonl"),
            "Prepare only a clean, inactive worktree")
    files = paths_for(control)
    git(tree, "ls-files", "--error-unmatch", "--", *files)
    originals, records = {}, {}
    for relative in files:
        data, metadata = read_file(tree / relative)
        originals[relative] = data
        records[relative] = {"before": metadata, "expected": metadata.copy()}
    changed = mutations(control, originals)
    evidence.mkdir()
    for relative, data in originals.items():
        backup = evidence / "backups" / relative
        backup.parent.mkdir(parents=True, exist_ok=True)
        backup.write_bytes(data)
    state = {"schema": "receiving-postcommit-mutation/v1", "control": control,
             "worktree": str(tree), "source_before": git(tree, "rev-parse", "HEAD"),
             "status": "captured", "files": records, "mutated_paths": sorted(changed)}
    for relative, data in changed.items():
        records[relative]["expected"]["sha256"] = digest(data)
    save(evidence, state)
    for relative, data in changed.items():
        current, metadata = read_file(tree / relative)
        require(digest(current) == records[relative]["before"]["sha256"]
                and metadata["mode"] == records[relative]["before"]["mode"],
                f"Source changed after capture: {relative}")
        (tree / relative).write_bytes(data)
    for relative in files:
        _, metadata = read_file(tree / relative)
        require(metadata["sha256"] == records[relative]["expected"]["sha256"],
                f"Prepared bytes differ: {relative}")
        records[relative]["expected"] = metadata
    state["status"] = "prepared"
    save(evidence, state)
    return state


def load(tree, evidence):
    state = json.loads((evidence / "state.json").read_text())
    require(state["schema"] == "receiving-postcommit-mutation/v1" and state["worktree"] == str(tree),
            "Evidence belongs to another worktree or tool format")
    require(state["control"] in ("replay-reset", "timeout-terminal")
            and set(state["files"]) == set(paths_for(state["control"])),
            "Evidence contains an unexpected restoration path")
    for relative, record in state["files"].items():
        backup, _ = read_file(evidence / "backups" / relative)
        require(digest(backup) == record["before"]["sha256"], f"Backup hash differs: {relative}")
    return state


def unchanged(tree, state, paths, key="expected"):
    for relative in paths:
        _, actual = read_file(tree / relative)
        expected = state["files"][relative][key]
        require(actual["sha256"] == expected["sha256"] and actual["mode"] == expected["mode"],
                f"Refusing to overwrite unexpected bytes or mode: {relative}")


def seal_generated(tree, evidence, state):
    require(state["status"] == "prepared" and state["control"] == "replay-reset",
            "Seal generated files once, after replay-reset regeneration")
    unchanged(tree, state, state["mutated_paths"])
    changed = []
    for relative in GENERATED:
        _, metadata = read_file(tree / relative)
        if metadata["sha256"] != state["files"][relative]["before"]["sha256"]:
            changed.append(relative)
        state["files"][relative]["expected"] = metadata
    require(set(changed) == set(GENERATED), "Normal replay regeneration must change the five recorded outputs")
    state["generated_paths"] = changed
    state["source_sealed"] = git(tree, "rev-parse", "HEAD")
    state["status"] = "generated-sealed"
    save(evidence, state)


def restore(tree, evidence, state):
    require(state["status"] in ("prepared", "generated-sealed", "restored"),
            "Preparation did not complete; inspect its retained backups before recovery")
    key = "before" if state["status"] == "restored" else "expected"
    unchanged(tree, state, state["files"], key)
    for relative, record in state["files"].items():
        path = tree / relative
        original = record["before"]
        path.write_bytes((evidence / "backups" / relative).read_bytes())
        path.chmod(original["mode"])
        require(digest(path.read_bytes()) == original["sha256"], f"Restored bytes differ: {relative}")
        os.utime(path, ns=(original["atime_ns"], original["mtime_ns"]))
        observed = path.stat()
        require(stat.S_IMODE(observed.st_mode) == original["mode"]
                and observed.st_atime_ns == original["atime_ns"]
                and observed.st_mtime_ns == original["mtime_ns"], f"Restored metadata differs: {relative}")
        record["restored"] = original.copy()
    state["source_restored"] = git(tree, "rev-parse", "HEAD")
    state["status"] = "restored"
    save(evidence, state)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "seal-generated", "restore"))
    parser.add_argument("--inactive-worktree", type=Path, required=True,
                        help="Explicit inactive, separate worktree; never the active proof source")
    parser.add_argument("--evidence-dir", type=Path, required=True)
    parser.add_argument("--control", choices=("replay-reset", "timeout-terminal"))
    args = parser.parse_args()
    tree = args.inactive_worktree.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    require((tree / ".git").is_file() and Path(git(tree, "rev-parse", "--show-toplevel")) == tree,
            "The target must be a separate Git worktree")
    root = Path(__file__).resolve().parents[1]
    require(evidence.is_relative_to(root) and evidence != root and not evidence.is_relative_to(root / "tools"),
            "Evidence must be a new run directory under main docs/perf/2026.09/receiving-postcommit")
    if args.action == "prepare":
        require(args.control is not None, "Preparation requires --control")
        state = prepare(tree, evidence, args.control)
    else:
        require(args.control is None, "Restoration and sealing read the control from retained evidence")
        state = load(tree, evidence)
        if args.action == "seal-generated":
            seal_generated(tree, evidence, state)
        else:
            restore(tree, evidence, state)
    print(json.dumps({"status": state["status"], "control": state["control"], "evidence": str(evidence)}))


if __name__ == "__main__":
    main()
