#!/usr/bin/env python3
"""Run the Receiving fixture's native startup proof and verify server exposure."""
import base64
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time
from urllib.parse import unquote, urlencode, urlsplit


def main():
    document = json.loads(Path(sys.argv[1]).read_text())
    fixture = document['fixture']
    private = Path(fixture['private_dir'])
    evidence = Path(fixture['evidence_dir'])
    evidence.mkdir()
    kube = ['kubectl', '--kubeconfig', document['kubeconfig'],
            '--context', document['context']]
    children = []
    final = {'source': fixture['source'], 'verdict': 'fail', 'cleanup': []}
    commands = []
    stage = 'fixture identity'
    code = 1

    def interrupted(signum, frame):
        for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(number, signal.SIG_IGN)
        raise KeyboardInterrupt('journey interrupted')

    for number in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(number, interrupted)

    def capture(command, filename, budget=30):
        command_receipt = {'argv': command, 'output': filename, 'budget_seconds': budget}
        commands.append(command_receipt)
        began = time.monotonic()
        try:
            completed = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                       timeout=budget, check=False)
        except subprocess.TimeoutExpired:
            command_receipt['timed_out'] = True
            raise
        finally:
            command_receipt['elapsed_seconds'] = time.monotonic() - began
        command_receipt['exit_code'] = completed.returncode
        sanitized = redact(completed.stdout).encode()
        (evidence / filename).write_bytes(sanitized)
        if completed.returncode:
            # These are private-kubeconfig reads of public objects, not Secret reads.
            (evidence / (filename + '.stderr')).write_text(redact(completed.stderr))
            raise RuntimeError('public evidence command failed')
        return sanitized

    try:
        redactions = set()
        for name in ('identity-reader', 'guest-sql', 'executor-platform',
                     'http-admitter', 'event-materializer'):
            url = json.loads((Path(fixture['host_secrets']) / (name + '.json')).read_text())['stringData']['url']
            redactions.add(url)
            password = urlsplit(url).password
            if password:
                redactions.update((password, unquote(password)))
        pat = json.loads(Path(fixture['pat_secret']).read_text())['stringData']['token']
        redactions.add(pat)
        for entry in json.loads(Path(fixture['registry_auth']).read_text())['auths'].values():
            for key in ('auth', 'password', 'identitytoken', 'registrytoken'):
                if entry.get(key):
                    redactions.add(entry[key])
            if entry.get('auth'):
                pair = base64.b64decode(entry['auth'], validate=True).decode()
                redactions.update((pair, pair.split(':', 1)[1]))

        def redact(raw):
            text = raw.decode(errors='replace')
            for value in sorted((v for v in redactions if v), key=len, reverse=True):
                text = text.replace(value, '<redacted>')
            return text

        deployment = json.loads(Path(document['host_deployment']).read_text())
        host = next(c for c in deployment['spec']['template']['spec']['containers'] if c['name'] == 'host')
        limits = [entry.get('value', '') for entry in host['env']
                  if entry['name'] == 'WASH_MAX_CONCURRENT_STARTS']
        if (len(limits) != 1 or not limits[0].isdigit() or int(limits[0]) < 1
                or any(argument.split('=', 1)[0] == '--max-concurrent-starts'
                       for argument in host.get('args', []))):
            raise RuntimeError('deployed host has no unique explicit chart native start limit')
        fixture['max_concurrent_starts'] = int(limits[0])
        final['deployment_resources'] = host['resources']
        final['local_process_resource_limit'] = 'inherits runner cgroup; not the Kubernetes 6-CPU quota'
        with open(fixture['host_binary'], 'rb') as binary:
            final['host_binary_sha256'] = hashlib.file_digest(binary, 'sha256').hexdigest()
        final['profile'] = 'release'
        final['source_requirements'] = {
            'wasmtime_parallel_compilation': 'Cargo.toml workspace Wasmtime features, captured by source SHA',
            'guest_memory_mode': 'count', 'meter_mode': 'duration',
            'start_limit': fixture['max_concurrent_starts']}
        for name, path in [('Cargo.toml', Path(document['repo']) / 'Cargo.toml'),
                           ('Cargo.lock', Path(document['repo']) / 'Cargo.lock'),
                           ('host-deployment.json', Path(document['host_deployment'])),
                           ('production-workload.json', Path(fixture['workload']))]:
            final.setdefault('input_sha256', {})[name] = hashlib.sha256(path.read_bytes()).hexdigest()

        stage = 'owned OTLP port forward'
        forward_log = private / 'otlp-port-forward.log'
        command = kube + ['-n', document['system_namespace'], 'port-forward',
                          'deployment/otel-collector', ':4317', '--address=127.0.0.1']
        commands.append(command)
        with forward_log.open('wb') as log:
            forward = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT,
                                       start_new_session=True)
        children.append(forward)
        deadline = time.monotonic() + 120  # Existing collector/host readiness budget.
        while True:
            if forward.poll() is not None:
                raise RuntimeError('OTLP port forward exited')
            match = re.search(r'^Forwarding from 127\.0\.0\.1:(\d+) -> 4317$',
                              forward_log.read_text(), re.M)
            if match:
                fixture['otlp_endpoint'] = 'http://127.0.0.1:' + match[1]
                break
            if time.monotonic() >= deadline:
                raise TimeoutError('OTLP port forward did not become ready')
            time.sleep(0.05)
        input_path = private / 'input.json'
        input_path.write_text(json.dumps(fixture))
        input_path.chmod(0o600)

        stage = 'real native startup integration proof'
        command = ['cargo', 'test', '-p', 'wamn-proof-integration', '--locked', '--offline',
                   '--test', 'startup_burst_live', '--', '--ignored', '--exact',
                   'production_http_start_burst_keeps_native_host_progress',
                   '--nocapture', '--test-threads=1']
        commands.append(command)
        environment = os.environ.copy()
        environment.update({'CARGO_TARGET_DIR': document['cargo_target_dir'],
                            'RUSTC_WRAPPER': '', 'WAMN_STARTUP_BURST_INPUT': str(input_path)})
        private_test_log = private / 'test.raw.log'
        with private_test_log.open('wb') as log:
            test = subprocess.Popen(command, cwd=document['repo'], env=environment,
                                    stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        children.append(test)
        # Sum of existing heartbeat/readiness, two start RPC, exact-owned stop
        # RPC and host cleanup budgets. This is a harness deadline, not a latency gate.
        test_budget = 2 * 120 + 2 * 30 + 4 * fixture['max_concurrent_starts'] * 30 + 70 + 5
        final['test_exit_code'] = test.wait(timeout=test_budget)
        test_log = redact(private_test_log.read_bytes())
        (evidence / 'test.log').write_text(test_log)
        if final['test_exit_code'] != 0 or not re.search(
                r'^test result: ok\. 1 passed; 0 failed;', test_log, re.M):
            raise RuntimeError('real startup proof did not execute exactly one passing test')
        protocol = json.loads((evidence / 'protocol.json').read_text())
        if protocol['verdict'] != 'protocol-pass-awaiting-trace-exposure':
            raise RuntimeError('startup protocol receipt did not pass')
        stage = 'native trace collection'
        expected = set(protocol['owned_workloads'])
        proxy = '/api/v1/namespaces/' + document['system_namespace'] + '/services/http:tempo:3200/proxy'
        query = '{ resource.wamn.startup.proof = "' + fixture['proof_id'] + '" && name = "workload_start" }'
        search_path = proxy + '/api/search?' + urlencode({'q': query, 'limit': 1000})
        deadline = time.monotonic() + 120  # Existing trace/recovery observation budget.
        spans = {}
        span_sources = {}
        attempt = 0
        while True:
            attempt += 1
            search = json.loads(capture(kube + ['get', '--raw', search_path],
                                        f'trace-search-{attempt:03}.json'))
            for trace in search.get('traces', []):
                trace_id = trace['traceID']
                if not re.fullmatch('[0-9a-fA-F]{32}', trace_id):
                    raise RuntimeError('Tempo returned a malformed trace identity')
                trace_file = f'native-trace-{attempt:03}-{trace_id}.json'
                raw = capture(kube + ['get', '--raw', proxy + '/api/traces/' + trace_id], trace_file)
                value = json.loads(raw)
                for batch in value.get('batches', []):
                    for scope in batch.get('scopeSpans', []):
                        for span in scope.get('spans', []):
                            key = (trace_id, span['spanId'])
                            spans[key] = span
                            span_sources[key] = trace_file
            observed = {attribute.get('value', {}).get('stringValue')
                        for span in spans.values() if span['name'] == 'workload_start'
                        for attribute in span.get('attributes', []) if attribute['key'] == 'workload_id'}
            if expected <= observed:
                break
            if time.monotonic() >= deadline:
                raise TimeoutError('native start spans did not arrive for every owned workload')
            time.sleep(1)

        stage = 'measured server overlap and progress'
        phases = {}
        for phase in ('cold', 'warm'):
            ids = {entry['id'] for entry in protocol[phase]['starts']}
            starts = []
            for key, span in spans.items():
                attrs = {a['key']: a.get('value', {}).get('stringValue') for a in span.get('attributes', [])}
                if span['name'] == 'workload_start' and attrs.get('workload_id') in ids:
                    if int(span['endTimeUnixNano']) <= int(span['startTimeUnixNano']):
                        raise RuntimeError('native start span has no positive duration')
                    starts.append({'id': attrs['workload_id'], 'trace_id': key[0],
                                   'span_id': key[1], 'source_file': span_sources[key],
                                   'start_ns': int(span['startTimeUnixNano']),
                                   'end_ns': int(span['endTimeUnixNano'])})
            if len(starts) != len(ids) or len({s['id'] for s in starts}) != len(ids):
                raise RuntimeError('native start span identity is missing or duplicated')
            events = sorted([(s['start_ns'], 1) for s in starts] + [(s['end_ns'], -1) for s in starts])
            active = maximum = 0
            for _, delta in events:
                active += delta
                maximum = max(maximum, active)
            if maximum <= fixture['max_concurrent_starts']:
                raise RuntimeError('insufficient measured native queued-start exposure')
            continuous = []
            for span in sorted(starts, key=lambda span: span['start_ns']):
                if continuous and span['start_ns'] <= continuous[-1][1]:
                    continuous[-1][1] = max(continuous[-1][1], span['end_ns'])
                else:
                    continuous.append([span['start_ns'], span['end_ns']])
            origin = int(protocol[phase]['started_unix_ns'])
            # Require all four sequential operations to finish inside a period
            # when a native start handler is continuously active. Mere overlap
            # could put the heartbeat/probes before all native starts.
            progress = [o for o in protocol[phase]['observations']
                        if any(start <= origin + int(o['started_seconds'] * 1e9)
                               and origin + int(o['finished_seconds'] * 1e9) <= end
                               for start, end in continuous)
                        and o['native_live_status'] == 200 and o['native_ready_status'] == 200
                        and (phase == 'cold' or o['application']['status'] == 200)]
            if not progress:
                raise RuntimeError('no measured native/serving progress during server start intervals')
            phases[phase] = {'native_starts': starts, 'max_overlapping_start_handlers': maximum,
                             'complete_progress_observations_within_continuous_start_intervals': len(progress),
                             'continuous_start_intervals_ns': continuous,
                             'first_success_seconds': protocol[phase]['first_success_seconds'],
                             'all_running_seconds': protocol[phase]['all_running_seconds']}
        raw_host = re.sub(rb'\x1b\[[0-9;]*m', b'', (private / 'host.raw.log').read_bytes())
        if not re.search(rb'max_concurrent_starts=' + str(fixture['max_concurrent_starts']).encode() + rb'\b', raw_host):
            raise RuntimeError('native host log did not confirm the configured start limit')
        final.update({'verdict': 'pass', 'phases': phases,
                      'cache_scope': protocol['cache_scope'],
                      'phase_attribution': protocol['phase_attribution'],
                      'host_ready_seconds': protocol['host_ready_seconds'],
                      'first_success_since_process_start_seconds': (int(protocol['cold']['started_unix_ns']) - int(protocol['process_started_unix_ns'])) / 1e9 + protocol['cold']['first_success_seconds'],
                      'occupancy_limit': 'workload_start begins before the permit; overlap proves queued demand, not active permit occupancy or CPU-core use',
                      'comparison_limit': 'Distinct from a herd of different cold digests; local host cgroup and new native probe semantics differ from historical in-cluster timings. Existing performance modes are unchanged.'})
        code = 0
    except BaseException as error:
        final['failure'] = {'stage': stage, 'error_type': type(error).__name__}
        if 'redact' in locals():
            final['failure']['message'] = redact(str(error).encode())
        code = 130 if isinstance(error, KeyboardInterrupt) else 1
        print('native startup proof failed; see startup-burst/result.json', file=sys.stderr)
    finally:
        # Each child owns its own process group. Also terminate descendants left
        # by a failed test, even when its cargo parent already exited.
        for child in reversed(children):
            killed = False
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            deadline = time.monotonic() + 70
            while True:
                child.poll()  # Reap the owned group leader when it exits.
                try:
                    os.killpg(child.pid, 0)
                except ProcessLookupError:
                    break
                if time.monotonic() >= deadline:
                    killed = True
                    try:
                        os.killpg(child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    break
                time.sleep(0.05)
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                killed = True
            final['cleanup'].append({'pid': child.pid, 'exit_code': child.returncode,
                                     'wait_exceeded_host_grace': killed})
            if killed:
                code = 1
                final['verdict'] = 'fail'
        for raw_name, public_name in [('host.raw.log', 'host.log'), ('test.raw.log', 'test.log'),
                                      ('failure.raw.log', 'failure.log'),
                                      ('otlp-port-forward.log', 'otlp-port-forward.log')]:
            raw = private / raw_name
            if raw.exists() and 'redact' in locals():
                (evidence / public_name).write_text(redact(raw.read_bytes()))
        (evidence / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
        (evidence / 'result.json').write_text(json.dumps(final, indent=2) + '\n')
        (evidence / 'evidence.sha256').write_text(''.join(
            hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.name + '\n'
            for path in sorted(evidence.iterdir()) if path.name != 'evidence.sha256'))
    return code


if __name__ == '__main__':
    sys.exit(main())
