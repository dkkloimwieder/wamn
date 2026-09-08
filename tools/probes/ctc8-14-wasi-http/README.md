# ctc8.14: standard WASI HTTP hook probe

This standalone workspace tests the runtime pinned by WAMN commit
`7780a6313bc72a0bc7141b32da3de4cc5bc4ed93`. It changes no production imports,
admission policy, runtime adapter, WIT, or root dependency lockfile.
Its Wasmtime, WASI, and WASI HTTP host crates match the root lockfile at 47.0.4.

The P2 guest uses the `wasi` 0.13.3 bindings, whose source describes HTTP 0.2.2.
The actual compiled guest imports `wasi:http/outgoing-handler@0.2.9` and
`wasi:http/types@0.2.9`. The host records this compiled import list.
This disposable command component does not define a tenant admission profile.
The unchanged production policy refuses its HTTP imports.

Run these commands from this worktree only, after obtaining the shared build
slot. The local workspace owns its lockfile and target directory.
The wrapper reuses compiled dependencies without sharing a target directory.

```sh
export RUSTC_WRAPPER=sccache
export CARGO_TARGET_DIR="$PWD/tools/probes/ctc8-14-wasi-http/target"
cargo build --offline --locked --manifest-path tools/probes/ctc8-14-wasi-http/Cargo.toml -p ctc8-14-wasi-http-guest --target wasm32-wasip2
cargo build --offline --locked --manifest-path tools/probes/ctc8-14-wasi-http/Cargo.toml -p ctc8-14-wasi-http-probe
WAMN_CTC8_14_SECRET=ctc8-14-synthetic tools/probes/ctc8-14-wasi-http/target/debug/ctc8-14-wasi-http-probe tools/probes/ctc8-14-wasi-http/target/wasm32-wasip2/debug/ctc8-14-wasi-http-guest.wasm
```

The sentinel is synthetic. It tests that the process environment is not inherited
by the guest. Actual database or service credentials are neither needed nor used.
The run starts only loopback recording peers. It requires no cluster, database,
OCI publication, or outbound internet access. Each guest call has a ten-second
deadline; the case sequence has a two-minute deadline. A failed assertion exits
nonzero. The final JSON record says `probe-complete`, never that adoption is safe.

`ProbeHook` is deliberately a fixture adapter. Its three URI aliases, path rule,
credential sentinel, and trace injection demonstrate what the public request
hook can transform. They are not copies of WAMN binding authorization, and they
do not prove invocation, release, candidate, tenant, or credential-generation
authority. Server observations establish the actual loopback peer reached;
they do not prove native DNS pinning or a connector policy callback exists.

The gRPC cases are negative controls for adopting this adapter. They send real
H2 requests, including TLS, through the runtime's guest-selected fast path. The
server records whether that path bypasses the hook's credential rejection and
trace injection. Successful transmission in these cases is evidence of the gap.
These cases do not test a protobuf service or its RPC schema.
The two-component case uses separate stores with the same workload identity;
it does not impersonate the production nested invocation driver.

Raw commands, exit codes, observations, and retained gaps belong in
`docs/perf/2026.09/ctc8-14-wasi-http/`. Pooling, rotation, aggregate quotas, and
the blobstore path belong to `wamn-ctc8.13`; P3 guest adoption remains separate.
