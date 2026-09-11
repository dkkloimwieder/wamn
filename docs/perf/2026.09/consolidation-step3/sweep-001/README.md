# Stage 3 retained workspace capture

The capture and classification tools are ready. The stage 3 workspace test has not run. The [source review](source-review.json) lists the 19 added ignored cases and their setup boundaries.

After the integrated source is fixed and the root Cargo target is idle, run this from the main repository:

```bash
env -u DB_URL python3 -B \
  docs/perf/2026.09/consolidation-step3/sweep-001/tools/capture.py \
  "$(git rev-parse HEAD)" run-001
```

The capture requires the supplied commit to match HEAD and a new `run-001` directory. It calls the unchanged [retained runner](../../generated-tui-integration/tools/workspace.py). The runner writes the complete, unpiped Cargo output and actual exit code. The capture records source bytes, modes, HEAD, and repository status before and after the command.

The Cargo command remains:

```bash
cargo test --workspace --locked --offline --no-fail-fast -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema
```

The runner sets `RUSTUP_TOOLCHAIN=1.98.0`, `RUSTC_WRAPPER=`, and `CARGO_BUILD_JOBS=2`. It uses the root default target. It removes ambient `WAMN_*`, `OTEL_*`, `GIT_*`, `PG*`, `DATABASE_URL`, and `CARGO_TARGET_DIR`. The outer command also removes `DB_URL`.

The runner sets `KUBECONFIG=/dev/null`, `GIT_CONFIG_GLOBAL=/dev/null`, and `GIT_CONFIG_NOSYSTEM=1`. It owns a temporary directory for `TMPDIR` and the three Helm directories. It removes that directory when Cargo exits. Environment records list names without private values. No live input is armed.

After capture finishes, including an exit of 101, run the two offline steps:

```bash
python3 -B docs/perf/2026.09/consolidation-step3/sweep-001/tools/reduce-current.py \
  --evidence-dir docs/perf/2026.09/consolidation-step3/sweep-001/run-001 \
  --output-dir docs/perf/2026.09/consolidation-step3/sweep-001/run-001/reduction-001
python3 -B docs/perf/2026.09/consolidation-step3/sweep-001/tools/finalize-classification.py \
  --evidence-dir docs/perf/2026.09/consolidation-step3/sweep-001/run-001 \
  --reduction-dir docs/perf/2026.09/consolidation-step3/sweep-001/run-001/reduction-001 \
  --output-dir docs/perf/2026.09/consolidation-step3/sweep-001/run-001/classification-001
```

The reduction preserves the retained parser's complete output. It compares every failure identity and normalized cause with the baseline, step 1, and final step 2 run. It also records changed test names and compares skips with their target executables. Changed ownership or missing cases need source reconciliation. Neither a removed failure nor a renamed case becomes a pass.

The final report lists every failed case, classification, actual cause, and raw log line. JSON retains the complete diagnostic excerpts and every explicit skip. Both same-named native B occurrences remain available when the child diagnostic hides the parent's missing input. Unknown causes and unresolved parser entries stay in `pending-review.json`. No fixed failure count or completion verdict is imposed.

Receiving's 12 new cluster cases require their explicit result-directory input before live setup. The five WMS cases check source cleanliness first, so untracked sweep output can cause setup refusal. Both native C cases require an explicit broker URL or native binary. The classifier applies these explanations only when the actual diagnostic matches and the captured source supports it. These refusals remain failed tests and establish no application outcome.

The [offline replay](replay-step2-001/comparison.json) reproduces all 82 step 2 failure identities and classes and all 84 named explicit skips. It starts no Cargo process or service. The first finalizer attempt failed to find a literal that the Rust source formats from `URL_ENV`. Its [record](replay-step2-001/finalize-attempt-001.json) and source remain. The corrected [second attempt](replay-step2-001/finalize-attempt-002.json) passed. These are replay results, not stage 3 execution.

[Tool inputs](tool-inputs.json) identify the retained scripts and their adapted hashes. All imported comparison inputs live in the repository. Earlier runs remain unchanged.
