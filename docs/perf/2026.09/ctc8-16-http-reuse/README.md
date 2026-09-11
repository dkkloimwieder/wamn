# Bounded HTTP reuse

`wamn-ctc8.16` owns this implementation and its proofs.
The work starts from main `0cdb9af37e065af305c089e9035526384ae24844`.
The approved implementation commit is `bbcdbecd3160377f7d8d5f0ebe2186754e198afe`.
It contains the same patch as `2d1e2468`, rebased onto main `b8c527975028931386cf11849a8e6144e9f14848`.
The owner directs correctness work without benchmarks.
This report makes no latency or throughput claim.

## Transport boundary

The native 2.9 connector does not accept WAMN's approved address through its public interface.
The existing Reqwest wrapper also hides the connection lifetime required for hard socket accounting.
The replacement uses the existing Hyper and Rustls libraries directly, without an upstream patch.

The connector opens only the approved address.
TLS still authenticates the logical server name with the platform trust verifier.
The transport does not follow redirects, use ambient proxies, or retry requests internally.
HTTP/1.1 and TLS-negotiated HTTP/2 use the same authority and quota path.
The change does not admit new guest interfaces.

Every request retains the existing invocation, release, binding, credential, and destination authorization.
The retained client stores transport state, not caller claims, credential headers, bodies, or trace context.
Each request carries its own headers and trace context.
Each component invocation still receives a fresh store.

## Resource limits

A socket is one network connection.
A retained client can own several sockets.
A logical connection is the tuple of tenant, project, environment, and connection instance.
Its quota covers all bindings and credential generations in one process.
These limits do not establish a distributed quota across service replicas.

| Resource | Initial limit | Scope |
| --- | --- | --- |
| Retained clients | 128 | Process, including retired clients with live tasks |
| Sockets | 64 | Process, including connections in setup, use, idle state, or drain |
| Concurrent requests | 32 | Process, through response collection |
| Sockets | 8 | Logical connection, across generations |
| Concurrent requests | 8 | Logical connection, across generations |
| Idle sockets | 2 | Client |
| Idle expiration | 30 seconds | Client pool |
| Request body | 8 MiB | Request, before copying into owned transport storage |
| Response body | 8 MiB | Response, enforced during reading |
| Header bytes | 32 KiB | Request or response, including response trailers |
| Header fields | 100 | Request or response, including response trailers |
| Transport deadline | 30 seconds | Request, including connection setup and body collection |

The limits bound connection retention and buffer use.
They are starting product limits, not measured machine capacity.
Larger transfers refuse or lose their response under the existing outcome contract.
At capacity, new work refuses before dispatch without a waiting queue.
Retiring a client does not refund its permits while its sockets or tasks remain alive.
The pool refuses reuse after 30 idle seconds.
Hyper checks idle sockets on a 30-second timer, so physical closure can take about 60 seconds.
Those sockets retain their permits until closure.

## Outcome boundary

Unavailable capacity maps to the existing refusal before dispatch.
A deadline before the response head maps to timeout.
A body failure after the response head maps to response loss.
Other failures in flight remain uncertain.
Cancellation retains the existing effect guard.
A timeout or cancellation does not prove that the remote operation did nothing.

## Evidence

The first diagnostic compilation passed for the runtime and execution driver libraries.
It took 380.839 seconds in a new debug target directory.
Source edits continued during this compilation, so it is not a frozen-source runtime proof.
Its exact command and output live in `build-001`.

All three targeted HTTP runs passed all 35 tests, including 15 transport tests.
The runs used real TCP and TLS sockets for reuse, protocol, generation, quota, cancellation, and failure assertions.
Their exact commands and output live in `native-001`, `native-002`, and `native-003`.
The final two runs also vary tenant, project, and environment independently.
It tests disconnect, redirect, and unavailable responses across cleartext HTTP/1.1, TLS HTTP/1.1, and HTTP/2.
Each case observes exactly one socket and one POST request.

The related regression run passed 13 blobstore tests, 17 router tests, and three guest error tests.
These tests retain frozen candidate bindings, original caller identity, fresh stores, shared transport ownership, and errors without guest traps.
The commands and output live in `regression-001`.

The real `http-request` guest built successfully in its separate debug workspace.
Its command and output live in `http-guest-build-001`.
This build does not prove execution through the production driver.

