# Native HTTP probe: wamn-ctc8.13

The final [run receipt](run-002/run-receipt.json) records exit zero and all 27 expected cases.
Native connection reuse worked across all eight transport combinations.
The experiment also exposed missing guarantees, so production adoption remains unproved.
The crate calls the pinned runtime's public `HostHandler` methods through its real `Ingress` and `DefaultOutgoingHandler`.
Its servers use ephemeral loopback ports, synthetic credentials, and fixture TLS certificates.
The crate does not execute guest WIT or replace WAMN's private HTTP transport.

## Provenance

The production source base is `7780a6313bc72a0bc7141b32da3de4cc5bc4ed93`.
The runtime fork is `735b57982545358409a7d965a22549b08487ca09`.
The toolchain is `rustc 1.98.0 (88d9e12ae 2026-08-18)` on `x86_64-unknown-linux-gnu`.
Builds use the probe's own debug target directory and `RUSTC_WRAPPER=sccache`.

`build-001` passed compilation but selected Wasmtime `47.0.3` from the manifest floor.
The production lock resolves the family to `47.0.4`.
No experiment ran against that first build.
Its manifest, lock, command, and logs remain in the evidence directory.
Its `build.pid` belongs to an initial detached launch that did not survive.
The captured build then ran through the persistent command session and exited zero.

`build-002` corrected Wasmtime to `47.0.4` but used a fresh dependency resolution.
Its audit found 507 identical package identities, twelve differences under shared names, and sixteen new names.
The differences include Hyper, AWS-LC, and Tokio's socket dependency `mio`.
`run-001` completed all 27 receipts with that exploratory lock.
It is not evidence for a production-lock-matched transport or a performance comparison.
The evidence preserves its lock and wrapper snapshots.

The final probe lock starts with the unchanged production lock.
An offline workspace update removes unused packages and adds fixture dependencies.
The [final audit](build-003/shared-lock-audit.log) records 518 identical package identities and sixteen new names.
Identity here means package name, version, source, and checksum.
The only shared-name difference is `bit-vec 0.9.1`, an optional dependency of fixture library `yasna 0.6.0` through `rcgen 0.14.8`.
The [reverse dependency command](build-003/fixture-bit-vec.log) reports no active target dependency on `bit-vec`.
The audit does not claim that the standalone executable has the production binary's complete feature graph.

| Native dependency | Version in production and the final probe lock |
|---|---|
| Wasmtime, WASI, WASI HTTP | `47.0.4` |
| Hyper / Hyper-util / Hyper-rustls | `1.11.0` / `0.1.20` / `0.27.9` |
| Tokio / Mio / H2 | `1.53.1` / `1.2.2` / `0.4.19` |
| Rustls / AWS-LC Rust / AWS-LC native | `0.23.43` / `1.18.0` / `0.44.0` |

`build-003` passed the locked offline debug build in 3 minutes, 22 seconds.
Its [command record](build-003/commands.log), [build log](build-003/build.log), and [exit code](build-003/build.exit-code) are retained.
The final binary SHA-256 is `fb0b9f87f37a8ab3e82ba8ec3b3539e7e2b37387fe24a486c11ede1b0818c650`.

## Recorded execution

The [raw receipts](run-002/stdout.jsonl) contain server observations and client outcomes for every case below.
The final run emitted 27 JSON records and no stderr.
The executable's outer 90-second deadline bounds the fixture, not the production transport.

| Case and executed scope | Final result |
|---|---|
| P2/P3 × HTTP/HTTPS × ordinary H1/gRPC H2 | Each row sent 16 requests on two connections, one per native key. |
| Eight allowed-host and four TLS refusals | No refused HTTP request reached a recording server. |
| Stable-key explicit unbind, P2/P3 cleartext H1 | Old work retained the sole quota slot, completed, and closed before the next connection served new work. |
| Generation keys, P2/P3 cleartext H1 | Two keys allowed two live connections for one logical scope despite the per-key limit of one. |
| Concurrent requests, P2/P3 cleartext gRPC H2 | Four held requests shared one connection despite the connection limit of one. |
| Byte cases, P2/P3 cleartext H1 | 128 KiB request and response bodies and 4 KiB header padding passed the diagnostic threshold. |
| Deadlines, P2/P3 cleartext H1 | Header waits timed out. Streams lasted 206/205 ms despite 100 ms phase timeouts. |
| Response loss, P2/P3 cleartext H1 | Each recorded mutation analogue dispatched once and lost its response. |
| Approved-peer mismatch, P2 cleartext H1 | Approved `127.0.0.2:43935`, but native transport dispatched to `127.0.0.1:43935`. |

The wrapper requires every expected case exactly once and requires the final completion receipt.
Its [contract test](run-002/receipt-contract.log) accepts a complete run and rejects four damaged variants.
Those variants remove a case, duplicate a case, remove evidence, or declare failure.
Source hashes, the binary hash, Rust formatting, shell syntax, and whitespace checks also passed.
No production regression suite or standalone Rust unit-test suite ran in this lane.

