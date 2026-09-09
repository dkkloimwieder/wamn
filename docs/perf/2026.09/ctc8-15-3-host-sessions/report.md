# Host session admission

`wamn-ctc8.15.3` passed its deployed proof at source `581016ee975d65ebaecd470619f8fae750a769ba` on 2026-09-09 UTC.
This source includes main `2a4cd288`.
The final integration sweep is not yet recorded here.

## Scope

Hosts verify signed sessions against their configured issuer, organization, and exact environment audience.
Each request reads one fresh tenant permission union through the existing HTTP admission authority.
The host checks token and key age again after that read.
Nested calls preserve the original caller and credential kind.
Hosts receive public keys only.

Current release writers, readers, and fixtures use one manifest format, v1, and its v1 OCI media type.
The route policy uses `auth-policy.modes`, with no old scalar fallback.
The existing `catalog.release_manifest_v3_snapshots` table remains the generic byte store.
No database object, grant, or compatibility layer was added for this format correction.
The JWT proposal and its section 8 proof list remain unchanged.

## Deployed result

[Run 007](deployed-007/session-host-proof-journey.receipt) passed both two-host cases and cleanup.
Its [wrapper log](deployed-007.log) records exit 0.
The runner built the standard `host`, `gates`, and `identity` Dockerfile stages from clean source.
The retained image records bind the actual Pods to those images.

The [PAT journey](deployed-007/production-route.log) passed all thirteen routes through one integration test.
The [session fixture](deployed-007/session-host-fixture.log) published the actual v1 release.
The [nested-call receipt](deployed-007/session-nested.receipt) records two actual component invocations with the original human identity and session credential kind.
The nested test compares the complete operation response with independent database facts.

Each case first sends the same valid session token to two distinct host processes.
The runner then removes its signing key.
After the 300-second key window, both hosts must return HTTP 401 twice while the token remains valid.
The [reachable case](deployed-007/host-session-reachable.receipt) also requires HTTP 200 from JWKS, the public-key endpoint, with the removed key absent.
The [unreachable case](deployed-007/host-session-unreachable.receipt) requires a connection or timeout failure from that endpoint.
It uses fresh workloads, Pods, and host identities.

Both [reachable](deployed-007/host-session-reachable-process-continuity.json) and [unreachable](deployed-007/host-session-unreachable-process-continuity.json) records preserve the same container IDs, start times, and restart counts across each window.
All four host processes record zero restarts.
The [cleanup receipt](deployed-007/cleanup.receipt) records removal of the owned deployment resources and temporary credentials.
The frozen `wamn` cluster remains outside this proof.
The run's `evidence.sha256` passed its integrity comparison.

## Local results

The following current-source runs passed 143 tests, with no failed or ignored tests in their selected scopes.
Each log records the exact command, source hash, and exit status.

| Scope | Passed | Log |
| --- | ---: | --- |
| Public-key cache and session verifier | 20 | [Cache and verifier](local-cache-verifier-581016ee.log) |
| Route modes and authentication | 20 | [Routing](local-routing-581016ee.log) |
| Catalog and manifest digest | 69 | [Catalog](local-catalog-581016ee.log) |
| Release publication | 19 | [Publisher](local-publisher-581016ee.log) |
| Host executable | 10 | [Host](local-hostbins-581016ee.log) |
| Built-byte declarations and HTTP observer | 4 | [Fixture and observer](local-declaration-observer-581016ee.log) |
| Fresh human and service PAT authentication | 1 | [Fresh PostgreSQL 18 regression](local-pat-regression-581016ee.log) |

The cache log also preserves a sandbox refusal and an accidental zero-test invocation without `test-util`.
Neither attempt counts as validation.
The final invocation enables `test-util` and passes all 20 tests.

The [scoped session proof](local-session-route-restored.log) separately passed against fresh PostgreSQL 18 with restricted credentials.
It covers role unions, tenant isolation, permission changes, signed role snapshots, one permission read per warm request, and expiry during a blocked read.
The [live manifest test](local-manifest-publication-44e1e91d.log) passed nine cases, including actual publication and an exact retry that does not push again.
That run used a verified, disposable registry and a synthetic manifest.

## Deliberate faults

Each temporary fault compiled and failed its intended assertion.
The production source was restored before the passing proofs and deployment.

| Temporary fault | Evidence |
| --- | --- |
| Remove the final age check | [Age failure](mutant-age.log) |
| Remove tenant isolation from the permission query | [Tenant failure](mutant-tenant.log) |
| Mark a session caller as a PAT caller | [Credential-kind failure](mutant-kind.log) |
| Restore the observer's old five-second refusal limit | [Budget failure](mutant-observer-budget.log) |

[Restored runtime hashes](local-runtime-restored.sha256), the restored scoped proof, and the [restored observer tests](local-observer-refusal-restored.log) record recovery.
The observer now allows ten seconds for a host refusal after the host's five-second key fetch.
The production fetch limit and 300-second key window remain unchanged.

## Earlier runs

All seven deployment attempts retain their original logs and cleanup receipts.
Only run 007 is the complete two-case pass.

| Run | Outcome | Reason |
| --- | --- | --- |
| [001](deployed-001.log) | Exit 101 | The disposable declaration named an authored digest instead of the built component digest. |
| [002](deployed-002.log) | Canceled during build | Two replicas did not establish placement on two distinct hosts. |
| [003](deployed-003.log) | Canceled during build | The runner needed evidence that neither host process restarted. |
| [004](deployed-004.log) | Exit 5 | The runner expected one JSON List where kubectl emitted two objects. |
| [005](deployed-005.log) | Exit 101 | The nested fixture expected old fields instead of the completed PAT journey's current fields. |
| [006](deployed-006.log) | Exit 1 | Nested and reachable proofs passed, but the unreachable observer reported a request failure after 305 seconds. |
| [007](deployed-007.log) | Exit 0 | Nested, reachable, unreachable, and cleanup proofs passed. |

The canceled wrappers did not retain numeric exit codes.
Run 006 timing supports a race between the two five-second request limits, but its generic error does not establish that cause independently.
Commit `581016ee` adjusts only the observer budget and failure diagnostics.
The built-digest correction closes `wamn-8o40` at `1d691c9c`, with the successful run 004 PAT journey as evidence.

## Boundaries

Unknown or empty session roles produce no permitted operations in the scoped database proof.
The deployed PAT journey exercises HTTP 403 through the shared operation guard.
This is combined evidence, not an actual session request with an empty grant set receiving HTTP 403.
The nested native test uses the approved loopback HTTPS bridge.
The two deployed host Jobs exercise the cluster network.

Normal package routes remain PAT-only.
`wamn-ctc8.15.4` owns fresh-only operations and the paired nested session/PAT refusal proof.
`wamn-ctc8.15.5` owns TUI login and the existing-bench comparison.
This report makes no latency claim and does not close the parent JWT proposal.

The final bounded credential scan covered the complete evidence directory, including this report, before this report update.
It counted 368 files, 3,268,082 bytes, and 71,799 lines, with no credential-material candidates.
The scans cover recognizable token strings, credential fields, private keys, Secret payloads, and decoded base64 material.
They do not guarantee detection of arbitrarily encoded secrets.
