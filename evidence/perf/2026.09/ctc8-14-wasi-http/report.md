# Standard WASI HTTP probe: wamn-ctc8.14

The real P2 guest run passed with 17 case records and 604 exported spans.
The native gRPC path bypassed the probe request hook and its trace injection.
Production still refused standard WASI HTTP imports before the protocol cases ran.
This is a probe result, not evidence of an exploit through an admitted tenant capability.
The WAMN adapter remains necessary. Adoption belongs to `wamn-ctc8.16`.

## Source and build record

The source baseline is WAMN `7780a6313bc72a0bc7141b32da3de4cc5bc4ed93`.
The runtime pin is `735b57982545358409a7d965a22549b08487ca09`.
The standalone workspace owns its lockfile and target directory.
No production source, WIT, admission rule, or root lockfile changed.

The host pins Wasmtime, WASI, and WASI HTTP to the baseline lockfile's 47.0.4.
Its lockfile starts from the baseline lockfile and resolves probe additions offline.
`lock-audit-03.log` compares 455 packages with names present in the baseline.
It records two version exceptions: certificate-fixture `bit-vec` 0.9.1 through
`yasna` 0.6.0, and the explicit guest bindings `wasi` 0.13.3.
All shared native transport versions match, including Hyper 1.11.0 and AWS LC 1.18.0/0.44.0.
The audit also lists 15 probe additions, including the two probe packages.

The bindings source describes HTTP 0.2.2. The actual compiled guest imports
`wasi:http/outgoing-handler@0.2.9` and `wasi:http/types@0.2.9`.
The first record in `run-01.log` contains its complete import list.
`rustc-version.log` records Rust 1.98.0 and the compiler commit.

| Evidence | Exit | Result |
| --- | --- | --- |
| `guest-build-01.log` | 0 | Initial guest build, 3.46 seconds |
| `host-build-01.log` | 101 | Runtime references the disabled `oci` module with all features off |
| `host-build-02.log` | 0 | Baseline-seeded lock and root-equivalent `oci` feature, 177.68 seconds |
| `guest-build-02.log` | 0 | Guest confirmed against the corrected lock, 0.14 seconds |
| `run-01.log` | 0 | Real loopback protocol run, 3.02 seconds |

The failed build remains intact with `Cargo.lock.build-01` and `source-build-01.sha256`.
The successful source and binary hashes are in `source-build-02.sha256` and `binaries-run-01.sha256`.
`source-snapshot.sha256` records the final snapshot after blank-line cleanup in two manifests and `.gitignore`.
No Rust source changed after the successful run.
`run-01-summary.json` indexes the raw case records without replacing them.
`lock-audit-02.log` contains an audit-parser header error. The corrected audit is `lock-audit-03.log`.
Build times include dependency-cache effects and are not benchmark comparisons.

## Observed behavior

| Case group | Real observation | Limit |
| --- | --- | --- |
| Standard import | Unchanged `analyze_tenant` returns `UnadmittedImport` for the compiled HTTP imports | No tenant-policy expansion |
| Alias and destination | HTTP 200 at the recording peer, rewritten Host, HTTP/1.1 | The three alias rules are probe fixtures, not WAMN authority |
| Credential hiding | Peer sees the host sentinel, not the forged guest credential. Guest-owned Fields stay unchanged and host environment stays absent | The peer does not echo credentials. The consumed outgoing request is no longer guest-readable |
| TLS selection | An HTTP guest alias selects a working TLS destination. A trusted certificate with the wrong hostname fails before HTTP | No native DNS or connected-peer authorization callback is proven |
| Refusal | Unknown alias, escaped path, forged Authorization, excluded destination, FTP scheme, and wrong TLS hostname all fail before application traffic | Alias/path/header refusals explicitly return the WASI request-denied variant |
| Tracing | Wire trace matches an exported hook span. Native HTTP span is its child. Guest trace takes precedence. Span attributes contain no credential sentinel | No production WAMN effect-telemetry or frozen-outcome proof |
| Context | Separate parent and child component contexts expose the same workload ID to the native hook | No actual nested invocation, caller, release, or candidate-binding proof |
| gRPC-selected transport | H2 cleartext, H2 TLS, and `application/grpc+proto` all reach the real peer without entering `send_request` | Guest-forged Authorization reaches the peer, host credential is absent, and wire trace is absent |
| Unbound context | Guest traps and the known recording peer receives no request | Separate from the typed denial cases |

The gRPC rows test HTTP requests on the native gRPC-selected transport.
They do not test a protobuf service or its RPC schema.
The fixture does not retry guest calls. Each guest attempt has a ten-second deadline.
The case sequence has a two-minute deadline, and assertions fail the process.
The fixture uses only loopback peers and synthetic credentials.

## Gaps retained

Production tenant admission has no `wasi:http` capability row.
`crates/platform/component-policy/src/lib.rs::analyze_tenant` remains the controlling check.
The successful local fixture does not authorize a policy cutover.

The native public hook receives a workload ID, URI, body, and transport configuration.
`engine/ctx.rs::CtxHttpHooks` does not pass the executing component, original caller,
invocation, release, or frozen candidate-binding world.
Two stores with one workload ID do not prove nested identity preservation.

`host/http.rs::Ingress::outgoing_request` selects gRPC before `OutgoingHandler::send_request`.
The P3 sibling has the same source ordering, but this probe executes only P2.
The three real gRPC cases confirm the missing request transformation and wire trace injection.

The native connector does not expose WAMN's pinned `AuthorityDecision` as its input.
The loopback peer observations do not prove DNS pinning or destination authorization.
`ConnectionHttp::send`, release/candidate checks, and its error boundary remain production owners.

The standard WASI errors do not establish the frozen WAMN connection/outcome vocabulary.
The run distinguishes typed request denial from the unbound-context trap.
P3 guests, production nesting, candidate bindings, pooling, generation rotation,
aggregate quotas, and the shared blobstore path remain outside this P2 probe's proof.
The native pooling work belongs to `wamn-ctc8.13`. This probe blocks neither transport reuse nor session work.
