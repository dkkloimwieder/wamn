# Generated TUI integration

This record covers local integration on `integrate/generated-tui-20260909`.
The generated-TUI parent is `2ad19106cd81790080a44c19a7001e0b59297ac3`.
The incoming `origin/main` parent is `c8c7362883fb5db19a7ef8cacf1663d3ca0e7b12`.
The first integration source commit is `a0f90cdacabff32900c239ec31376eed73bd4214`.
Main then advanced to `f6f113147362020134ee32d57199e7f24129e1f5` during the workspace sweep.
Commit `cc3e0d3c` includes that watcher update.
Commit `b57c8945` then repairs the materialization comparison fixture.
The [source record](source.json) identifies the final source and its changed-file hashes.
Earlier lane proofs do not establish results for this merged source.

The gate owner runs these checks in sequence.
Each recorded result must name its command, tested source, exit status, and retained log.
Tests that return early because their environment is absent count as unarmed skips, even when Rust reports them as passed.

| Gate | Current result | Evidence |
| --- | --- | --- |
| Merge compilation and native binaries | Exit 0 at `a0f90cda` | [Command](build-001/command.txt), [log](build-001/build.log), [status](build-001/exit-code.txt) |
| Deterministic generation against three fresh databases | Exit 0 at `a0f90cda`, zero changed files | [Result](runs/materialize-001/evidence/result.json), [commands](runs/materialize-001/evidence/commands.json) |
| Full workspace sweep at `a0f90cda`, including ignored tests | Exit 101, 2,074 reported passes, 68 failures, six doctest passes | [Comparison](runs/workspace-001/comparison.json), [log](runs/workspace-001/workspace.log) |
| Combined watcher at `cc3e0d3c` | 113 reported passes, two unarmed database failures | [Log](runs/watcher-001/watcher.log), [status](runs/watcher-001/exit-code.txt) |
| Materialization fixture repair at `b57c8945` | One pass | [Log](runs/final-001/materialize-fixture.log) |
| Contract runner and final CLI build at `b57c8945` | Exit 0, 33 contract assertions | [Commands](runs/final-001/commands.json), [results](runs/final-001/results.json), [contract log](runs/final-001/contract-diff.log), [build log](runs/final-001/build-wamn.log) |
| Disposable generated-operator launch and restart at `b57c8945` | Exit 0, source restored, owned cleanup complete | [Result](runs/live-001/evidence/result.json), [operator result](runs/live-001/evidence/operator-result.json), [hashes](runs/live-001/evidence/evidence.sha256) |

Build the native artifacts from the merged source before the live run.
Use this worktree's normal `target` directory.
Run this command from the repository root during a reserved machine gap.

```bash
RUSTUP_TOOLCHAIN=1.98.0 RUSTC_WRAPPER= \
  cargo build -p wamn-ctl -p wamn-host -p wamn-scenario-worker \
  -p wamn-generated-receiving-tui --bins --locked --offline
```

The live runner requires Linux, Docker Compose, Cargo, `wash`, and the `wasm32-wasip2` target for Rust 1.98.0.
It builds `http-route` in `components/target` before provisioning.
The existing helper runs the real development stages, including native rebuilds.
Reserve one machine gap for the whole runner invocation.
Do not wrap its internal commands in another gap helper.

```bash
python3 docs/perf/2026.09/generated-tui-integration/live.py \
  --evidence-dir docs/perf/2026.09/generated-tui-integration/runs/live-001/evidence
```

Each invocation needs a new evidence directory.
The runner creates a unique Compose project and reserves free localhost ports.
It starts only `receiving-route-postgres`, `authenticated-registry`, `receiving-dev-nats`, and `receiving-dev-tempo`.
It uses fresh disposable services and performs no kind or cluster operations.

The runner keeps credentials, configuration files, and raw logs in private scratch outside the source tree.
The scratch directory uses mode 0700.
Only redacted stage logs, helper results, terminal output, source hashes, and cleanup results enter the evidence directory.
Raw host diagnostics remain private; the helper retains their presence and content assertions as booleans.

Each run records the actual `HEAD`, any `MERGE_HEAD`, and source hashes before and after execution.
`result.json` requires a passing helper, complete redaction, unchanged source inputs, and successful owned cleanup.
Cleanup records process exits, container and volume removal, and closed listeners.
`operator-result.json` retains the helper's terminal restoration and target replacement assertions.
`evidence.sha256` covers the exported files.

The live helper covers a real request, native-source restart, fresh target state, clean terminal frames, retained diagnostics, and operator exit.
Receiving composition parity, WMS partial-completion proof, and deployed gates retain their separate acceptance requirements.
The materialization run used PostgreSQL 18.6 and three separate databases.
All nine write/check/check commands passed, and all 249 generated files retained their original bytes.
The runner removed its database container and volumes.

The workspace sweep repeated all 63 failures from the latest recorded sweep that included ignored tests.
Four new failures require absent live inputs.
The fifth new failure compared different package directory names, which correctly produce different generated TUI paths.
The fixture repair gives both packages the same basename and retains the full path and byte comparison.
Its focused test passed after the repair.

The sweep reported 65 missing-input failures, one unavailable Kubernetes endpoint, the known `wamn-362o.58` assertion, and the fixture failure.
At least 85 reported passes explicitly skipped their live work.
The comparison record names each failure, skip message, baseline source, and log line.
The sweep reported no compiler errors and excluded only the two documented schema regeneration commands.

The watcher run tests the reflog merge after the workspace sweep.
Its linked-worktree commit test and disabled-reflog refusal test passed.
Its two failures require `WAMN_DEV_VERIFICATION_PG_URL` and also occur in the workspace baseline.
The production delta after the sweep only changes the watcher.
The generator delta only changes its comparison fixture.

The final live run served a real location query through the generated Receiving operator.
A native source edit replaced environment instance `20155` with `22015`.
The old operator, host, and listener stopped before the replacement operator started.
The replacement showed an empty result state and kept its terminal frames free of host logs.
The operator exited with status zero, restored the terminal, and retained its private host diagnostics.

The runner restored the source bytes and removed every owned process, container, and volume.
All reserved listeners closed.
The exported evidence passed credential redaction and its SHA-256 manifest.
No in-cluster gate ran.
Receiving parity remains behind `wamn-ctc8.15.5`, and WMS response proof remains behind `wamn-b2m6.10`.
The complete recipe remains owned by `wamn-10yt.62.8`.
