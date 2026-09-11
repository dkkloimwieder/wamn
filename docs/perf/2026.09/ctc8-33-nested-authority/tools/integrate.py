#!/usr/bin/env python3
"""Fast-forward only the approved nested-authority change. Preserve unrelated local work."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

ROOT = Path(__file__).resolve().parents[5]
PREFIX = "docs/perf/2026.09/ctc8-33-nested-authority/"
ACTIVE = tuple(f"docs/perf/2026.09/{name}/" for name in (
    "receiving-postcommit", "receiving-update-projection", "native-b-adoption", "native-f-retained"))
# The nine source and documentation paths for wamn-ctc8.33. Follow-up commits get no wider scope.
ALLOWED = frozenset((
    "crates/execution/host/src/router_driver.rs",
    "crates/platform/runtime/src/plugins/connection_http.rs",
    "crates/platform/runtime/src/plugins/wamn_blobstore/plugin.rs",
    "crates/platform/runtime/src/plugins/wamn_postgres/claims.rs",
    "docs/exe-model.md",
    "docs/operations/build-and-test.md",
    "docs/perf/2026.09/ctc8-16-http-reuse/tools/live.sh",
    "services/ctl/tests/bind_connection_live.rs",
    "tests/integration/src/trusted_http_route.rs",
))


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT,
                                   env={**os.environ, "GIT_OPTIONAL_LOCKS": "0"})


def paths(*args):
    return set(filter(None, git(*args, "-z").decode().split("\0")))


def dirty():
    return paths("diff", "--name-only", "--no-renames") | paths(
        "diff", "--cached", "--name-only", "--no-renames")


def local_path(name):
    path = ROOT / name
    require(not Path(name).is_absolute() and ".." not in Path(name).parts, "unsafe relative path")
    require(all(not parent.is_symlink() for parent in path.parents if parent != ROOT.parent),
            f"symlink ancestor refused: {name}")
    return path


def snapshot(names):
    result = {}
    for name in sorted(names):
        path = local_path(name)
        if not path.exists() and not path.is_symlink():
            result[name] = None
            continue
        mode = stat.S_IMODE(path.lstat().st_mode)
        if path.is_symlink():
            result[name] = {"symlink": os.readlink(path), "mode": mode}
        else:
            require(path.is_file(), f"non-file local work refused: {name}")
            with path.open("rb") as source:
                digest = hashlib.file_digest(source, "sha256").hexdigest()
            result[name] = {"sha256": digest, "mode": mode}
    return result


def write(path, value):
    with path.open("x") as output:
        json.dump(value, output, indent=2)
        output.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True)
    parser.add_argument("--head", required=True)
    parser.add_argument("--run-name", required=True)
    args = parser.parse_args()
    require(re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,79}", args.run_name), "invalid evidence run name")
    base, head = [git("rev-parse", "--verify", "--end-of-options", value + "^{commit}").decode().strip()
                  for value in (args.base, args.head)]
    require(git("branch", "--show-current").strip() == b"main", "checkout must be main")
    require(git("rev-parse", "HEAD").decode().strip() == base, "main no longer equals supplied base")
    git("merge-base", "--is-ancestor", base, head)
    changed = paths("diff", "--name-only", "--no-renames", base, head)
    require(changed, "incoming commit has no changes")
    require(all(name in ALLOWED or name.startswith(PREFIX) for name in changed),
            "incoming paths exceed the explicit nested-authority allowlist")
    require(not git("ls-files", "--unmerged", "-z"), "index has unresolved entries")
    require(not changed & dirty(), "incoming paths collide with staged or unstaged changes")
    run_prefix = PREFIX + args.run_name + "/"
    require(not any(name.startswith(run_prefix) for name in changed), "incoming tree occupies evidence run")
    evidence = local_path(run_prefix.rstrip("/"))
    require(not evidence.exists() and not evidence.is_symlink(), "evidence run already exists")
    tracked = paths("ls-files")
    collisions = {name for name in changed - tracked
                  if local_path(name).exists() or local_path(name).is_symlink()}
    require(all(name.startswith(PREFIX) for name in collisions), "untracked collision outside own evidence")
    originals = snapshot(collisions)
    for name in sorted(collisions):
        path = local_path(name)
        require(path.is_file() and not path.is_symlink(), f"non-regular collision refused: {name}")
        entry = git("ls-tree", head, "--", name).split(b"\t", 1)[0].split()
        require(len(entry) == 3 and entry[0] in (b"100644", b"100755") and entry[1] == b"blob",
                f"incoming collision is not a regular blob: {name}")
        require(path.read_bytes() == git("cat-file", "blob", entry[2].decode()),
                f"untracked bytes differ from incoming blob: {name}")
        require(bool(originals[name]["mode"] & stat.S_IXUSR) == (entry[0] == b"100755"),
                f"untracked executable mode differs from incoming blob: {name}")

    def stable_names():
        names = paths("ls-files") | paths("ls-files", "--others", "--exclude-standard") | dirty()
        return {name for name in names - changed if not name.startswith((*ACTIVE, run_prefix))}

    def stable_index():
        return sorted(line.decode() for line in git("ls-files", "--stage", "-v", "-z").split(b"\0")
                      if line and line.split(b"\t", 1)[1].decode() not in changed
                      and not line.split(b"\t", 1)[1].decode().startswith(ACTIVE))

    before, index_before = snapshot(stable_names()), stable_index()
    evidence.mkdir(parents=True, exist_ok=False)
    write(evidence / "before.json", {"base": base, "head": head, "incoming_paths": sorted(changed),
          "stable_files": before, "stable_index_entries": index_before, "collisions": originals,
          "concurrent_evidence_not_compared": ACTIVE})
    moved = []
    merge = None
    try:
        for name in sorted(collisions):
            require(snapshot([name])[name] == originals[name], f"collision changed before backup: {name}")
            backup = evidence / "backups" / name
            backup.parent.mkdir(parents=True, exist_ok=True)
            require(not backup.exists(), "backup destination already exists")
            local_path(name).rename(backup)
            moved.append(name)
        require(git("rev-parse", "HEAD").decode().strip() == base, "main moved during preparation")
        require(snapshot(stable_names()) == before and stable_index() == index_before,
                "unrelated work changed during preparation")
        merge = subprocess.run(["git", "-c", "core.hooksPath=/dev/null", "merge", "--ff-only",
                                "--no-autostash", "--no-stat", head], cwd=ROOT,
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        with (evidence / "merge.log").open("xb") as output:
            output.write(merge.stdout)
        merge.check_returncode()
        for name in moved:
            local_path(name).chmod(originals[name]["mode"])
        require(snapshot(collisions) == originals, "reconciled evidence bytes or modes changed")
        require(git("rev-parse", "HEAD").decode().strip() == head, "unexpected final HEAD")
        require(not changed & dirty(), "incoming tree is dirty after fast-forward")
        require(snapshot(stable_names()) == before, "unrelated file bytes, modes, or names changed")
        require(stable_index() == index_before, "unrelated index entries changed")
    except BaseException:
        restored = []
        for name in reversed(moved):
            destination = local_path(name)
            if destination.exists() or destination.is_symlink():
                displaced = evidence / "failed-checkout" / name
                displaced.parent.mkdir(parents=True, exist_ok=True)
                require(not displaced.exists() and not displaced.is_symlink(), "recovery destination exists")
                destination.rename(displaced)
            (evidence / "backups" / name).rename(destination)
            restored.append(name)
        write(evidence / "failure.json", {"passed": False, "base": base, "head": head,
              "actual_head": git("rev-parse", "HEAD").decode().strip(),
              "merge_exit": merge.returncode if merge is not None else None,
              "restored_original_files": restored, "manual_inspection_required": True,
              "git_refs_or_index_rolled_back": False})
        raise
    result = {"passed": True, "base": base, "head": head, "incoming_files": len(changed),
              "preserved_stable_files": len(before), "preserved_index_entries": len(index_before),
              "reconciled_own_files": len(moved), "backups_retained": moved,
              "concurrent_evidence_not_compared": ACTIVE, "merge_exit": merge.returncode}
    write(evidence / "result.json", result)
    print(json.dumps(result, separators=(",", ":")))


if __name__ == "__main__":
    main()
