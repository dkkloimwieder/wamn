#!/usr/bin/env python3
"""Fast-forward B while preserving exact owned evidence collisions and other work."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

TREE = Path('/home/kaalin/dev/wamn')
PREFIX = 'docs/perf/2026.09/native-b-adoption/'


def git(*args):
    return subprocess.check_output(['git', *args], cwd=TREE)


def paths(*args):
    return [os.fsdecode(p) for p in git(*args).split(b'\0') if p]


def identity(path):
    info = path.lstat()
    data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
    return {'mode': stat.S_IMODE(info.st_mode), 'sha256': hashlib.sha256(data).hexdigest(),
            'symlink': path.is_symlink()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-main', required=True)
    parser.add_argument('--incoming', required=True)
    parser.add_argument('--run-name', required=True)
    args = parser.parse_args()
    assert git('branch', '--show-current').strip() == b'main'
    assert git('rev-parse', 'HEAD').decode().strip() == args.expected_main
    incoming = git('rev-parse', '--verify', args.incoming + '^{commit}').decode().strip()
    subprocess.run(['git', 'merge-base', '--is-ancestor', args.expected_main, incoming], cwd=TREE, check=True)
    assert not git('diff', '--cached', '--name-only'), 'Preserve the nonempty shared index'
    dirty = paths('diff', '--name-only', '-z')
    assert all(path.startswith('.beads/') for path in dirty), dirty
    untracked = set(paths('ls-files', '--others', '--exclude-standard', '-z'))
    incoming_paths = set(paths('ls-tree', '-r', '--name-only', '-z', incoming))
    changed = set(paths('diff', '--name-only', '-z', args.expected_main, incoming))
    assert not set(dirty) & changed
    collisions = sorted(untracked & incoming_paths)
    assert all(path.startswith(PREFIX) for path in collisions), collisions
    for path in collisions:
        local = TREE / path
        assert local.is_file() and not local.is_symlink(), path
        assert local.read_bytes() == git('show', incoming + ':' + path), path
        mode = git('ls-tree', incoming, '--', path).decode().split()[0]
        assert mode == ('100755' if local.stat().st_mode & 0o111 else '100644'), path
    preserved = {path: identity(TREE / path) for path in sorted((untracked - set(collisions)) | set(dirty))}
    output = TREE / PREFIX / args.run_name
    assert output.parent == TREE / PREFIX
    output.mkdir(exist_ok=False)
    backup = Path(tempfile.mkdtemp(prefix='wamn-native-b-integration-', dir='/tmp'))
    receipt = {'before': args.expected_main, 'incoming': incoming, 'backup': str(backup),
               'collisions': {path: identity(TREE / path) for path in collisions}, 'preserved': preserved}
    (output / 'before.json').write_text(json.dumps(receipt, indent=2) + '\n')
    (output / 'integrate.py').write_bytes(Path(__file__).read_bytes())
    removed = []
    try:
        for path in collisions:
            saved = backup / path
            saved.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(TREE / path, saved)
            assert identity(saved) == receipt['collisions'][path]
            (TREE / path).unlink()
            removed.append(path)
        with (output / 'merge.log').open('wb') as log:
            result = subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', 'merge', '--ff-only', incoming],
                                    cwd=TREE, stdout=log, stderr=subprocess.STDOUT)
        assert result.returncode == 0, 'Merge failed; owned backups remain recorded'
        assert git('rev-parse', 'HEAD').decode().strip() == incoming
        for path in collisions:
            assert (TREE / path).read_bytes() == (backup / path).read_bytes(), path
            # Git records the executable bit, but not the other permission bits.
            (TREE / path).chmod(receipt['collisions'][path]['mode'])
            assert identity(TREE / path) == receipt['collisions'][path], path
        differences = [path for path, value in preserved.items() if identity(TREE / path) != value]
        assert not differences, differences
        assert not git('diff', '--cached', '--name-only')
        (output / 'result.json').write_text(json.dumps({'exit_code': 0, 'head': incoming,
            'exact_collisions_preserved': len(collisions), 'other_inputs_preserved': len(preserved),
            'backup_retained': str(backup)}, indent=2) + '\n')
    finally:
        # On refusal, restore only paths that the failed merge left absent.
        for path in removed:
            if not (TREE / path).exists():
                shutil.copy2(backup / path, TREE / path)


if __name__ == '__main__':
    main()