## Interpretation of the transport cases

The protocol cases cover P2 and P3, HTTP and HTTPS, and ordinary HTTP/1.1 and gRPC HTTP/2.
The gRPC cases select the native `application/grpc+proto` branch.
They do not implement or test a protobuf service contract.
Each case sends sixteen requests across two host-selected workload keys.
Connected-peer metadata and the recording server's connection identities provide transport evidence.
These keys are fixture inputs, not a proven mapping from WAMN authority.

An allowed-host refusal follows each reuse case.
It must dispatch no additional request, even when a matching pooled connection exists.
Every TLS case also refuses the fixture certificate when the client uses unrelated default trust.
These controls do not establish WAMN candidate, caller, or credential authorization.

The fixture keys do not establish tenant, connection-instance, approved-peer, or TLS-policy boundaries.
WAMN's mapping of those boundaries to reusable native clients remains unproved.
Changing TLS policy or the approved peer during an active generation is outside the executed cases.

The draining case uses cleartext HTTP/1.1 for P2 and P3.
It explicitly calls `on_workload_unbind` while keeping the quota key unchanged.
The old request must retain its quota slot until completion.
The next successful request must use a different connection.
This case does not prove automatic credential rotation or lifecycle behavior for HTTPS and gRPC.

## Limits of the authority proof

The peer-mismatch case uses P2 cleartext HTTP/1.1 only.
WAMN's real resolver approves a controlled `127.0.0.2` destination.
The public native transport receives the logical URL and independently resolves `localhost` to the recording server at `127.0.0.1`.
The request reaches the server before response metadata reports the mismatch.
Observation after dispatch does not enforce the approved destination.

The fork constructs its connector privately in `crates/wash-runtime/src/host/http_client.rs:600` and `:614`.
Its public pool interface does not accept WAMN's `PinnedEndpoint`.
WAMN's existing transport consumes that decision through `ClientBuilder::resolve` in `crates/platform/runtime/src/plugins/connection_http.rs:695`.
This probe does not patch either transport or supply a replacement connector.

Frozen candidate bindings, the shared blobstore rule, and nested identity remain unproved under native substitution for every protocol.
WAMN's send path is private at `crates/platform/runtime/src/plugins/connection_http.rs:221`.
The release and candidate checks are crate-private at `:417` and `:460`.
The probe does not copy these checks and call the copy a production proof.
Existing regression runs belong to the root task and do not prove a replacement that this crate does not implement.

## Resource and outcome boundaries

The generation-key case uses two keys for one logical scope, with one connection allowed per key.
The keys receive different quotas because the fork uses the same workload identity for its pool and quota lookup.
The source is `crates/wash-runtime/src/host/http_client.rs:901`.
Changing only a credential header also leaves the pool key unchanged.
Native workload separation is not, by itself, WAMN credential-generation separation.

The concurrent-request case uses cleartext gRPC HTTP/2 for P2 and P3.
It places four held requests on one connection with a connection limit of one.
The native connection limit does not supply a request limit.
The client cache also lacks a retained-client count cap at `crates/wash-runtime/src/host/http_client.rs:863`.
That count-cap statement comes from source inspection, not a stress measurement.

The byte cases use cleartext HTTP/1.1 for P2 and P3.
Their 1 KiB diagnostic threshold is not an owner-selected product limit.
The fixture sends 128 KiB request bodies and 4 KiB request headers, then receives oversized bodies or headers.
The public native configuration does not provide the requested aggregate byte budget.
This result does not mean that the HTTP parser has no internal limits.

The timeout and response-loss cases use cleartext HTTP/1.1 for P2 and P3.
The fixture records each request before delaying or truncating its response.
One delayed request exceeds the first-byte deadline, while a streaming response exceeds the phase timeout in total.
The P2 direct-body path omits the guest `HostIncomingBody` wrapper.
These cases do not prove guest timeout behavior or a total request deadline across every protocol.

The response-loss case counts upstream dispatches after an already-recorded mutation analogue.
One dispatch does not establish a no-retry guarantee for every reset race.
Hyper-util `0.1.20` enables `retry_canceled_requests` by default at `src/client/legacy/client.rs:1034`.
Its documented scope is requests disrupted before writing starts at `:1541`.
The native pool does not expose that builder setting.
This source fact is not evidence that the runtime repeated a committed mutation.

The probe records native errors without translating them into WAMN's outcome vocabulary.
That translation remains unproved for native adoption.
Timeouts do not undo an upstream effect.
No result in this report establishes a production performance improvement.

## File boundary

The probe owns only `tools/probes/ctc8-13-native-http/` and this evidence directory.
It changes no production code, shared lockfile, guard, WIT, schema, grant, or runtime fork.
The separate WASI probe owns guest-interface evidence.
Transport adoption remains the separate `wamn-ctc8.16` decision and implementation.
