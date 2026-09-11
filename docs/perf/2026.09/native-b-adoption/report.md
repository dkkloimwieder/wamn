# Native dispatch implementation checkpoint

Owner: `wamn-0ct2.2`. The production substitution remains in progress.

B accepts one component provider for each fully qualified operation interface imported by the admitted closure.
Export-only handlers can repeat because wiring selects their exact admitted component facts.
A dependency digest preserves artifact provenance and cannot select among ambiguous imported providers.

## Source and mechanism

The command receipts record base commit `03e594e9cb97e88fb6177f7516c5fa0f206b56d4` and SHA-256 identities for the changed source.
The upstream source remains wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`, with Wasmtime 47.0.4.
The local modules use public native loading, resolution, plugin binding, and dispatch APIs.

Release admission derives provider uniqueness from declared application imports.
The native loader uses the full admitted import inventory.
It makes sure that every supplied byte buffer matches its admitted digest before native cache access.
Exact repeated facts share one workload component. Distinct facts retain separate component identities even when their bytes match.
Wiring resolution now retains its exact node-to-component map through lowering.

Each native invocation receives a distinct host authority scope.
The scope retains the verified caller and absolute deadline, then revokes its claims and resources when the call ends.
Guest initialization receives no invocation authority.
A request-owned host error receipt preserves typed policy refusals across the native error boundary.
Guest error text does not determine the refusal type.

Native readiness initializes a component under an enclosing deadline without running its application handler.
The production driver still uses its previous lifecycle until B completes the replacement and deletion contract.
No manual production cache, linker, or store lifecycle was removed in this checkpoint.

## Executed checks

These rows overlap. The final native selection contains 12 Rust tests.
The authenticated proof is one Rust test with six distinct scenarios.

| Evidence | Result |
| --- | --- |
| [Catalog admission](admission-imports-001/result.json) | 69 library tests and 13 manifest tests pass. Imported ambiguity refuses, export-only sharing succeeds, and exact provenance remains required. |
| [Native loader and invocation](loader-imports-001/result.json) | Eight tests pass. These include shared handlers, altered bytes, typed refusal, deadlines, and cancellation. |
| [Public native binding](binding-imports-001/result.json) | Two tests pass through the upstream resolver and host policy binding. |
| [Node provenance](node-provenance-001/result.json) | Seven resolution tests pass, including distinct complete facts with the same digest. |
| [Readiness and authenticated nesting](authenticated-nested-001/result.json) | All 12 selected tests pass. The authenticated test executes its original five scenarios. |
| [Explicit child initialization deadline](authenticated-nested-002/result.json) | The authenticated test passes with all six scenarios, including a permitted child that hangs during initialization. |
| [Final native tests](authenticated-nested-003/result.json) | All 12 tests pass after the lint fixes. The authenticated test retains all six scenarios. |
| [Focused Clippy](clippy-003/result.json) | The library and test targets pass. The new modules retain only unused-helper warnings until production integration. |

The final authenticated scenarios cover permission refusal, session fresh-only refusal, permitted nesting, child initialization deadline, child execution deadline, and cancellation.
They use real Ed25519 signatures, a local HTTPS issuer, the existing session verifier, and PostgreSQL permission lookup.
The refused children do not initialize. The permitted child retains the original caller, separate invocation scope, and parent deadline.
The child initializer receives no invocation authority before its deadline expires.
All authority scopes and guest memory return after each scenario.

The fixture uses PostgreSQL 18.6 from image `sha256:8d6ed4b5a1f44557313cab42a8951c53d1b86ccc899266bcee2ee1ee84076c14`.
The [first fixture receipt](authenticated-nested-001/database-probe.json) records the server response through the same localhost TCP path.
The [final binary identity](authenticated-nested-003/artifact.json) records the executed test artifact.
The cleanup receipts cover the [first](authenticated-nested-001/cleanup.json), [second](authenticated-nested-002/cleanup.json), and [third](authenticated-nested-003/cleanup.json) disposable containers.
Each removal uses the exact owned container identity.
No benchmark ran.

## Earlier failures and limits

The initial library check in `check-001` failed on adapter compilation errors. `check-002` records the corrected check.
The first two invocation runs lost the typed nested refusal when native dispatch formatted its outer error.
Those failures remain in `tests-001` and `tests-002`. The host error receipt fixes that boundary, and `tests-003` records five passing cases.

The first Clippy run reports local lint warnings.
The second run fails because the lint fix lacks a test import.
The third run passes after that import is added. All three receipts remain in this directory.

These checks exercise the native helpers and the real signed-session authorization path.
They do not establish a converted deployed route, frozen-candidate execution, or production workload teardown.
B retains ownership of those paths, predecessor removal, and the final integrated gate.
Production ownership must also clear immutable policy bindings if cancellation interrupts native resolution.
Native resolution rolls back returned errors, but cancellation has no native drop cleanup.
The WAMN workload owner must retain each active call and clear its own policy state on retirement.
The shared driver and connection authority changes wait for D and `wamn-ctc8.33` to release their files.
The earlier [B stop report](../native-b-dispatch/report.md) retains its findings against the superseded selection contract.
