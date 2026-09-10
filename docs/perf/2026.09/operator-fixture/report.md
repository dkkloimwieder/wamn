# Operator fixture race

Issue: `wamn-10yt.62.9`.
Base: `b214506aa6cda434202e22d22941ff19390c37c8`.

The operator test fixture wrote executable scripts in the multithreaded test process.
In the reproduction, another thread created a child while the script remained open for writing.
The child inherited that descriptor until its own program started.
Linux then refused a concurrent script launch with `ETXTBSY`, even after the parent closed its descriptor.
`CLOEXEC` closes a descriptor when the child starts its program, not when the child first appears.
The upstream Rust report describes the same sequence: [rust-lang/rust#114554](https://github.com/rust-lang/rust/issues/114554).

The fixture now asks `/bin/sh` to write and mark each script executable.
The parent waits for that writer to exit before it launches the script.
The test process never opens the script for writing.
The fixture keeps its existing script contents, mode `0700`, arguments, and bound environment facts.
Only the test helper changes.
Production operator behavior stays unchanged.

The reproduction tool extracts the actual `Fixture::script` implementation from `operator.rs`.
Four threads repeatedly launch `/bin/true` while the main thread writes and launches scripts.
The tool stops at the first failure or after 2,000 successful launches.
Every run removes its temporary directory and joins its competitor threads.
The tool records the source hash, extracted fixture, command, result, and elapsed time.

The unchanged fixture failed with error 26 on attempt 1 in `baseline-001`.
The traced run failed on attempt 2 in `baseline-trace-001`.
Its `syscalls.log`, lines 132 through 158, captures the open writer, concurrent child creation, parent close, and refused script execution.
The competing child finished its own `execve` after the script received `ETXTBSY`.
This trace supports the reproduced cause.
The historical failing run did not retain a descriptor trace, so this report does not claim that evidence exists.

The fixed fixture completed 2,000 launches in 10.807 seconds in `fixed-001`.
The actual operator module passed all 12 tests in `focused-001`.
After removal of one unrelated formatting change, the final source passed all 12 tests again in `focused-002`.
The final focused command took 8.674 seconds, including compilation, and the test runner filtered 252 unrelated tests.
Neither focused run ignored tests.
The earlier `invocation-error-001.log` records an invalid capture-tool argument and counts as no test run.

Run the reproduction from an isolated worktree:

```bash
python3 docs/perf/2026.09/operator-fixture/tools/run_reproduction.py \
  --tree "$PWD" \
  --evidence-dir "$PWD/docs/perf/2026.09/operator-fixture/reproduction-next"
```

Run the operator tests:

```bash
cargo test --locked --offline -p wamn-ctl --lib dev::operator::tests -- --include-ignored
```

The scoped Clippy command passed in 189.575 seconds with the repository warning baseline still visible.
It denied `clippy::correctness` and `clippy::suspicious` across all `wamn-ctl` targets.
It did not deny every existing warning or run a separate baseline lint pass.
`git diff --check` and Python syntax parsing passed.

These local tests establish the fixture fix.
They do not constitute a full workspace sweep, deployment proof, or cluster gate.
No manifest, inventory, component artifact, or fenced production directory changed.
