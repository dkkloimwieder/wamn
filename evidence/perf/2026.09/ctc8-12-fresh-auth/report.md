# Fresh-auth step 1 measurement

`wamn-ctc8.12` removes one database statement from successful fresh authentication, from three reads to two. Both comparison runs are complete, but the measurements do not show a uniform speedup. All focused tests and the deployed `MEMBERSHIP-HTTP` gate pass.

For future JWT §8 comparisons, retain `after-001` at `a9e23b54` as the step-1 fresh-auth baseline. This baseline records two successful-auth reads. `before-002` is the pre-change comparison arm, not the retained step-1 baseline. The owner's stop rule also allowed three reads, so two reads were a target, not a session prerequisite.

## Source boundary and method

The pre-change run, `before-002`, uses `b5ae3eba8bd3da805df15b2b8c46becc2860de7e`. It includes the Receiving component pin refresh but leaves authentication unchanged. The candidate, `after-001`, uses `a9e23b54d5ac88e01052c076ea8019ebd8888af5`.

`before-001` failed before timed traffic because Acme pinned Receiving `43fcb1...`, but the built artifact was `8057a0...`. Exact admission remained intact. The `wamn-10yt.35` pin refresh at `b5ae3eba`, normal regeneration, and four focused tests resolved the mismatch. The [failed log](before-001.log), [exit 101](before-001/exit), and [cleanup receipt](before-001/journey/cleanup.receipt) remain as evidence.

The candidate combines the two system identity reads into one prepared SELECT and keeps the tenant permission query unchanged. The only production files changed between these commits are `crates/identity/platform/src/lib.rs` and `crates/platform/runtime/src/plugins/flow_http_routing.rs`. The remaining changes are tests, documentation, and offline proof helpers. Dependencies, schemas, grants, guests, and the measurement runner do not change between these commits.

Fixture and runner changes at `754a8559`, plus the Bash failure-propagation fix at `ac864880`, precede both measured source commits. The two-production-file statement covers only `b5ae3eba` to `a9e23b54`, not the whole implementation wave. Main commit `42dc0844` separately preserves a partial `before-001` snapshot and remains unchanged.

The [measurement wrapper](measure.sh) runs the existing journey with `--apply --fresh-auth-bench` at an explicit source commit. Both runs use release hosts in disposable clusters, with disposable PostgreSQL 18 and NATS. The [CPU record](machine-cpu.json) identifies an Intel i7-1185G7 with eight logical CPUs. The [kernel record](machine-kernel.txt) records Linux 7.0.0-31-generic.

Each credential has three sweeps, with concurrency 1, 4, 8, 16, 32, and 64. Each step runs for ten seconds across three layers. `route` calls `POST /purchase_order/get` with a service or human PAT, a personal access token. `nodb` calls an unauthenticated route that returns 404 without a database read. `pg` runs the generated read directly through prepared `pgbench` statements. The [sweep index](before-002/journey/throughput/service-1/index.json) records these targets and the sweep configuration.

## Completed comparison

`before-002` finished with [exit 0](before-002/exit) and [cleanup pass](before-002/journey/cleanup.receipt). `after-001` also finished with [exit 0](after-001/exit) and [cleanup pass](after-001/journey/cleanup.receipt). Each run completed six sweeps and 108 steps with zero recorded errors. Before and after recorded 1,499 and 1,496 deadline cutoffs, respectively. A cutoff is an unfinished request at the fixed deadline, not a successful response.

The route table shows before → after medians across three repetitions, with [minimum, maximum] for throughput. Rps means requests per second. P50 is the median request latency. P99 is the 99th-percentile request latency. Latency uses milliseconds, and host CPU/request uses CPU milliseconds per request. The [72-group matrix](comparison.json), produced by [summarize.jq](summarize.jq), retains every metric, range, concurrency, and control layer.

| Credential | Concurrency | Rps | p50 (ms) | p99 (ms) | Host CPU/request (ms) |
|---|---:|---:|---:|---:|---:|
| Service | 1 | 289.15 [193.27, 315.34] → 328.32 [315.03, 358.45] | 3.22 → 2.82 | 7.05 → 6.97 | 5.695 → 5.018 |
| Service | 8 | 1083.44 [924.92, 1160.89] → 1342.76 [1196.69, 1380.72] | 6.71 → 5.58 | 17.10 → 12.25 | 2.974 → 2.900 |
| Service | 16 | 1409.44 [1053.72, 1410.63] → 1374.80 [1192.31, 1417.40] | 10.72 → 10.93 | 26.28 → 26.57 | 2.762 → 2.889 |
| Human | 1 | 322.96 [288.56, 344.64] → 317.07 [281.66, 347.24] | 2.94 → 2.97 | 5.68 → 5.87 | 5.147 → 5.325 |
| Human | 8 | 1268.45 [1257.84, 1342.42] → 1218.38 [1182.15, 1382.06] | 5.89 → 6.16 | 12.75 → 13.27 | 2.832 → 2.995 |
| Human | 16 | 1207.24 [1170.34, 1344.32] → 1307.11 [1258.65, 1476.47] | 12.21 → 11.67 | 28.44 → 24.04 | 2.895 → 2.970 |