The integration and service test compilation passed in `integration-build-001`.
The first live runs each executed one named test and failed during fixture setup or mutation.
The reuse fixture tried to update an immutable connection binding.
The nested fixture tried to extend a sealed release.
Both runs removed their own PostgreSQL and registry containers and anonymous volumes.
Their commands, failures, and cleanup records remain in `live-reuse-001` and `live-nested-001`.
The fixture corrections retain the database guards.
The second live runs reached the intended refusals but failed because expected messages omitted the generated `ConnectionError::` prefix.
Those failures remain in `live-reuse-002` and `live-nested-002`.
The corrected assertions retain exact equality and the existing error classes.

The final integration compilation passed in `integration-build-003`.
Both final live proofs passed through the production driver with fresh PostgreSQL 18 and registry containers.
Each run executed exactly one named test, with no ignored tests or self-skip.
Each runner removed only its own containers and anonymous volumes.

| Evidence | Result | Scope |
| --- | --- | --- |
| `native-003` | 35 passed | HTTP authority helpers and real socket, TLS, generation, quota, and outcome tests |
| `regression-001` | 33 passed | Blobstore candidate authority, router identity and lifecycle, and guest error handling |
| `live-reuse-003` | 1 passed | Real guest reuse, connection disablement, frozen candidate drift, and credential generation changes |
| `live-nested-003` | 1 passed | Warm direct child, nested refusal, original caller and child identity, and no extra network request |
| `clippy-002` | Exit 0 | Scoped library lint, with existing warnings |
| `static-001` | Exit 0 | Changed Rust formatting, shell syntax, whitespace, and source hashes against the final live proof |
| `guard-002` | 10 passed | Scope guard, 24 unsafe mutations, one unrelated-counter control, and all nine Cargo steps |
| `native-004` | 35 passed | HTTP tests after seven test-only lint corrections |
| `clippy-003` | Exit 0 | All targets in six affected crates, with existing warnings and seven new test warnings |
| `clippy-004` | Exit 0 | Runtime all-target lint after the narrow correction, with no transport warnings |
| `deployed-001` | Exit 0 | Canonical host and gate images, the deployed membership Job, and exact cleanup |

The native, related regression, two live proofs, and guard runs cover 80 distinct targeted tests.
Later HTTP runs repeat 35 of those tests rather than add new cases.
The live proofs use cleartext HTTP/1.1.
The socket tests separately cover TLS HTTP/1.1 and HTTP/2.
The final integration binary hash is `45ef0e7dc8612fe9189a1cdcd69808587ecc72fecdcf0e109eb12e94c25c26f1`.
The HTTP guest hash is `d3e161308374d7dbb8d4d6a28267b63218d7492c9c7ed5ffe0475bbfacabdfa6`.

The all-target lint in `clippy-001` failed on the existing `wamn-10yt.80` error in `receiving_command_histories_live.rs`.
The HTTP change leaves that peer-owned file untouched.
Main commit `1d38b6da38a460753d89f877ec0a0c68345a7d60` carries its separate fix.
The rebased HTTP commit includes that fix.
The all-target run in `clippy-003` passes, but reports seven style warnings in the new transport tests.
The narrow test correction changes no production code or test assertions.
The subsequent `native-004` run passes all 35 HTTP tests.
The `clippy-004` run passes with no diagnostics in the transport module or its tests.
Scoped lint does not replace the final integrated workspace run.

The deployed run uses clean source `bbcdbecd3160377f7d8d5f0ebe2186754e198afe`.
It builds the standard Dockerfile `host` and `gates` stages and deploys one rebuilt host through the 2.9 operator.
The route fixture passes its eight P3 protocol cases before deployment.
The in-cluster Job then proves seven membership cases through the real provisioning CLI and HTTP route.
Absent membership returns 401, grants return 200, removed roles return 403, and membership revocation returns 401.
Repeated grants and revocations retain their expected results.

The Job receipt, deployed image identities, and cleanup receipt live in `cluster-membership-001`.
Every file in its `evidence.sha256` matches the recorded hash.
The runner removes its own cluster, containers, images, and private scratch directory.
The existing `wamn` cluster remains outside the run.
This is a deployed authentication regression, not an outbound pooling or performance proof.

