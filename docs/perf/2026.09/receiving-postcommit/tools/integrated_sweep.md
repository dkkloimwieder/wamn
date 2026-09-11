# Final workspace sweep

Run this sweep once after the implementation reaches main.
Use a dedicated verification worktree at that exact integrated commit.
Keep the main checkout for integration, fetch, and push.
The wrapper refuses a dirty checkout or a commit that differs from local main.
The wrapper also requires the post-commit proof source file.

Use the worktree's own default `target/` directory.
The wrapper creates a unique temporary scratch directory directly under `/home/kaalin/.cache/wamn-lanes`, outside the source worktree.
It uses that scratch directory for `TMPDIR` and the Helm directories.
It removes only that directory on normal or error completion and records `scratch-cleanup.json`.
The wrapper clears inherited database, NATS, Kubernetes, and WAMN authority.
It supplies no live fixture URL.
The armed observer remains separate evidence under `../observer-pg18-002/`.
That receipt uses the existing `pg_virtualenv -t -v 18` helper, applies the platform bootstrap, and records cleanup.
Its URL never arms other database suites.

After the verification worktree exists, run the following command.
Replace the commit operand with the full integrated main commit.
Omit `--apply` to inspect the command without a build.

```bash
python3 /home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/tools/integrated_sweep.py \
  --source-tree /home/kaalin/.cache/wamn-lanes/receiving-postcommit-final-20260910 \
  --expected-main FULL_INTEGRATED_MAIN_COMMIT \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/integrated-workspace-001 \
  --apply
```

The wrapper uses this retained P3 command without a pipe.
It retains the Cargo exit code and source state before and after the run.
It uses Rust 1.98.0, two build jobs, and an empty `RUSTC_WRAPPER`.
The two exclusions prevent regeneration tests from writing source files.

```bash
cargo test --workspace --no-fail-fast --locked --offline -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema
```

After `exit-code.txt` exists, run the comparison.
The comparison also accepts a failed Cargo run.

```bash
python3 /home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/tools/compare_integrated_sweep.py \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/receiving-postcommit/integrated-workspace-001
```

The P3 baseline is `481d281ba8138c639345daa56b0bd2da0ff1952d` in `p3-http-cutover/integrated-workspace-001`.
It reports 2,195 test passes, six doctest passes, 75 failures, and 85 explicit self-skips.
The later PAT baseline is `437672fcae8f76ad1df6cde705524a267dac1e04` in `ctc8-20-pat-service/main-landing-001`.
It reports 2,209 test passes, six doctest passes, 78 failures, and 85 explicit self-skips.
All 75 P3 failure identities and causes remain in the PAT baseline.
The latest HTTP reuse baseline is `dce31af910ba7b586e3537ab2b1cfea784ea2066` in `ctc8-16-http-reuse/integrated-workspace-001`.
It reports 2,231 test passes, six doctest passes, 81 failures, and 85 explicit self-skips.
Its source remained clean at the same commit before and after the completed sweep.
All 78 PAT failure identities remain, alongside three failures for absent live fixture inputs.
Those additions are the additive UPDATE regression and two HTTP reuse tests.
The PAT comparison retains one changed cause because the WIT diagnostic contains a different checkout path.
The comparator retains all three comparisons and evaluates its exit status against the latest HTTP reuse reference.

A reported pass that skips its live work does not prove that work ran.
The parser excludes nested test summaries from totals.
It preserves exact package, target, and test names, plus failure causes.
It normalizes only the retained timestamp, process ID, and panic location rules.
An absent failure does not prove a repair.
Read every added failure, changed cause, missing target, and unresolved classification before integration acceptance.

`integrated_sweep_preparation.json` records an offline replay of the retained P3 log.
The command, all count fields, 75 failure identities and causes, and 85 explicit self-skips agree.
`../integrated-http-reuse-preparation-001/result.json` records an offline comparison of the retained HTTP reuse result.
All 81 failure identities and causes and all 85 explicit self-skips match that latest reference.
No build or live job ran during either preparation.