At concurrency 1/4/8/16/32/64, service median RPS changes are +13.55%, +5.05%, +23.93%, -2.46%, +13.64%, and -0.61%. Human changes are -1.82%, +5.54%, -3.95%, +8.27%, +1.69%, and +5.20%. Human host CPU/request medians rise by 2.10% to 5.77% at all six levels. Service CPU results are mixed.

The spread remains large, including the pre-change service c1 range of 193.27 to 315.34 rps and p99 range of 6.84 to 15.48 ms. All completed repetitions remain in the comparison, including slower results. These descriptive changes do not establish statistical significance or a uniform speedup.

## Semantic evidence and measurement limits

At the candidate commit, [quality-001](quality-001.log) and [quality-002](quality-002.log) each passed 46 focused tests. Both captures returned exit 0. Three deliberate code alterations compiled, then failed the route authentication proof at the intended assertion. They cover [environment membership](mutation-001/cargo.log), [required service role](mutation-002/cargo.log), and [full-token verification](mutation-003/cargo.log). Each mutation capture returned exit 0 because the proof detected the alteration, not because the altered code passed.

The deployed [membership receipt](membership-001/journey/membershipproof.receipt) records seven passing HTTP cases at the [same candidate commit](membership-001/journey/membershipproof-journey.receipt). Absent membership returns 401. Grant and repeated grant return 200. Removing the environment role returns 403, and restoring it returns 200. Revoke and repeated revoke return 401. The capture returned [exit 0](membership-001/exit), with [cleanup pass](membership-001/journey/cleanup.receipt).

The final deployed gate finished at 12:10:32 UTC on September 8, 2026. All measurements and tests use disposable resources. These local worktree results do not claim a merge to main or a full workspace test pass.

The three sweeps share one host process within each phase. They are repeated observations, not three independent deployments, and requests are not independent trials. Host CPU counters cover the whole sample window, including background activity. Sample windows range from 13.150 to 21.889 seconds before, and 13.218 to 23.489 seconds after, beyond the ten-second load duration. CPU/request is therefore not the isolated CPU cost of authentication.

At c8, `nodb` median RPS falls 13.71% in the service-labeled sweeps and 7.12% in the human-labeled sweeps. Direct `pg` median RPS falls 9.14% and rises 12.40%, respectively. These are schedule labels, not authenticated control requests. The controls use different request paths or bypass the host entirely. They do not isolate the identity query cost, so subtraction from a control does not establish the effect of the combined SELECT.

## Load and memory

The [load and memory result](load-memory.json) contains 432 raw samples and 144 matched groups. The table gives observed minimum to maximum across 216 samples per phase, covering all credentials, layers, concurrency levels, and edges. The [offline reducer](reduce_load.py) retains these coordinates and source-file paths without filling missing values.

| Metric and unit | Before range | After range |
|---|---:|---:|
| 1-minute load (tasks) | 1.81 to 31.12 | 4.60 to 23.65 |
| 5-minute load (tasks) | 3.39 to 16.61 | 6.80 to 13.81 |
| 15-minute load (tasks) | 2.15 to 12.15 | 7.65 to 11.34 |
| MemAvailable (KiB) | 43,987,972 to 49,377,664 | 43,979,000 to 45,861,724 |
| memory.current (bytes) | 50,311,168 to 71,835,648 | 48,246,784 to 70,942,720 |

Machine load includes this benchmark itself and other activity, so it is not an independent measure of background load. Load averages use tasks, not percent. `MemAvailable` uses Linux kB, meaning KiB or 1,024 bytes. `memory.current` records sampled host cgroup memory, not RSS or peak memory.

## Completed startup traces

All 22 startup traces are complete in [trace-summary.json](trace-summary.json), produced by [trace-summary.jq](trace-summary.jq). Each pre-change trace records two identity reads and one permission read. Each candidate trace records one identity read and one permission read. These are application trace counts, supported by the changed query call site, not database statement counters. Together, the call site and traces establish one fewer statement for successful authentication.

Warm authentication times use five samples per credential per phase. Values below are medians [minimum, maximum] in milliseconds. The service median fell, but the human median rose, and both ranges overlap. These small samples do not establish a latency gain.

| Credential | Before (ms) | After (ms) |
|---|---:|---:|
| Service | 1.75104 [0.761344, 1.871104] | 0.975872 [0.814592, 2.032896] |
| Human | 1.275392 [1.15712, 4.11136] | 1.795072 [0.937472, 2.32576] |

Restart results contain one sample per phase and remain separate in the linked JSON. The four component hash lines compare byte-identically between the [pre-change run](before-002/journey/component-bytes.sha256) and [candidate](after-001/journey/component-bytes.sha256). Both timed runs and their startup traces are complete.
