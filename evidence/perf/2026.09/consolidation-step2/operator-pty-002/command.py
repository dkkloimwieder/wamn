import contextlib
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time

root = Path('/home/kaalin/dev/wamn')
review = Path('/home/kaalin/.cache/wamn-lanes/consolidation-plan-20260911')
evidence = Path(sys.argv[1])
evidence.mkdir(parents=True, exist_ok=False)
(evidence / 'command.py').write_bytes(Path(__file__).read_bytes())
helper = review / 'crates/client/terminal/tests/operator_pty.py'
binary = root / 'target/debug/wamn-receiving'
files = [helper, binary] + [review / 'apps/wamn_receiving' / relative for relative in (
    'generated/contracts/purchase_order/query.operation.json',
    'generated/contracts/purchase_order/query.result.json',
    'publication/attachments.json',
    'publication/wirings/purchase_order_query.json',
)]
before = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}
(evidence / 'command.json').write_text(json.dumps({
    'argv': ['python3', '-B', str(helper), '--binary', str(binary)],
    'cwd': str(review),
    'instrumentation': 'Temporary in-process failure-frame capture and completed-check recording; scenario assertions unchanged.',
    'review_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=review, text=True).strip(),
    'binary_build_source': '52ac90a6400e4b955a90d3f97733c0c5be8a5418',
    'before_sha256': before,
}, indent=2) + '\n')
spec = importlib.util.spec_from_file_location('operator_capture', helper)
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)
checks = []
failures = []
original_init = module.Session.__init__
original_until = module.Session.until
original_finish = module.Session.finish
original_quiet = module.Session.quiet
original_verify = module.verify_request

def capture_init(self, binary, directory, fixture, instance):
    self.capture_instance = instance
    original_init(self, binary, directory, fixture, instance)

def capture_until(self, predicate, description, allow_exit=False):
    try:
        original_until(self, predicate, description, allow_exit)
    except BaseException as error:
        name = f'failure-{len(failures) + 1:02d}'
        frame = self.display.text().replace(module.TOKEN, '[REDACTED]')
        output = bytes(self.output).replace(module.TOKEN.encode(), b'[REDACTED]')
        (evidence / (name + '-frame.txt')).write_text(frame + '\n')
        (evidence / (name + '-terminal.ansi')).write_bytes(output)
        failures.append({'instance': self.capture_instance, 'wait': description,
                         'error_type': type(error).__name__, 'frame': name + '-frame.txt'})
        raise
    checks.append({'instance': self.capture_instance, 'wait_completed': description})

def capture_finish(self, unresolved=False, exit_code=0):
    original_finish(self, unresolved, exit_code)
    checks.append({'instance': self.capture_instance, 'finish_passed': True,
                   'unresolved': unresolved, 'exit_code': exit_code,
                   'terminal_and_warning_checks_passed': True})

def capture_quiet(self, count):
    original_quiet(self, count)
    checks.append({'instance': self.capture_instance, 'quiet_exact_request_count': count})

def capture_verify(request):
    result = original_verify(request)
    checks.append({'request_checks_passed': True, 'method': request.method,
                   'path': request.path, 'canonical_request_id': result})
    return result

module.Session.__init__ = capture_init
module.Session.until = capture_until
module.Session.finish = capture_finish
module.Session.quiet = capture_quiet
module.verify_request = capture_verify
sys.argv = [str(helper), '--binary', str(binary)]
started = time.monotonic()
with (evidence / 'stdout.log').open('w') as stdout, (evidence / 'stderr.log').open('w') as stderr:
    with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
        code = module.main()
after = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}
result = {'exit_code': code, 'elapsed_seconds': round(time.monotonic() - started, 3),
          'input_bytes_unchanged': before == after, 'after_sha256': after,
          'helper_is_uncommitted_delta': True,
          'binary_build_source': '52ac90a6400e4b955a90d3f97733c0c5be8a5418',
          'completed_checks': checks, 'failed_waits': failures,
          'scenario_passed': code == 0}
(evidence / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'evidence': str(evidence), 'exit_code': code,
                  'elapsed_seconds': result['elapsed_seconds'], 'failed_waits': failures,
                  'completed_checks': len(checks), 'inputs_unchanged': before == after}))
raise SystemExit(code)
