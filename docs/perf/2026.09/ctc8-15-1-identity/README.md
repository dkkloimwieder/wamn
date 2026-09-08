# Identity foundation

This evidence belongs to `wamn-ctc8.15.1`.
`SHA256SUMS` records every evidence file except itself.
Source commit `ff8cb4b2` adds the separate identity service, system-held signing keys, scoped credentials, and public-key cache.
Commit `9ab1c622` integrates main `0f1021d6` into that source.
Commit `8cbf57c8` corrects the deployed observer timestamp.
The deployed proof passes at that commit.
Integrated main `9f16600d` differs only in tracker data.
The integrated workspace and contract runs are complete.
The workspace exits 101 for the environment and baseline failures listed below.
The contract runner exits 0.

Session admission stays disabled.
This issue does not prove `/session`, host token admission, fresh-only operations, or TUI login.
The required two-host token proof belongs to `wamn-ctc8.15.3`.
No performance result is claimed here.
Later measurements must use the step-1 `after-001` baseline at `a9e23b54`.

## Local proofs

Each live suite uses a separate PostgreSQL 18.6 container.
Its `server`, `test.command`, `test.log`, `exit`, and `cleanup.log` files record the server, command, result, and cleanup.
The fixtures explicitly arm the destructive schema tests against those disposable servers.

| Proof | Evidence | Result |
| --- | --- | --- |
| Fixed token profile | `targeted-001/tokens.log` | 7 passed |
| Issuer credential parser | `targeted-001/issuer.log` | 3 passed |
| Compiled provisioning CLI and redacted help | `targeted-002/cli.log`, `cli-help.log` | 3 and 1 passed |
| Signing lifecycle and failed COMMIT | `live-keys-003` | 2 passed, exit 0 |
| Actual issuer grants | `live-issuer-002` | 1 passed, exit 0 |
| Compiled HTTPS service | `live-surface-001` | 2 passed, exit 0 |
| Credential preparation and retirement | `live-cli-001` | 2 passed, exit 0 |
| Generated protected-write inventory | `live-protected-update-002` | 1 passed, exit 0 |
| Independent protected-write comparison | `live-protected-check-001` | 1 passed, exit 0 |
| Schema and server invariants | `live-control-storage-002` | 11 passed, exit 0 |
| Restored HTTPS cache | `cache-restored-001` | 13 passed, exit 0 |
| Targeted Clippy | `final-targeted-002/lints.log` | Exit 0, inherited warnings |
| Architecture inventories | `final-targeted-002/inventories.log` | Exit 0 |
| Rendered deployment inventory | `final-targeted-002/platform.log` | 7 passed, 1 inherited failure |

The deployment proofs inspect the rendered chart and actual host and authoring Gate credential references.
Only the identity service receives the issuer database credential and identity serving TLS key.
The live grant proof denies the identity-reader role access to both signing tables.
The HTTPS service refuses `/session` and `/authoring`.

## Deliberate defects

Each modified program compiled before its test failed.
Each receipt records original, modified, and restored source hashes.
Each database mutation used a fresh server.
The restored key, credential, and cache suites then rebuilt and passed.

| Removed safeguard | Evidence | Failure | Exit |
| --- | --- | --- | --- |
| Signing COMMIT | `live-keys-mutant-no-commit` | Signer returned before the COMMIT barrier | 101 |
| Signing shared lock | `live-keys-mutant-no-share` | Rotation passed the uncommitted signer | 101 |
| Issuer DELETE grant | `live-issuer-mutant-no-delete` | Actual server grants lacked DELETE | 101 |
| Exact cache expiry | `mutant-cache-expiry-001` | A known key outlived its evidence | 101 |

The missing DELETE control fails at the actual grant comparison, before later write assertions.
The tests compiled and ran in one Cargo process.
The capture does not claim a separate compiler exit code.

## Deployed proof

`deployed-002` exits 0 with `completed=true` and `cleanup_failed=false`.
The canonical identity and gate images run in a new, isolated kind cluster.
The journey provisions the issuer credential through the actual CLI and installs the actual identity chart.
Each Job receives public inputs only.

| Job | Public key set | Result |
| --- | --- | --- |
| `identity-jwks-published-a` | A | Passed, container exit 0 |
| `identity-jwks-published-b` | A and B | Passed, container exit 0 |
| `identity-jwks-activated-b` | A and B | Passed, container exit 0 |
| `identity-jwks-removed-a` | B | Passed, container exit 0 |
| `identity-jwks-removed-b` | Empty | Passed, container exit 0 |

Each Job records its actual Pod state, image identity, and public receipts.
The checks cover HTTPS, exact public output, issuer attribution, unknown-key refusal, an unrelated CA, plaintext refusal, and absent authority routes.
Nonempty sets also exercise two independent caches and unchanged deadlines on known-key hits.
The observer uses the time after return for its external deadline bound.
The deterministic cache tests prove the exact internal request-start bound and HTTP Age accounting.

These Jobs do not prove session-token admission or deployed expiry during an outage.
Each final receipt explicitly says `token_admission=not_proven expiry_outage=not_proven`.
The journey removes its own cluster, image tags, and temporary credentials.

## Integrated tests

`integration-001` runs once on main `9f16600d` with ignored tests included.
The source commit remains unchanged throughout the run.
The capture unsets live connection variables to protect shared infrastructure.
Separate disposable servers provide the new identity proofs listed above.

The workspace reports 1,876 passed and 59 failed tests across 165 top-level targets.
Of the 28 failed targets, 27 lack required environment inputs or fixtures.
Those targets account for 58 failed tests, including five tests from the four new identity live targets.
Their separate armed runs pass.
The remaining failure is the WMS Secret prerequisite defect reproduced on the clean baseline.
The run identifies no additional code failure.

All 34 doctest targets succeed, with six actual doctests passed.
One nested runtime process reports another passed test, which these top-level counts exclude.
`integration-001/workspace-classification.json` records every failed test, its cause, and the corresponding log lines.

The contract runner passes all three legs: 14 authoring tests, three routing-coherence tests, and 16 guest adversarial tests.
The integrated runtime cache suite also passes all 13 tests.

## Baseline and retries

The clean baseline is `3a48ea15` in a separate detached worktree.
Its metadata, architecture inventories, and Docker provenance tests passed.
Its deployment inventory failed because the WMS overlay lacks a declared object-store Secret prerequisite.
The changed source fails the same test for the same Secret.
Bead `wamn-362o.58` tracks that inherited failure.

The evidence retains failed development runs.
These include the CLI ownership compile error, one stale workspace count, the non-Drop lint, and the missing database-owner fixture role.
Later runs fix each defect and pass its relevant proof.
`final-targeted-001` stopped before Cargo started and carries no test claim.

`deployed-001` built both canonical images, provisioned the scoped credential, and deployed the actual chart.
Its first Job passed HTTPS configuration, the exact public set, and issuer binding.
The observer then applied an invalid upper bound to the cache deadline.
It used the caller timestamp, which precedes the internal request timestamp by a small amount.
The service returned fresh evidence, but the observer rejected that valid ordering.
This failed run exits 1 and records successful cleanup.
The corrected observer passes the new `deployed-002` run.

Pre-commit `source.patch` files contain tracked diffs only.
They do not archive new files that were still untracked.
The source commits and final integration evidence identify the complete implementation.
