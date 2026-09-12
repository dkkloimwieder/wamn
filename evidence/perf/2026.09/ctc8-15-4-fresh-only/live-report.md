# Fresh-only live proof

The [original boundary report](report.md) retains the earlier local tests and deliberate-fault evidence.

## Live proof, 2026-09-10

The full gate passed at clean source `e13fd2f391a6cf0c4db797803a1f6c4c2cd846ea`.
It uses direct upstream wasmCloud 2.9 and includes the effects boundary from `c8648804`.
The command uses the standard host, gates, and identity images:

```sh
tools/receiving-cluster-journey-run --fresh-only-proof --apply --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-4-fresh-only/deployed-006
```

The [exit record](local-002/deployed-006-exit.txt) is 0.
The [final receipt](deployed-006/fresh-only-proof-journey.receipt) records every required arm as passing.
Each of the three selected Rust tests ran exactly once, with no failures or ignored tests.
The [structured result](local-002/deployed-006-result.json) records the counts and durations.

The [nested-call log](deployed-006/session-nested.log) pairs direct and nested session refusals with successful calls using the same human's PAT.
The refusals retain the exact HTTP 403 fresh-credential-required response.
Removing the project role or project-environment membership refuses the next PAT request.
The fixture leaves the existing business record unchanged.

The separate counter fixture commits its first effect before the nested fresh-only refusal.
Its [receipt](deployed-006/fresh-only-prior-commit.receipt) records counter 1 after the session call and counter 2 after the explicit PAT call.
The fixture does not retry with a PAT automatically.
A second tenant's row stays at zero.
The raw parent declares no partial-completion result contract, so the effects boundary does not replace its exact refusal.

Both two-host key-removal runs pass, with the public-key service [reachable](deployed-006/host-session-reachable.receipt) and [unreachable](deployed-006/host-session-unreachable.receipt).
Separate [reachable](deployed-006/host-session-reachable-process-continuity.json) and [unreachable](deployed-006/host-session-unreachable-process-continuity.json) records retain each host process through its key-removal window.
The [cleanup receipt](deployed-006/cleanup.receipt) passes.
Both owned runner processes ended, and Docker listed only the three frozen wamn nodes afterward.

## Earlier attempts

All five earlier attempts remain failures. Their logs and cleanup records stay in this evidence set.

| Attempt | Source | Result and correction |
| --- | --- | --- |
| [001](local-002/deployed-001-launch.log) | `aed88333` | Docker planner omitted the generated packages. Fix `wamn-kxif` is `2547f28f`. |
| [002](local-002/deployed-002-launch.log) | `2547f28f` | Copied directory names changed the generated TUI path. Fix `d8a28593` retains the source names. |
| [003](local-002/deployed-003-result.json) | `ee9d8fcf` | The fixture parsed the publisher subject as a principal ID. Fix `1673ee4f` uses the existing subject lookup. |
| [004](local-002/deployed-004-result.json) | `1673ee4f` | The first counter request returned HTTP 503 before the nested call. The local Wasmtime test did not reproduce it. |
| [005](local-002/deployed-005-result.json) | `21775e60` | Failure diagnostics showed counter zero and PostgreSQL execution. The isolated database test below identified the fixture's tenant-setting mismatch. |

The counter fixture incorrectly used `app.tenant`, but production derives tenant authority from the database login.
The [isolated PostgreSQL 18 test](local-002/counter-login-pg-001.log) reproduces the old statement's SQLSTATE 42704.
The corrected policy permits two own-row updates and no foreign-row updates, even with a forged tenant setting.
It uses an actual non-superuser login without permission to bypass row policies.
The test passed once, and root removed its exact disposable database container.

The [focused tests](local-002/fresh-only-tests-008.log) passed four tests and ignored three live tests.
The separate armed PostgreSQL run and the full deployed gate cover those ignored selections.
The [local results](local-002/results.json) distinguish compilation, focused tests, and live coverage.
The existing three deliberate-fault results above remain the mutation evidence.
No additional host-guard bypass test ran.

The [new evidence hashes](live-SHA256SUMS) cover all 341 files from the six deployed attempts and the second local increment.
They passed before and after the move into this worktree.
The earlier `SHA256SUMS` remains unchanged.
The retained evidence contains no matches for credential-bearing database URLs, bearer tokens, compact JWTs, or private-key headers in the recorded pattern scan.
That scan is not a guarantee that all possible secret forms are absent.

This gate makes no latency claim and activates no production session routes.
The release manifest remains format 1.
The combined workspace sweep and main integration are not part of this evidence.
