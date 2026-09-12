# Native NATS source checkpoint

Issue: `wamn-0ct2.4`.
Outcome: stopped substitution, with no production adoption.
The owner retains the Rust host policy and defers the adapter decision to `wamn-0ct2.6`.

## Scope and ordering

The WAMN source is `88db1d3a138fe6f3ae21e450223c3f30d1a64532`.
The runtime source remains unmodified wasmCloud 2.9 at `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
The source check finds no technical dependency on B.
The materializer calls the existing router delivery bridge, which invokes `RouterDriver::execute_with_causation`.
Native NATS can bind service imports without its separate exported-handler dispatch path.
The owner removes C's ordering dependency on B and retains B2's dependency on B.

## Public boundary

Native NATS exposes broker operations through public guest WIT interfaces.
It provides bounded pulls, message metadata, acknowledgements, delayed redelivery, termination, and publication with a server acknowledgement.
Its named bindings separate connection credentials from workload configuration.
Those mechanisms do not establish WAMN's registration and dead-letter policy.
A dead letter is the retained record of a failed event.

The native Rust connection, pull-consumer, and message-handle modules are `pub(super)`.
The exported `WasmcloudNats` type provides construction and workload binding, but no public forwarding method for a checked broker message.
Its connection fields and lookup methods are also private.
The retained WAMN host facade cannot forward its checked operations through those resource types using the supported public Rust API.

WAMN validates the exact package and registration against the serving release before binding.
It associates each fetched broker message with a host-held dead-letter identity.
The host derives the dead-letter destination, message identifier, source stream, sequence, attempt count, headers, and body from that identity and message.
It awaits the publication acknowledgement and requires the expected dead-letter stream.
Native `term` does not perform that publication.
Guest-selected publication fields do not preserve this existing host boundary.

Native pull attachment checks stream grants, stored subject filters, and the pull-consumer shape.
It does not accept WAMN's release-registration identity or a host authorization callback for each attachment.
Its public consumer information also omits acknowledgement policy, which WAMN's exact drift check requires.
The plan already permits retained WAMN consumer creation and configuration checks.
Those retained checks do not expose the private native message to WAMN's dead-letter method.

The current WAMN bind method accepts the guest-supplied durable name.
The materializer derives that name, but the host does not compare it with a separate derived name.
Therefore, absence of a native consumer-name grant alone does not demonstrate a newly lost host guarantee.
The stop concerns exact registration validation and the host's association between the original message and its dead-letter identity.

## Stop and owner ruling

The plan's stop condition is:

> identify any release, metadata, acknowledgement or authority requirement the public interface cannot preserve. Keep the necessary WAMN portion and document why; do not widen grants or rewrite the event architecture to claim adoption.

The owner confirms that the private message-handle boundary triggers this condition.
WAMN retains the host checks and its existing broker implementation.
No second backend, private source copy, new grant, or native adoption claim is introduced.

The owner identifies a trusted platform adapter component as a real, separate option.
The capability registry can make that adapter the sole importer of `wasmcloud:nats`.
That restriction supplies the direct-import bypass boundary, but no such adapter or restriction is implemented here.
Moving platform policy into a component changes the trust model and requires its own decision and executed bypass proof.

Deferred decision `wamn-0ct2.6` owns that option.
Its reopening trigger requires a stable upstream host-component-plugin binding surface and a public native message handle.
The pinned Cargo manifest keeps `host-component-plugins` outside the default features.
Its module describes a supervised component store and proxied resource handles.
The future decision must align with that supported upstream shape rather than invent a temporary one.
Upstream availability alone does not authorize implementation.

## Executed evidence and limits

The [source receipt](checkpoint-002/source.json) records four successful Git commands and hashes for 15 source files.
Both recorded revisions match the expected pins, and the selected source files are clean.
The [capture tool](tools/capture-source.py) records its own hash and the exact command arguments, working directories, output, and exit codes.
The [first capture](checkpoint-001/result.json) fails because its upstream WIT path is wrong.
The corrected capture uses a new directory and completes successfully.

No Rust source, dependency, WIT, manifest, grant, or runtime feature changes.
No build, broker test, cluster journey, adapter probe, or benchmark runs.
No compiled artifact exists for this checkpoint.
The source review does not claim executed native delivery, duplicate-handler behavior, poison recovery, or bypass resistance.
No performance-sensitive substitution occurs, so no new performance comparison is claimed.
The existing P3 and 2.9 proof reports remain historical evidence for their own source revisions.

No production code is removed because C does not replace its predecessor.
The plan, ledger, and `[NATIVE-C]` recipe record the stop and the separate decision.
Fresh stores, B2's policy gate, the capability registry, and `.74` through `.76` remain unchanged.

## Source anchors

All upstream paths below refer to the pinned revision recorded above.

| Boundary | Source |
| --- | --- |
| Materializer registration bind, fetch, and router delivery | `components/execution/materializer/src/main.rs:536`, `:690`, `:716` |
| Existing application dispatch | `crates/execution/host/src/router_delivery.rs:193` |
| Exact registration and consumer configuration | `crates/platform/runtime/src/plugins/wamn_jetstream.rs:1375`, `:1436`, `:1452` |
| Original-message dead-letter construction | `crates/platform/runtime/src/plugins/wamn_jetstream.rs:1832` |
| Private native modules | `crates/wash-runtime/src/plugin/wasmcloud_nats/mod.rs:8` |
| Private connections and public service binding | `crates/wash-runtime/src/plugin/wasmcloud_nats/plugin.rs:43`, `:118`, `:747` |
| Closed native configuration keys | `crates/wash-runtime/src/plugin/wasmcloud_nats/keys.rs:80` |
| Native attachment checks | `crates/wash-runtime/src/plugin/wasmcloud_nats/interfaces/jetstream/mod.rs:329` |
| Native consumer and message interfaces | `wit/nats/wit/world.wit:219`, `:281` |
| Optional host component plugin feature | `crates/wash-runtime/Cargo.toml:35`, `:47` |
| Component policy execution and resource proxying | `crates/wash-runtime/src/plugin/component_host/mod.rs:1` |
