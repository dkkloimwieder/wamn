# Receiving composition parity

Receiving composes four generated screens through the shared terminal loop.
The composition maps the order projection into line editors and location selection.
The shared submission layer owns request validation, captured retry, uncertainty, and pending state.
The terminal selects ordinary or fresh credentials for each attempt.
Identity login helpers and their tests remain unchanged from `c0ac2cf6`.

The [focused run](focused-002/command.log) passes 128 tests, with no failures or ignored tests.
That run includes 17 Receiving interaction tests, three transport workflows, six login tests, and two transport tests for the binary.
The [emitter run](gates-001/command.log) passes 11 tests.
The [final local gate](gates-004/result.json) records successful scoped Clippy, the complete outgoing-byte assertion, and native builds.
The [materialization run](materialize-002/result.json) writes and checks Receiving, its Acme overlay, and WMS against fresh PostgreSQL databases.

The [negative control](negative-control-001/result.json) removes numeric scale from the composition mapping.
The outgoing-byte test fails with quantity `4` instead of `4.0000`.
It passes after exact source restoration and recompilation.

The [live journey](live-002/environment/client/result.json) runs the actual `wamn-receiving` binary against a served development environment.
The terminal retains the refused entry, accepts a quantity correction, and submits the selected location.
The [database result](live-002/environment/client/19-committed-db.stdout) contains one claim and one receipt.
The entered quantity remains `2.5000`, and the blank second line produces no receipt line.
The journey also proves that success spends the entry and that reopening an order clears its inputs.
Terminal exit returns zero and restores the original terminal attributes.
Fixture cleanup and [environment cleanup](live-002/environment/result.json) pass.
Database counts measure committed effects, not HTTP attempts.
The recording transport tests cover request counts and the absence of automatic replay.

The first materialization attempt lacked its package schema.
The first focused attempt contained a test fixture syntax error.
Early lint attempts found local style warnings that the final gate resolves.
The first live attempt stopped before terminal entry because its database helper supplied the URL through the wrong connection argument.
The corrected helper passes the [database preflight](database-preflight-001/result.json) before the successful live run.
The retained failed runs remain failed evidence.

The same source change deletes the old model, reducer, request builder, and screen modules after parity passes.
It repoints the Receiving recipe and preserves the developer-owned crate and its public login helpers.

The workspace sweep runs at clean commit `9e3307eb4ecab84efe2009ceece0f5bad69d170a` after the identity proof lands.
It reports 2,183 passing tests, six passing doctests, and 74 failures, with exit code 101.
The test count includes 85 explicit self-skips, which do not prove their live behavior.
No tests remain ignored, and the two existing schema regeneration exclusions remain in place.
The [source receipt](workspace-001/source-stability.json) records a clean tree before and after the sweep.

The [baseline comparison](workspace-001/baseline-comparison.json) preserves all 68 cutover failure identities and causes.
Six further failures lack their declared live fixtures.
Compared with the effects sweep, the corrected TUI revision fixture passes and the new session-client test lacks its journey document.
The identity lane retains that test's separate deployed proof under `wamn-ctc8.15.5`.
No new TUI failure appears in the sweep.
The [contract follow-up](contract-diff-001/result.json) exits zero with 36 passing tests across its three legs.