The workspace comparison helper imports only committed classifier and failure-cause helpers.
Its offline `comparison-selfcheck-002.json` reproduces all 75 P3 and 78 PAT failure identities and causes.
It also preserves target counts, explicit self-skips, unresolved results, and the original Cargo exit.
The earlier `comparison-selfcheck-001.json` remains diagnostic evidence from the helper that used an uncommitted peer dependency.
The final helper removes that dependency and refuses to overwrite existing comparison output.
These offline checks do not execute workspace tests or decide acceptance.

## Integrated workspace

Main integrates the unchanged HTTP patches onto the published Receiving repair `ccd3ac401f7be09ddca39d43b727dfa092297db5`.
The integrated source is `dce31af910ba7b586e3537ab2b1cfea784ea2066`.
Its production commit is `a7068545`, which carries the same patch as the deployed source commit `bbcdbecd`.
The source worktree remains clean at the same commit before and after the full run.
The exact command, environment names, output, and source receipts live in `integrated-workspace-001`.

The standard serialized workspace run exits 101.
It reports 2,231 passing tests, six passing documentation tests, and 81 failing tests across 38 targets.
Its 85 explicit self-skips remain unchanged, so the reported passes do not establish an exact count of executed proofs.
No benchmark fixture is armed, and the run reports no measured tests.
The command excludes only the two recorded schema-regeneration tests.

A baseline is a retained earlier test run.
The comparison uses the PAT baseline at `437672fcae8f76ad1df6cde705524a267dac1e04` and the earlier P3 baseline.
The [exact comparison](integrated-workspace-001/workspace-comparison.json) retains every failure identity and cause.
Against PAT, 77 failures match exactly and one existing WIT-scanner failure differs only in its absolute checkout path.
The scanner and its four cited WIT files remain byte-identical between the two source commits.
The [recorded comparison assertions](interpretation-checks-001/result.json) pass without changing the comparison rules.

The three added failures name missing fixture inputs, not executed behavior failures.
The Receiving UPDATE test requires `WAMN_RECEIVING_PG_URL` and passes in its [separate PostgreSQL proof](../receiving-update-projection/regression-positive-001/regression-result.json).
That test source remains unchanged from the published Receiving repair.
Both HTTP tests refuse before fixture setup because `WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET` is absent.
Their separate armed runs pass in `live-reuse-003` and `live-nested-003`.

No PAT failure disappears, and no failure identity is duplicated or unresolved.
The target names, target counts, explicit self-skip names, and self-skip counts remain unchanged from PAT.
All 75 P3 baseline failures match exactly.
The three added PAT failures and three new fixture failures account for the difference from P3.
This is not a green workspace run, and it supplies no new live proof for an unconfigured fixture.
The completed comparison finds no new behavioral regression from the HTTP change.

The main integration audit preserves 22,731 unrelated files and 22,675 index entries.
It reconciles 177 byte-identical copies of owned evidence through recoverable backups.
The audit receipts live in `main-landing-001`.
The helper also passes three offline tests for preservation and conflict refusal in `integration-smoke-001.json`.

The approval check rejected two earlier attempts to replace the fresh-client guard with runtime tests alone.
The owner now directs a preventive guard for the complete client isolation key.
It must refuse invocation identity in that key and retained clients outside it.
The guard now enforces the complete key, derived equality and hashing, scoped client storage, and attested scope inputs.
It accepts an unrelated static counter and refuses an unscoped static client.
The nine Cargo steps remain unchanged.
The first compiled guard run passed eight tests and failed two mutation fixtures.
One fixture matched two peer fields, and another moved a derive attribute onto its inserted type.
The corrected fixtures retain exact one-site mutations and require the intended refusal.
The final frozen run passed all ten tests in `guard-002`.
The earlier run remains in `guard-001` and does not establish a frozen-source proof.
These runner tests use fake Cargo for the nine steps and do not claim that those lint steps pass.

Source inspection also found a separate nested authority mismatch, tracked by `wamn-ctc8.33`.
Retargeting changes the executing child identity but retains the parent wiring and node.
The existing HTTP and blobstore snapshot compares those different identities.
Pooling does not correct that authority model.
The owner directs pooling to land first with the existing fail-closed denial unchanged.
The correction follows immediately under `wamn-ctc8.33`.
It authorizes the executing child as B and preserves A as the origin, following the existing origin/executor ruling.
The passing nested proof establishes refusal and identity preservation, not successful nested HTTP dispatch.

Beads and Git own completion and publication status.
This report records the approved correctness scope and its exact proof sources.
The owner removes benchmarks as a condition for this correctness landing.
Performance evidence remains deferred and unclaimed.
