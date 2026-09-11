The [Receiving live run](../../consolidation-step3/receiving-cluster-live-003/result.json) at `e440fb18` failed with materializer binary SHA256 `04ddf5788331f15833db31a8274f05ed5e41469783c8bd9ede3ab2401162132f`, which still matches the retained guest file and the run’s recorded component hash.

The first stack offset, `0x83714`, calls canonical `waitable-set.wait` through shim function 7, while the same binary exports synchronous `wasi:cli/run@0.2.0`; the materializer’s first async wait is `block_on(events::open_pull_consumer(...))` after registration preparation.

Wasmtime 47.0.4 forbids that wait in a synchronous task (`concurrent.rs:3568`, `1979`, `5584`), and the pinned host’s P2 service path (`workload.rs:781`) then retries the trapped instance, which accounts for the subsequent `cannot enter component instance` errors; the original host log omits the inner cause, so the canonical restriction is established from the binary and runtime source.

The repair uses the pinned upstream `wasi:cli/run@0.3.0` async contract and its existing cdylib service entry pattern, retaining `src/main.rs`, package and artifact names, every existing Rust body, all test assertions, credentials, and workload declarations.

[Recorded checks](result.json) confirm WIT resolution, Rust syntax, unchanged original binary bytes, and exact upstream run-interface text; the repaired guest build, actual export inspection, and live validation remain pending.
