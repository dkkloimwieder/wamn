This tool prepares two temporary defect controls and restores their exact source bytes and file metadata.
It does not build, run a proof, change Git history, or assign a control result.

Use an inactive worktree and its dedicated Cargo target directory.
Set `CONTROL_TREE`, `CONTROL_TARGET`, and `CONTROL_EVIDENCE` to those explicit paths.
The evidence path must be new and below the main checkout's `docs/perf/2026.09/receiving-postcommit/` directory.

```bash
MUTATION_TOOL=/home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/tools/mutation.py
python3 "$MUTATION_TOOL" prepare --control replay-reset \
  --inactive-worktree "$CONTROL_TREE" --evidence-dir "$CONTROL_EVIDENCE"
```

The replay control changes only the duplicate-only statement and its declared update fields.
The package already grants these fields through `quality.approve_inspection`, so its combined database privileges stay unchanged.
The timeout control changes only the private handler's timeout classification.
Use `--control timeout-terminal` to select it, then skip generation and sealing.

For the replay control, use the retained helper below.
Set `CONTROL_TARGET` to a dedicated directory inside `CONTROL_TREE`.
Set `CONTROL_GENERATION_EVIDENCE` to a new directory below `CONTROL_EVIDENCE`.
The helper applies the exact base and overlay migrations in a fresh PostgreSQL 18 cluster.
It follows the migration-only fixture in the [Receiving recipe](../../../../operations/build-and-test.md).
The authoring fixture carries no runtime ACL grants because the normal generator refuses them.

The helper runs normal `materialize_package` commands with `write` and then `check`.
After successful generation and cluster cleanup, it seals the five changed output hashes through `mutation.py`.
It retains the exact commands, migration hashes, logs, and cleanup result.
The timeout control skips this helper.

```bash
CONTROL_TOOLS=/home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/tools
python3 "$CONTROL_TOOLS/capture.py" \
  --tree "$CONTROL_TREE" --evidence-dir "$CONTROL_GENERATION_EVIDENCE" -- \
  python3 "$CONTROL_TOOLS/materialize_replay_pg18.py" \
  --inactive-worktree "$CONTROL_TREE" --cargo-target-dir "$CONTROL_TARGET" \
  --mutation-evidence-dir "$CONTROL_EVIDENCE" \
  --evidence-dir "$CONTROL_GENERATION_EVIDENCE"
cd -- "$CONTROL_TREE"
CARGO_TARGET_DIR="$CONTROL_TARGET" RUSTC_WRAPPER= tools/build-components m1
```

Commit the prepared mutant with normal Git hooks before the live journey.
Run one baseline installation per control through `tools/receiving-cluster-journey-run --apply --receiving-postcommit baseline` with its new evidence directory.
Require the intended assertion failure and successful owned-resource cleanup.
A build failure, admission refusal, or earlier journey failure does not kill a control.
The replay control must fail state preservation after a completed duplicate delivery.
The timeout control must observe one blocked attempt instead of three.

```bash
python3 "$MUTATION_TOOL" restore \
  --inactive-worktree "$CONTROL_TREE" --evidence-dir "$CONTROL_EVIDENCE"
```

Restoration refuses unexpected bytes or modes and keeps the retained backups.
It restores source and generated files, including their original nanosecond access and modification times.
Exact old timestamps can let Cargo reuse a mutant artifact.
Before the restored positive run, remove the owned mutant component outputs and rebuild through the normal path.

```bash
CARGO_TARGET_DIR="$CONTROL_TARGET" RUSTC_WRAPPER= cargo clean \
  --release --locked --offline --manifest-path components/Cargo.toml \
  --target wasm32-wasip2 -p client-acme-receiving \
  -p wamn-client-acme-receiving-data-access
rm -- "$CONTROL_TARGET/virtualized/std-empty-environment/client_acme_receiving.wasm"
CARGO_TARGET_DIR="$CONTROL_TARGET" RUSTC_WRAPPER= tools/build-components m1
```

The `m1` build uses the release profile, so cleanup must select that profile too.
Without `--release`, timestamp restoration can retain the previous control's generated data-access library.

Record the restored positive result separately from the deliberate failures.
The tool records Git HEAD for provenance and permits the caller's normal mutant and restoration commits.
