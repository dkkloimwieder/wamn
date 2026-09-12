#!/usr/bin/env python3
"""One-use execution record for the September 2026 app moves; never installed."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
from datetime import datetime, timezone
import subprocess
import sys
import time

# The build tool calls this transparent adapter only to record actual Cargo argv.
if len(sys.argv) > 1 and sys.argv[1] == '--cargo':
    with open(os.environ['GUEST_CARGO_LOG'], 'a', encoding='utf-8') as stream:
        stream.write(json.dumps({'argv': sys.argv[2:], 'cwd': os.getcwd(),
                                 'target': os.environ.get('CARGO_TARGET_DIR'),
                                 'rustflags': os.environ.get('RUSTFLAGS', '')}) + '\n')
    os.execv(os.environ['GUEST_REAL_CARGO'], [os.environ['GUEST_REAL_CARGO'], *sys.argv[2:]])

parser = argparse.ArgumentParser()
parser.add_argument('phase', choices=['prepare', 'build-a', 'build-b', 'compare'])
parser.add_argument('--tree-a', type=Path, required=True)
parser.add_argument('--tree-b', type=Path, required=True)
parser.add_argument('--evidence', type=Path, required=True)
parser.add_argument('--test-root', type=Path)
args = parser.parse_args()
trees = {'a': args.tree_a.resolve(), 'b': args.tree_b.resolve()}
evidence = args.evidence.resolve()
assert trees['a'] != trees['b'], 'independent checkout paths are required'
assert all(evidence != tree and tree not in evidence.parents for tree in trees.values())
evidence.mkdir(parents=True, exist_ok=True)
real_cargo = shutil.which('cargo')
assert real_cargo
script = Path(__file__).resolve()
workspace_manifests = ['apps/Cargo.toml', 'apps/platform/no-std/Cargo.toml']

def arm_name(selector):
    return 'all' if selector == 'proof' else selector

def write_json(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

def read_json(name):
    return json.loads((evidence / name).read_text())

def base_env():
    env = os.environ.copy()
    for key in ['CARGO', 'CARGO_TARGET_DIR', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS']:
        env.pop(key, None)
    env.update(CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='', RUSTC_WORKSPACE_WRAPPER='')
    return env

def execute(name, argv, tree, env=None, json_stdout=False):
    started = time.monotonic()
    started_utc = datetime.now(timezone.utc).isoformat()
    output = evidence / (name + ('.json' if json_stdout else '.log'))
    error = evidence / (name + '-stderr.log')
    assert not output.exists() and not error.exists(), f'preserve earlier command output: {name}'
    env = env or base_env()
    with output.open('wb') as out, error.open('wb') as err:
        result = subprocess.run(argv, cwd=tree, env=env, stdout=out, stderr=err)
    write_json(name + '-command.json', {
        'argv': [str(value) for value in argv], 'cwd': str(tree),
        'environment': {key: env.get(key) for key in [
            'CARGO_BUILD_JOBS', 'CARGO_TARGET_DIR', 'RUSTC_WRAPPER',
            'RUSTC_WORKSPACE_WRAPPER', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS']},
        'started_utc': started_utc, 'ended_utc': datetime.now(timezone.utc).isoformat(),
        'exit_code': result.returncode, 'seconds': round(time.monotonic() - started, 3),
        'stdout': output.name, 'stderr': error.name,
        'disk_free_bytes_after': shutil.disk_usage(tree).free,
    })
    if result.returncode:
        raise RuntimeError(f'{name}: exit {result.returncode}; see {error}')
    return json.loads(output.read_text()) if json_stdout else output

def git(tree, *argv):
    return subprocess.check_output(['git', '-C', str(tree), *argv])

def snapshot(tree):
    paths = set(git(tree, 'ls-tree', '-r', '--name-only', '-z', 'HEAD').split(b'\0'))
    paths.update(git(tree, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').split(b'\0'))
    result = {}
    for raw in sorted(paths - {b''}):
        relative = os.fsdecode(raw)
        if relative == '.beads' or relative.startswith('.beads/'):
            continue
        path = tree / relative
        if not path.exists() and not path.is_symlink():
            result[relative] = {'missing': True}
            continue
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            data = os.fsencode(os.readlink(path))
        else:
            assert stat.S_ISREG(info.st_mode), f'unexpected source type: {relative}'
            data = path.read_bytes()
        result[relative] = {'mode': oct(stat.S_IMODE(info.st_mode)),
                            'type': 'symlink' if path.is_symlink() else 'file',
                            'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
    return result

def expected_outputs(metadata, selected):
    outputs = {}
    supporting = []
    for manifest, document in zip(workspace_manifests, metadata):
        for package in document['packages']:
            if package['id'] not in document['workspace_members'] or package['name'] not in selected:
                continue
            targets = [target for target in package['targets']
                       if 'cdylib' in target['crate_types'] or target['kind'] == ['bin']]
            if not targets:
                supporting.append(package['name'])
            for target in targets:
                name = target['name'] + '.wasm'
                assert name not in outputs, f'colliding guest output: {name}'
                outputs[name] = {'package': package['name'], 'workspace_manifest': manifest}
    assert outputs, 'no Wasm-producing Cargo members'
    return {'outputs': outputs, 'supporting_packages': sorted(supporting)}

def source_unchanged(side):
    current = snapshot(trees[side])
    write_json(side + '-source-after.json', current)
    assert current == read_json(side + '-source-before.json'), f'{side} source bytes or modes changed'

def collect_raw(side, profile):
    expected = read_json(side + '-expected.json')[arm_name(profile)]
    directory = trees[side] / 'target' / ('guest-digest-' + arm_name(profile)) / 'wasm32-wasip2/release'
    actual = {path.name: path for path in directory.glob('*.wasm')}
    assert set(actual) == set(expected['outputs']), {
        'missing': sorted(set(expected['outputs']) - set(actual)),
        'extra': sorted(set(actual) - set(expected['outputs']))}
    result = {}
    for name, path in sorted(actual.items()):
        data = path.read_bytes()
        assert data, f'empty guest: {path}'
        result[name] = {**expected['outputs'][name], 'bytes': len(data),
                        'sha256': hashlib.sha256(data).hexdigest()}
    write_json(side + '-' + arm_name(profile) + '-raw.json', result)
    (evidence / (side + '-' + arm_name(profile) + '-raw.sha256')).write_text(
        ''.join(f'{item["sha256"]}  {name}\n' for name, item in result.items()))
    return result

def check_invocations(side, profile, plan):
    expected = read_json(side + '-expected.json')[arm_name(profile) + '_legs']
    assert plan['build'] == expected, 'selection differs from actual workspace/app declarations'
    calls = [json.loads(line) for line in (evidence / (side + '-' + arm_name(profile) + '-cargo.jsonl')).read_text().splitlines()]
    builds = [call for call in calls if call['argv'][0] == 'build']
    wanted = [['build', '--locked', '--offline', '--release', '--target', 'wasm32-wasip2',
               '--manifest-path', str(trees[side] / leg['manifest']), '-p', leg['packages'][0]]
              for leg in expected]
    assert [call['argv'] for call in builds] == wanted, 'guest Cargo argv/count drift'
    assert all(call['rustflags'] == '--remap-path-prefix=' + str(trees[side]) + '=/wamn' for call in builds)
    write_json(side + '-' + arm_name(profile) + '-invocations.json',
               {'passed': True, 'build_count': len(builds), 'one_package_per_invocation': True})

def instrumented_env(side, profile):
    env = base_env()
    env.update(CARGO_TARGET_DIR=str(trees[side] / 'target' / ('guest-digest-' + arm_name(profile))),
               CARGO=str(evidence / 'cargo-argv'), GUEST_REAL_CARGO=real_cargo,
               GUEST_CARGO_LOG=str(evidence / (side + '-' + arm_name(profile) + '-cargo.jsonl')))
    return env

try:
    if args.phase == 'prepare':
        assert not (evidence / 'prepared.json').exists(), 'use fresh evidence directory'
        # Preserve the exact one-off script as historical execution evidence.
        shutil.copy2(script, evidence / 'capture.py')
        adapter = evidence / 'cargo-argv'
        adapter.write_text('#!/usr/bin/env python3\nimport os,sys\nos.execv(sys.executable, [sys.executable, ' +
                           repr(str(evidence / 'capture.py')) + ', "--cargo", *sys.argv[1:]])\n')
        adapter.chmod(0o755)
        for side, tree in trees.items():
            for profile in ['proof', 'app']:
                assert not (tree / 'target' / ('guest-digest-' + arm_name(profile))).exists(), 'targets must start absent'
            before = snapshot(tree)
            write_json(side + '-source-before.json', before)
            (evidence / (side + '-commit.txt')).write_bytes(git(tree, 'rev-parse', 'HEAD'))
            (evidence / (side + '-source.diff')).write_bytes(git(tree, 'diff', '--binary', 'HEAD'))
            (evidence / (side + '-status.txt')).write_bytes(git(tree, 'status', '--short'))
            env = instrumented_env(side, 'proof')
            metadata = [execute(side + '-metadata-' + str(index),
                        [real_cargo, 'metadata', '--manifest-path', str(tree / manifest),
                         '--locked', '--offline', '--no-deps', '--format-version', '1'], tree, env, True)
                        for index, manifest in enumerate(workspace_manifests)]
            apps = sorted(path.parent.relative_to(tree).as_posix() for path in (tree / 'apps').glob('*/wamn.json'))
            components = {name.replace('_', '-') for app in apps
                          for name in json.loads((tree / app / 'wamn.json').read_text()).get('components', {})}
            assert apps and components
            members = [(manifest, package['name']) for manifest, doc in zip(workspace_manifests, metadata)
                       for package in doc['packages'] if package['id'] in doc['workspace_members']]
            all_names = {name for _, name in members}
            assert components <= all_names, 'an app component is missing from the guest workspaces'
            legs = [{'manifest': manifest, 'packages': [name]} for manifest, name in sorted(members)]
            write_json(side + '-expected.json', {
                'applications': apps,
                'all': expected_outputs(metadata, all_names),
                'app': expected_outputs(metadata, components),
                'all_legs': legs,
                'app_legs': [leg for leg in legs if leg['packages'][0] in components],
            })
            source_unchanged(side)
        assert read_json('a-source-before.json') == read_json('b-source-before.json'), 'checkout source/mode mismatch'
        assert read_json('a-expected.json') == read_json('b-expected.json'), 'checkout selection mismatch'
        write_json('prepared.json', {'passed': True, 'disk_free_bytes': shutil.disk_usage(trees['a']).free,
                                    'script_sha256': hashlib.sha256(script.read_bytes()).hexdigest(),
                                    'note': 'metadata and source checks only; no build executed'})
    elif args.phase in ['build-a', 'build-b']:
        side = args.phase[-1]
        assert read_json('prepared.json')['passed']
        source_unchanged(side)
        tree = trees[side]
        for profile in ['proof', 'app']:
            assert not (tree / 'target' / ('guest-digest-' + arm_name(profile))).exists(), 'targets must start absent'
        try:
            for profile in ['proof', 'app']:
                env = instrumented_env(side, profile)
                command = ['./tools/build-components', 'build-only', profile]
                if profile == 'app':
                    command.extend(read_json(side + '-expected.json')['applications'])
                plan = execute(side + '-' + arm_name(profile), command, tree, env, True)
                assert plan['profile'] == profile
                check_invocations(side, profile, plan)
                raw = collect_raw(side, profile)
                for artifact in plan['virtualization']['artifacts']:
                    assert raw[Path(artifact['raw']).name]['sha256'] == artifact['sha256']
                if profile == 'proof':
                    execute(side + '-virtualize', ['./tools/build-components', 'virtualize-only',
                            str(evidence / (side + '-all.json'))], tree, env)
                    outputs = {}
                    for artifact in plan['virtualization']['artifacts']:
                        path = Path(artifact['output'])
                        data = path.read_bytes()
                        assert data
                        outputs[path.name] = {'package': artifact['package'], 'bytes': len(data),
                                              'sha256': hashlib.sha256(data).hexdigest()}
                    parents = {str(Path(item['output']).parent) for item in plan['virtualization']['artifacts']}
                    assert len(parents) == 1
                    assert {path.name for path in Path(next(iter(parents))).glob('*.wasm')} == set(outputs)
                    write_json(side + '-virtualized.json', outputs)
            write_json(side + '-build-result.json', {'passed': True})
        finally:
            source_unchanged(side)
    else:
        assert args.test_root, '--test-root is required for the retained comparison tests'
        for side in trees:
            assert read_json(side + '-build-result.json')['passed']
            source_unchanged(side)
        assert read_json('a-all-raw.json') == read_json('b-all-raw.json'), 'all raw bytes differ between checkouts'
        assert read_json('a-app-raw.json') == read_json('b-app-raw.json'), 'app raw bytes differ between checkouts'
        assert read_json('a-virtualized.json') == read_json('b-virtualized.json'), 'virtualized bytes differ between checkouts'
        for side in trees:
            all_outputs = read_json(side + '-all-raw.json')
            for name, output in read_json(side + '-app-raw.json').items():
                assert output == all_outputs[name], f'{side} selection changes {name}'
        native_env = base_env()
        for side in trees:
            plan = read_json(side + '-all.json')
            native_env['WAMN_DIGEST_REPRO_' + side.upper()] = str(Path(plan['virtualization']['artifacts'][0]['output']).parent)
        prefix = [real_cargo, 'test', '--locked', '--offline', '-p', 'wamn-proof-conformance', '--test', 'guest_workspace_closure']
        suffix = ['--', '--include-ignored', '--exact', '--nocapture']
        test_output = execute('virtualized-comparison', [*prefix, 'one_commit_built_in_two_checkouts_yields_identical_guest_digests', *suffix], args.test_root.resolve(), native_env)
        assert 'test result: ok. 1 passed; 0 failed; 0 ignored;' in test_output.read_text(), 'cross-checkout test did not execute exactly once'
        for side in trees:
            native_env['WAMN_DIGEST_PROFILE_APP_PLAN'] = str(evidence / (side + '-app.json'))
            native_env['WAMN_DIGEST_PROFILE_PROOF_PLAN'] = str(evidence / (side + '-all.json'))
            test_output = execute(side + '-app-all-comparison', [*prefix, 'one_commit_built_under_two_profiles_yields_identical_guest_digests', *suffix], args.test_root.resolve(), native_env)
            assert 'test result: ok. 1 passed; 0 failed; 0 ignored;' in test_output.read_text(), 'cross-selection test did not execute exactly once'
        write_json('comparison-result.json', {
            'passed': True, 'raw_wasm_count': len(read_json('a-all-raw.json')),
            'app_wasm_count': len(read_json('a-app-raw.json')),
            'virtualized_wasm_count': len(read_json('a-virtualized.json')),
            'same_source_bytes_and_modes': True,
            'cross_checkout_raw_equal': True, 'cross_checkout_virtualized_equal': True,
            'cross_selection_raw_equal': True,
            'retained_tests': 3,
        })
except Exception as error:
    write_json(args.phase + '-failure.json', {'passed': False, 'error': str(error)})
    raise
