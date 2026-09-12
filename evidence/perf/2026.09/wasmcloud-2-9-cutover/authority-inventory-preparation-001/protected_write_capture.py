"""Read one fresh-fixture ACL after the existing denial matrix, before cleanup."""
import hashlib
import json
from pathlib import Path


EXPECTED = {
    'schema': 'wamn-event-registration-write-capture/v1',
    'database': 'wamn',
    'transaction_read_only': True,
    'relation': 'catalog.event_registrations',
    'relation_count': 1,
    'owner': 'postgres',
    'rls_enabled': True,
    'rls_forced': True,
    'nonowner_table_writes': [],
    'nonowner_column_writes': [],
    'guest_select': True,
    'guest_table_update': False,
    'guest_any_column_update': False,
    'guest_tenant_id_update': False,
}


def capture_event_registration(runner, environment, result):
    sql = Path(__file__).with_name('capture-event-registration.sql')
    matrix_passed = result['verdict'] == 'pass'
    # Any capture exception must leave the existing wrapper case failed.
    result['verdict'] = 'fail'
    result['stage'] = 'protected-write-capture'
    capture = {
        'schema': 'wamn-protected-write-capture-receipt/v1',
        'source': result['source'],
        'fixture': result['fixture'],
        'database': 'wamn',
        'scope': 'production-project-database',
        'relation': 'catalog.event_registrations',
        'fresh_only': True,
        'matrix_passed': matrix_passed,
        'matrix_test_summaries': result['test_summaries'],
        'helper_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        'sql_sha256': hashlib.sha256(sql.read_bytes()).hexdigest(),
        'verdict': 'fail',
        'cleanup_receipt': 'authority-denial-matrix.json',
    }
    result['protected_write_capture'] = capture
    receipt, raw = runner.command(
        'authority-denial-matrix-protected-write-capture',
        ['psql', '--no-psqlrc', '--quiet', '--set', 'ON_ERROR_STOP=1',
         '--tuples-only', '--no-align', '--file', str(sql)],
        environment | {'PGDATABASE': 'wamn'}, timeout=20,
    )
    capture['command'] = receipt
    captured = None
    if receipt['exit_code'] == 0 and not receipt['timeout']:
        try:
            captured = json.loads(raw)
        except json.JSONDecodeError:
            pass
    # Public output contains only strict, independently expected facts. Unexpected
    # server text (including role names) stays in the runner's private raw log.
    capture['checks'] = {
        key: isinstance(captured, dict) and key in captured
        and type(captured[key]) is type(expected) and captured[key] == expected
        for key, expected in EXPECTED.items()
    }
    version = captured.get('server_version_num') if isinstance(captured, dict) else None
    capture['checks']['postgres_18'] = type(version) is int and 180000 <= version < 190000
    capture['checks']['exact_capture_fields'] = (
        isinstance(captured, dict)
        and set(captured) == set(EXPECTED) | {'server_version_num'}
    )
    if matrix_passed and all(capture['checks'].values()):
        capture['verdict'] = 'pass'
        capture['observed'] = captured
        result['verdict'] = 'pass'
    path = runner.evidence / 'event-registration-protected-write-capture.json'
    path.write_text(json.dumps(capture, indent=2) + '\n')
    path.chmod(0o644)
    return capture
