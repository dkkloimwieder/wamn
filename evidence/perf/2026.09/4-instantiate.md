# 4 — what a fresh instance costs, and what the artifact costs

Two questions the owner refused to let me answer by assertion: is the served
guest actually thin, and is `instantiate` 1.30 ms because of the fresh-store rule
or because of the artifact.

## The served guest was a debug build

`tools/build-components` ran plain `cargo build` and read
`target/wasm32-wasip2/debug`. `components/Cargo.toml`'s `[profile.release]` —
`opt-level = "s"`, `strip = true` — was dead code on the path that ships. The
Dockerfile's component stage has always used `--release`; only this path did not.

| virtualized, as served | debug | release | |
|---|---:|---:|---:|
| `receiving.wasm` | 20,634,368 | **669,593** | **30.8×** |
| `client_acme_receiving.wasm` | 10,019,001 | 466,014 | 21.5× |
| `blob_put.wasm` | 6,812,184 | 325,357 | 20.9× |

**In-cluster, the OCI pull went 1,353 ms → 112 ms.** That is the clean win, and
it is the one this change was worth.

## It does not touch instantiate

`instantiate_async` timed directly against the production engine, component
compiled and `InstancePre` built, non-WASI imports stubbed as traps — the exact
state the router is in when it calls:

| artifact | MiB | instantiate mean µs |
|---|---:|---:|
| label_render | 2.94 | 76.3 |
| http_request | 3.78 | 159.9 |
| http_route | 8.01 | 426.8 |
| client_acme_receiving | 9.42 | 324.6 |
| materializer | 14.51 | 402.2 |
| **receiving (debug)** | **19.55** | **345.3** |
| **receiving (release)** | **0.51** | **350.3** |

**Not size-driven.** An 8 MiB guest costs more than a 19.55 MiB one, and the same
component at 38× smaller instantiates identically. Import and instance count
drives it, not bytes.

**And it is 313–350 µs, not 1.30 ms.** The in-cluster span is ~4× the same call
on bare metal. Most of that millisecond is environment, not the fresh-store rule.
The rule's real price is ~300 µs.

## Strip removes symbols, not imports

`wasm-tools component wit` over the virtualized artifacts: `receiving` 6 imports,
`client_acme_receiving` 7, `blob_put` 7 — every set byte-identical between debug
and release. Recorded in `4-instantiate/release-vs-debug.md`.

## Two environment findings this produced

**Cranelift compiles serially.** The root `Cargo.toml` pins
`wasmtime = { default-features = false, features = ["cache"] }`;
`parallel-compilation` is a wasmtime default, and `rayon` — the dependency it
gates — is absent from the graph entirely. Debug compile was 24,383 ms on an
8-core box against 27,250 ms under a 2-core pod cap, a ratio of 1.12: core count
barely matters because one core does the work. Filed as `wamn-0h0g.17.22`.

**The host pod is capped at 2 CPUs and guaranteed 0.25**, on an 8-core box. 8→2
is the 4× that separates every bare-metal number in this report from its
in-cluster twin. Filed as `wamn-0h0g.17.23`.

## Not a result

The steady overhead ratio read **5.348** on this run against 8.659 on the
previous one, and that is **not an improvement**. The denominator grew: `sql_ms`
inflated 0.556 → 2.948 ms under a launch load of 3.70. The ratio is only
load-independent when numerator and denominator move together, and a five-fold
database stall breaks that. The pull number is the honest one.

## Raw data

`4-instantiate/` holds the sweep and the import comparison;
`4a-release-profile/` holds the journey that passed end to end with release
guests. `tests/integration/src/bin/instantiate-bench.rs` regenerates the sweep.
