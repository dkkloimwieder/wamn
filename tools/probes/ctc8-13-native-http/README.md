# Native HTTP probe

This disposable crate calls the public HTTP transport hooks at runtime revision `68ebece9c537f8bb4b5c9999f274ec68d60f35a9`.
It does not replace a production transport or execute guest WIT.
The cutover source base is `dfa1c3187fe8cd671688a442b23106046e502cb6`.
The probe pins the Wasmtime family to `47.0.4`.
The root requirement and both lockfiles use that version.

The probe owns recording servers on ephemeral loopback ports.
Its TLS certificates and credential markers are synthetic.
The servers record each request and the connection that received it.
The client records the connected peer from native response metadata.
The process aborts its server tasks when it exits.

The protocol matrix covers eight paths: P2/P3, HTTP/HTTPS, and ordinary HTTP/1.1/gRPC HTTP/2.
The gRPC rows test the native `application/grpc+proto` transport branch, not a protobuf service contract.
Each path sends sixteen requests across two native workload keys, followed by an allowed-host refusal.
TLS paths also refuse an untrusted certificate.
The other experiments cover cleartext HTTP/1.1 draining, generation keys, byte thresholds, timeouts, and response loss.
The HTTP/2 concurrency experiment uses cleartext transport.
These scopes do not establish the same lifecycle results for every protocol combination.

The peer experiment calls the real WAMN authority resolver with a controlled DNS answer.
It then passes the logical URL to the public native transport.
A difference between the approved peer and the connected peer records a connector gap.
Detection after dispatch does not prevent an unauthorized effect.

The `gap` verdict means that the fixture observed a missing guarantee.
The `source-gap` verdict names an API limitation from source inspection.
The `not-tested` verdict names work that this probe did not execute.
The final `observed` result confirms that all listed experiments ran, not that native adoption is safe.
A failed assertion or missing final result fails the run.

Root coordinates the single build slot.
Do not build this crate while another lane owns that slot.
When the slot is granted, run these commands from the worktree root:

```bash
RUSTC_WRAPPER=sccache cargo build --manifest-path tools/probes/ctc8-13-native-http/Cargo.toml --locked --offline
```

Use the committed probe lockfile.
Do not regenerate it from manifest ranges.
The initial fresh resolution selected newer transport dependencies than the source-base lockfile.
The corrected lock starts with the source-base lockfile and adds only the fixture dependencies.
Audit package names, versions, sources, and checksums before interpreting a new result:

```bash
awk -f tools/probes/ctc8-13-native-http/audit-locks.awk \
  Cargo.lock tools/probes/ctc8-13-native-http/Cargo.lock
```

Run the wrapper with the built executable and a new evidence directory:

```bash
mkdir -p evidence/native-http
bash tools/probes/ctc8-13-native-http/run \
  tools/probes/ctc8-13-native-http/target/debug/ctc8-13-native-http \
  evidence/native-http/local-run
```

The wrapper writes stdout, stderr, the exit code, and the source and binary hashes to the selected directory.
It rejects missing or repeated experiment results.
Report pass, fail, or skip with the command, source, and observed gaps.
The diagnostic files do not require a permanent archive.
The executable emits one JSON object per line and limits the whole fixture to ninety seconds.

The probe does not exercise the private `ConnectionHttp::send` replacement, frozen candidate bindings, blobstore authorization, or nested caller propagation.
It records native transport errors; it does not establish their translation into WAMN's outcome vocabulary.
Root owns the existing regression tests and the benchmark integration.
Neither this program nor its elapsed request times establish a production performance improvement.
No schema, grants, production runtime, guard, shared manifest, or fork change belongs to this experiment.
