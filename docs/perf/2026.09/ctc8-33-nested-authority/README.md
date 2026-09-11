# Nested effect authorization

`wamn-ctc8.33` separates the original wiring and root component from the component that performs an HTTP or blobstore effect.
The original wiring can belong to a different package from its root component.
The host preserves both identities across nested calls and keeps the original caller.
The shared database query resolves the executor's operation and connection binding independently from the original wiring node.
The shared manifest check requires the exact declared operation dependency path, not release membership alone.

The change adds no database objects, grants, transport, or wire vocabulary.
Direct candidate execution retains its frozen bindings, and nested candidate execution remains refused.
The transport still authorizes each request before it selects a reusable connection.
No benchmark ran.

The published [pooling proof](../ctc8-16-http-reuse/live-nested-003/stdout.log) records the original nested refusal and zero additional wire requests.
That baseline remains unchanged.
The new live proof uses separate packages for the wiring and root component.
It requires authorized nested calls, unchanged caller identity, refusal of an undeclared import, and refusal after connection disablement.

The [targeted run](native-001/stdout.log) passed 117 tests: 36 HTTP, 58 blobstore, four snapshot, two authority, and 17 driver tests.
The [run receipt](native-001/result.json) records exit 0 and 7.236 seconds.
Those tests include two-hop origin preservation, exact dependency coordinates, shared blobstore authorization, and candidate refusal.
They do not replace the live database and deployed gates.

The [guest build](guest-build-001/result.json) passed in 76.360 seconds.
The first [Rust build](build-001/result.json) failed on a test-only digest constructor.
The corrected [Rust build](build-002/result.json) passed before the later test-manifest ordering correction.
The first [nested live run](live-nested-001/stdout.log) refused the test manifest's noncanonical ordering before dispatch and removed its temporary containers.
These failed runs remain separate from later results.

The [unified build](build-003/result.json) passed after that fixture correction.
It compiled the runtime, driver, integration library, CLI library, and CLI-owned SQL test in one Cargo invocation.
The [nested live proof](live-nested-002/stdout.log) passed one armed test and removed both temporary containers.
Two direct calls opened and reused one connection, and two nested calls reused that same connection with the original PAT caller.
The undeclared export and disabled connection both refused before dispatch, and re-enabling the connection restored a successful nested call.

The [SQL proof](live-bind-001/stdout.log) passed one armed test on PostgreSQL 18.6 and reported successful cleanup.
The wiring, original component, and executing component belonged to three different packages.
The executor's blobstore binding resolved, while nine altered origin or executor coordinates refused.
This database fixture uses administrative credentials and does not prove least privilege.
The HTTP proof separately uses the existing callable-HTTP reader authority without new grants.

The [direct regression](live-direct-001/stdout.log) passed with its frozen-candidate and generation-isolation cases and removed its temporary containers.
The first [Clippy run](clippy-001/result.json) passed across all targets of the four affected packages.
It reported two new string-assignment warnings in the SQL test fixture, which the follow-up changes to `clone_into` address.
Existing warnings remain outside this change.
