#!/usr/bin/env python3
"""Run the operator advisory proof against one disposable event broker."""
import json
import pathlib
import subprocess
import sys

worktree = pathlib.Path(sys.argv[1]).resolve()
out = pathlib.Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=False)
commands = []


def execute(argv, name):
    commands.append(argv)
    (out / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
    result = subprocess.run(argv, capture_output=True, text=True)
    (out / (name + ".stdout")).write_text(result.stdout)
    (out / (name + ".stderr")).write_text(result.stderr)
    (out / (name + ".exit-code")).write_text(str(result.returncode) + "\n")
    result.check_returncode()
    return result.stdout.strip()


execute(["docker", "image", "inspect", "nats:2.10-alpine"], "broker-image")
container = execute([
    "docker", "run", "--detach", "--name", "wamn-native-c-advisory-reader-001",
    "--label", "wamn.proof=native-c-advisory-reader-001",
    "--publish", "127.0.0.1::4222", "nats:2.10-alpine", "-js", "-sd", "/data",
], "broker-start")
exit_code = 1
try:
    execute(["docker", "inspect", container], "broker-inspect")
    execute(["docker", "exec", container, "nats-server", "--version"], "broker-version")
    port = execute(["docker", "port", container, "4222/tcp"], "broker-port")
    assert port.startswith("127.0.0.1:") and "\n" not in port
    command = [
        "env", "CARGO_BUILD_JOBS=4", "WAMN_NATIVE_C_NATS_URL=nats://" + port,
        "cargo", "test", "-p", "wamn-ctl", "--features", "ops", "--lib",
        "event_advisories::tests::retained_broker_advisories_report_missing_source_payloads",
        "--locked", "--offline", "--", "--ignored", "--exact", "--nocapture", "--test-threads=1",
    ]
    capture = pathlib.Path(__file__).with_name("capture.py")
    argv = [sys.executable, str(capture), str(worktree), str(out / "test"), *command]
    commands.append(argv)
    (out / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
    exit_code = subprocess.run(argv).returncode
finally:
    execute(["docker", "logs", container], "broker-logs")
    execute(["docker", "rm", "--force", container], "broker-remove")
    remaining = execute(["docker", "ps", "--all", "--filter", "id=" + container,
                         "--format", "{{.ID}}"], "broker-absence")
    assert not remaining, "the exact disposable broker remains"
    (out / "result.json").write_text(json.dumps({
        "test_exit_code": exit_code, "broker_id": container, "cleanup": "pass"
    }, indent=2) + "\n")
sys.exit(exit_code)
