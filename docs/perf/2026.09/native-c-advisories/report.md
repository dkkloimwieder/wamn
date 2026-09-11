# Native NATS and retained broker advisories

Broker advisories replace WAMN's separately retained dead-letter payload and correlation machinery. Payload loss after source retention is accepted.

`wamn-0ct2.7` owns the production substitution. This report records implementation checkpoints, not a completed landing.
The C worktree starts from published WAMN `79879412aceba021a5f77e273e6c1a21dff7dc34` on `work/native-c-advisories-20260910`.
Upstream remains unmodified wasmCloud 2.9.0 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`, with Wasmtime 47.0.4.
Each command directory records its exact arguments, output, and selected source-file hashes before and after execution.
The source hash inventories identify the exact inputs each checkpoint covered.
`checkpoint-001/source.json` records every changed source/document path for the local checkpoint, including deleted files.
`checkpoint-001/public-wit.json` confirms that the vendored public NATS WIT is byte-identical to the pinned upstream file.

## Implemented boundary

The production materializer imports named `events: wasmcloud:nats/jetstream@0.1.0` and calls the public attachment, fetch, acknowledgement, negative-acknowledgement and termination interfaces.
It retains its P2 CLI export and uses `wit_bindgen::block_on` for asynchronous NATS calls.
The guest enables the existing workspace `wit-bindgen` async feature locally.
The public native NATS WIT is vendored unchanged. No private runtime Rust source is copied.

WAMN retains exact release-registration selection, stream and durable provisioning, and refusal of consumer configuration drift.
The broker retains exhaustion and termination records in `WAMN_EVENT_ADVISORIES`.
The operator reader reports the source payload as available or unavailable. It does not reconstruct a missing payload or offer replay.
The Receiving proof expects the actual broker termination advisory while retaining real handler replay, business-state preservation and later valid-event progress assertions.

The implementation removes the custom consumer and message resources, host fetch/ack/nak/term path, payload DLQ model and publication, correlation identity, depth metrics, ack-lag metric, operator replay, and replaced tests.
The unused generic guest publisher is removed to prevent an administrative path from fabricating broker advisories.
Native host derived-event and router-tap publishers remain. The existing scheduler doorbell interface, implementation and setup remain.
The event broker remains separate from the scheduler broker.

The retained durable permits at most 64 pending acknowledgements, 64 messages and four MiB per pull, and one waiting pull.
The event broker explicitly retains its one MiB payload ceiling.
The materializer reads each native message once, including its headers, so it does not copy the payload again to read metadata.
The near-limit native delivery proof remains open below.

## Executed checkpoints

| Evidence directory | Command or observation | Result |
|---|---|---|
| `materializer-build-001` | Build the production P2 materializer | Failed because native `open_pull_consumer` takes owned strings. |
| `materializer-build-002` | Build, validate and inspect the corrected materializer | Passed. Native named NATS import, P2 CLI export, no WASI 0.3 import. |
| `materializer-build-003` | Rebuild, validate and inspect after WIT cleanup | Passed with the same public import and export shape. |
| `materializer-build-004` | Rebuild, validate and inspect after reading each message once | Passed. Latest checkpoint artifact; identity below. |
| `runtime-check-001` | Check runtime, CDC and operator libraries | Failed because the retained doorbell WIT definition was accidentally removed. Restored verbatim from the base. |
| `runtime-check-002` | Repeat the scoped library check | Runtime and CDC passed. The operator reader needed an explicit boxed-error conversion. |
| `runtime-check-003` | Check runtime, CDC and operator libraries | Passed with unchanged captured source. |
| `runtime-check-004` | Check all targets for runtime, CDC, operator and integration proof packages | Passed with unchanged captured source. |
| `advisory-reader-001` | Observe real broker advisories and source deletion | One test passed, zero failed or ignored, 283 filtered. Two retained advisories, two available payloads, two unavailable payloads after deletion. |
| `advisory-reader-002` | Add later valid delivery on the exhausted consumer | One test passed, zero failed or ignored, 283 filtered. Exhaustion 1, termination 1, available 2, unavailable 2, later valid 1. |
| `registration-tests-001` | Retained registration and host-publisher tests | Twelve reported passes: eleven executed unit proofs and one existing live-test self-skip. Zero failed or ignored, 304 filtered. |
| `registration-tests-002` | Repeat after the four MiB pull-bound correction | Eleven executed unit proofs and one existing live-test self-skip. Zero failed or ignored, 304 filtered. |
| `event-wire-cdc-tests-001` | Event wire and CDC unit tests | Event wire: 12 passed. CDC: 25 reported passes, including one existing live-test self-skip. Zero failed, ignored or filtered. |
| `wit-coherence-001` | Built host and guest WIT contract checks | Two passed, zero failed, ignored or filtered. |
| `materializer-tests-001` and `materializer-tests-002` | Materializer unit tests before and after the single-read correction | Each run: 10 passed, zero failed, ignored or filtered. |
| `cli-verbs-001` and `cli-verbs-002` | Operator command surface, then explicit refusal of the retired command | Each run: one passed, zero failed or ignored, nine filtered. |
| `effect-conformance-001` and `effect-conformance-002` | Retained effect instrumentation contract, then orphan-constant cleanup | Each run: four passed, zero failed, ignored or filtered. |

The latest materializer artifact has SHA-256 `b1e476940f6de53c8c5d15929ed321bc233622632760604dd6a797d9c9ac01ef` and 15,674,092 bytes.
The earlier artifact identities remain in their command directories.
Artifact inspection proves the native named NATS import, P2 CLI export and absence of WASI 0.3 interfaces; it does not establish native host execution.

The strengthened operator-reader test executable has SHA-256 `a453fac95d11524f23efa2e1b26b9e4097a3e266341c14290ed30b6a4116a3d0` and 315,152,912 bytes.
Its broker uses the production `nats:2.10-alpine` image, resolved to NATS 2.10.29 and image ID `sha256:8b9f712ae2148dcbd9d7f3d16a942d515e290526e5d018500931eab62b18e045`.
Each exact disposable container was removed, and the runner confirmed its absence.
This test proves broker record decoding, operator payload reporting and broker consumer progress. It does not exercise the native guest plugin or actual handler replay.
The self-skipped publisher and CDC tests lacked their respective live broker environment variables; they are not counted as executed live proofs.

## Public API evidence

The broker's [exhaustion schema](https://raw.githubusercontent.com/nats-io/jsm.go/main/schemas/jetstream/advisory/v1/max_deliver.json) identifies the stream, consumer, source sequence and delivery count.
The [termination schema](https://raw.githubusercontent.com/nats-io/jsm.go/main/schemas/jetstream/advisory/v1/terminated.json) describes the other retained disposition.
The live records above establish the actual NATS 2.10.29 numeric `consumer_seq` representation.

Native `targets_wasip3` classifies WASI 0.3 interfaces, so NATS 0.1 imports alone do not select a P3 service.
Wasmtime 47 `TypedFunc::call_async` uses its concurrent implementation when concurrency support is enabled.
That support defaults to true, and the native engine also enables component-model async support.
These source checks do not replace execution of the final production materializer.

## Remaining production proof

Native plugin registration and binding placement remain pending the owner's configuration decision.
The local checkpoint is not ready for production integration, and no production substitution is claimed.
The host must hold credentials that permit the intended stream and exact durable while refusing foreign access.
The tenant capability registry remains closed. No new tenant NATS capability is implied by the platform materializer import.
The native binding name identifies configuration and does not authorize a caller.

The final native materializer must pass real Receiving replay and poison progress, exact registration drift, foreign-access refusals, and bounded delivery pressure.
The integrated proof must cover a near-limit broker message so a fetch bound cannot prevent later valid-event progress.
The final source and artifact identities must accompany those results before the owning issue closes.
No benchmark ran, and these correctness checkpoints establish no performance improvement over the 2.9 baseline.

The [historical C report](../native-c-nats/report.md) retains its original host-owned payload contract and private-message-handle finding.
The accepted advisory trade supersedes that contract. It does not retroactively make the historical experiment pass.
