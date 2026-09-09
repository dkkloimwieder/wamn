This offline adapter reuses `ctc8-12-fresh-auth/summarize.jq`, `trace-summary.jq`, and `reduce_load.py` unchanged. It reads completed journey outputs and writes no measurement or acceptance verdict. The jq inputs use temporary directory aliases; output trace paths and input hashes name the original files.

After both cutover measurements finish, run this from the fixed source worktree, with `wamn_candidate` set to the full measured 2.9 commit:

```bash
wamn_cutover=/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover
python3 -B /tmp/wamn-cutover-performance-reducer-prepared/compare.py \
  --reducers "$PWD/docs/perf/2026.09/ctc8-12-fresh-auth" \
  --before-journey "$wamn_cutover/performance-baseline-001/journey" \
  --after-journey "$wamn_cutover/performance-2-9-001/journey" \
  --after-source "$wamn_candidate" \
  --output "$wamn_cutover/performance-comparison-001"
```

The baseline defaults to `dfa1c3187fe8cd671688a442b23106046e502cb6`; `--before-source` exists for offline checks against the older retained data. Both run directories must hold an exit 0 receipt (`exit-code.txt` or `exit`), and each journey must have a passing cleanup receipt, six complete sweeps, eleven traces, and all load/memory/CPU sidecars. The output directory must be new and outside the build worktree.

`comparison.json` retains all 216 per-step values and raw CPU counter deltas, the existing 72-group throughput/percentile/CPU reduction, 36 original knee/peak verdicts, eleven trace samples per source, separate cutoffs/errors, and candidate-minus-baseline median changes. `load-memory.json` is the existing 432-sample/144-group reducer output; `consumed-inputs.json` hashes every consumed reducer, summary, trace, receipt, and resource sidecar. Raw generator outputs remain referenced by their real paths; this adapter does not rerun or replace the native per-sweep report reducer.

The protocol remains three repetitions per credential at concurrency 1/4/8/16/32/64 for ten seconds per layer. HTTP p50/p99 come from oha; pgbench percentiles use nearest rank over its 5% sampled transaction log. A native knee is the previous step at the first adjacent throughput gain below 1.2; no knee is calculated from median curves. Cutoffs remain unfinished requests, separate from `total_requests` and errors. Five service-warm ratios retain the recorded gate; human and single restart ratios remain descriptive. CPU includes the whole sample window, cgroup `memory.current` is a sample rather than RSS or peak, and machine load includes the benchmark itself.

`validation.json` records exact equality with the published retained `ctc8-12` reductions, preservation of native step/knee values, and refusal of an incorrect source. `existing-reducer-tests.log` records the existing offline reducer tests. These checks do not claim results for either pending cutover performance run. Do not run the old `measure.sh`: its broad proof glob includes the new live telemetry helper.
