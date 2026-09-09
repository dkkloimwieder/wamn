# Final workspace sweep preparation

These scripts are prepared for the root agent to run after the serialized build and live slot is free.
No sweep has run from these files.
They adapt the capture recipe in `docs/perf/2026.09/generated-tui-integration/tools/workspace.py` and the result schema in cutover `validation-001/workspace-results.json`.
The old classifier source could not be found.

Run from any directory with Python 3.11 or later.
Use a fresh evidence directory in the main repository, outside the clean source worktree.
The source check requires the full fixed commit and a clean tree.
The example pins the currently active source; replace that commit only after the next reviewed source commit exists.

```bash
wamn_sweep_tree=/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909
wamn_sweep_evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/workspace-sweep-final-001
python3 -B /tmp/wamn-cutover-final-sweep-prepared/run.py \
  --source-tree "$wamn_sweep_tree" \
  --expected-source 0388e3bc98d231f1e69538e613ec33689c041653 \
  --evidence-dir "$wamn_sweep_evidence" \
  --host-nats-bin /tmp/wamn-cutover-nats-bin-20260909/nats-server
```

The wrapper returns Cargo's exit status, including 101 for failed tests.
Run this separate reduction command even when the sweep exits 101.
The reducer's exit 0 means it parsed the run; it does not mean the tests passed.
Exit 2 means the run or parsing has unresolved cases.

```bash
python3 -B /tmp/wamn-cutover-final-sweep-prepared/classify.py \
  --log "$wamn_sweep_evidence/workspace.log" \
  --baseline "$wamn_sweep_tree/docs/perf/2026.09/wasmcloud-2-9-cutover/validation-001/workspace-results.json" \
  --exit-code-file "$wamn_sweep_evidence/exit-code.txt" \
  --environment-names "$wamn_sweep_evidence/environment-names.json" \
  --output "$wamn_sweep_evidence/workspace-results.json"
```

The capture retains the exact requested workspace command, raw combined log, exit, UTC start/end, duration, source before/after, and machine load before/after.
It records environment names only.
It clears ambient `WAMN_*`, `WASH_*`, `OTEL_*`, `GIT_*`, `PG*`, `DB_URL`, `DATABASE_URL`, `KUBECONFIG`, and `CARGO_TARGET_DIR`.
It isolates Git configuration and Helm temporary directories, sets `KUBECONFIG=/dev/null`, and uses toolchain 1.98.0, four Cargo jobs, and an empty `RUSTC_WRAPPER`.
The baseline chart test can still attempt discovery at localhost:8080 and fail; no ambient cluster context is supplied.
The raw log remains unredacted; copied diagnostic excerpts redact URL userinfo and Bearer tokens.

The optional NATS argument arms only `WAMN_HOST_LIVE_NATS_SERVER_BIN` and `WAMN_HOST_LIVE_EVIDENCE_DIR`.
The service-owned test creates its own NATS process and temporary loopback ports; the wrapper does not stop or inspect other resources.
Root must confirm the serialized slot is free before running it.
The NATS binary path above is the one retained by `host-lifecycle-live-001/source.json`.
The live test adds about 70 seconds and writes under the fresh `host-lifecycle/` leaf.
Omit the argument only when that process proof cannot run; its missing-input failure then remains a separate new unarmed host fixture.
The host proof limits remain those in `docs/operations/build-and-test.md` under “Rebuilt host process lifecycle.”

The full sweep includes ignored tests and skips only the two schema regeneration functions.
The Receiving-owned `WAMN_STARTUP_BURST_INPUT` remains unarmed, so its missing-input failure is classified separately from the 67 validation-001 failures.
That classification does not replace the retained live startup proof.
Each prior failure matches package, Cargo target, test name, and its current diagnostic before retaining its old cause.
A changed cause is listed as `baseline_identity_cause_changed`; other new failures remain `new_potential_cutover_failure`.
An absent prior failure does not establish a fix.

The reducer counts the final libtest summary for each Cargo target and excludes nested subprocess summaries.
Explicit self-skips require a skip diagnostic and an `ok` completion for the named test.
Their count is a lower bound; silent returns and all arming paths were not surveyed.
No exact executed-proof count or cutover acceptance verdict is produced.

`offline-validation-final-002.json` records the preparation check against the retained validation-001 log.
It matches all counts, all 67 failure identities and classes, all 85 self-skip entries, and the one excluded nested summary.
Synthetic controls check a separate new startup fixture, a changed baseline cause, an inconsistent footer, compilation errors, and abnormal process exits.
Only Python syntax and offline log reduction ran.

```bash
python3 -B /tmp/wamn-cutover-final-sweep-prepared/validate_offline.py \
  --baseline-dir "$wamn_sweep_tree/docs/perf/2026.09/wasmcloud-2-9-cutover/validation-001" \
  --output /tmp/wamn-cutover-final-sweep-prepared/offline-validation-repeat.json
```
