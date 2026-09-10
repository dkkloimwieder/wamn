# Session client live proof

The live proof passed at clean source `8204b35d0bae424803cc231c7e375d44435ef0cf` on 2026-09-10.
The [outer exit record](deployed-002.exit) is 0.
The [final receipt](deployed-002/session-client-proof-journey.receipt) records all client cases and cleanup as passing.
The [functional report](functional-report.md) retains the earlier client tests and generator boundary.

## Results

The exact client selector ran once: one passed, zero failed, zero ignored, in 19.08 seconds.
Its [log](deployed-002/session-client.log) and [receipt](deployed-002/session-client.receipt) retain the result.
The production-route setup and session fixture each passed one test, in 36.27 seconds and 0.93 seconds.

The test uses the actual Receiving login functions and the existing credential-provider boundary.
Login exchanges a human PAT over HTTPS with the real identity service.
Ordinary calls reuse the in-memory session and resolve the expected result from the actual project database.
An explicit fresh call uses the PAT without another exchange.
Expired and revoked PATs each receive a real exchange refusal without an application call or PAT fallback.

The counter fixture proves that a late nested refusal does not replay prior work.
The session call receives the exact fresh-credential refusal after its first effect commits.
The explicit PAT call then succeeds.
The [counter receipt](deployed-002/fresh-only-prior-commit.receipt) records counters 1 and 2.
The fixture also keeps the other tenant's counter at zero.

## Scope

The identity exchange reaches the deployed service through the owned HTTPS bridge with its trusted fixture certificate.
Application calls use the actual client, production router, and guest execution inside the test process.
They are not client HTTP calls to deployed hosts.
The functional tests prove expiry renewal with a controlled clock.
This live run does not wait for token expiry or claim another renewal proof.

The run adds no production session routes and changes no protocol version.
It does not repeat the completed two-host key-removal windows.
It makes no latency claim and runs no benchmark.
`wamn-ctc8.15.5` remains open for its existing benchmark acceptance and the unresolved scope of the earlier benchmarking restriction.
The separate generated-TUI parity work retains ownership of the Receiving composition.

## Attempts and cleanup

[Attempt 001](deployed-001.log) exits 1 before any identity or application request.
The first node cannot find the gates image through CRI immediately after `kind load` reports success.
CRI is the container runtime interface.
Image tags and the Docker image identifier agree throughout the retained records.
The [cleanup receipt](deployed-001/cleanup.receipt) passes.
Finding `wamn-y2ml` retains this intermittent failure for investigation.

[Attempt 002](deployed-002.log) uses the same source and passes the image lookup on all three nodes.
It uses the existing 60-second cleanup delay to permit diagnostics if the failure recurs.
The failure does not recur, so no diagnostic Docker calls run during that delay.
No source correction or relaxed image test separates the attempts.
The [cleanup receipt](deployed-002/cleanup.receipt) passes.
After cleanup, Docker and kind list only the three pre-existing `wamn` nodes and their cluster.

Both attempts remain intact, including the failed run.
The [successful-run hashes](deployed-002/evidence.sha256) pass.
The new `live-SHA256SUMS` covers both attempts, their outer logs and exits, and this report.
The retained files contain no matches for credential-bearing database URLs, long bearer tokens, compact JWTs, or private-key headers in the recorded scan.
That pattern scan does not guarantee that every possible secret form is absent.

## Command

```bash
WAMN_JOURNEY_HOLD_SECONDS=60 tools/receiving-cluster-journey-run --session-client-proof --apply \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-5-session-client/deployed-002
```

The runner source stays clean throughout both runs.
The local source commit adds only the integration fixture, its dependencies, the runner mode, and the separate `[SESSION-CLIENT]` procedure.
The existing journey document harness failure remains separately tracked as `wamn-rs0s`.
Neither follow-up is silently treated as fixed by this passing client proof.
