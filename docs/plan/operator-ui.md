# Deferred operator client work

The generated Rust operator screens and shared request layer are implemented.
Their current rules belong in [operator clients](../architecture/execution.md#operator-clients).
Their required observations belong in [operator tests](../testing/application-tests.md#operator-outcomes).
This page retains only deferred extensions from the approved client designs.

## Web client

TypeScript generation remains deferred under `wamn-10yt.5.1` until a concrete web consumer requires it.
It must consume the same effective-release client-contract representation as the Rust emitter.
It must not infer deployment hosts or operation routes from package names.
The transport owns route selection, credentials, request envelopes, errors, and pagination according to each declared operation.

Generated TypeScript packages must expose public operations only.
Private event handlers must remain absent.
Distribution names require explicit package metadata rather than an inferred application identity.
Each invocation must resolve to the exact canonical operation identity selected by the release.

Framework-specific UI code remains separate from generated bindings.
A web application must keep server authorization authoritative and preserve the shared meaning of refusal, completion, partial completion, and uncertainty.
A browser adapter does not inherit Ratatui implementation details.
A real consumer must establish the required web artifact and distribution path before that path is built.

## Multiple input items

The generated terminal currently submits one outer envelope item for each submission.
Repeated fields inside that item, such as receipt lines, remain supported.
Submitting multiple outer items requires a separate behavior decision.

That decision must define how the UI displays independent item outcomes.
It must also define captured retry when different items have different completion evidence.
It must preserve the operation's declared transaction and replay guarantees without implying cross-item atomicity.

## Declared screen population

Automatic population of repeated inputs from projection rows needs an explicit declaration.
The same requirement applies when a list supplies a selector's values.
The current contract declares no general mapping for those cases.

Until a named consumer supplies that requirement, applications compose generated screens with ordinary Rust.
No field-name heuristic can supply a record, revision, route, or repeated input.
An eventual declaration must preserve typed input bounds and refuse incompatible mappings.
The existing generated form remains a typed editor without inferred prefilled values.
