The [Receiving run](../../consolidation-step3/receiving-cluster-live-003/result.json) at `e440fb18` fails during materializer startup.
Its binary SHA-256 is `04ddf5788331f15833db31a8274f05ed5e41469783c8bd9ede3ab2401162132f`.
The retained guest file and recorded component hash match those bytes.

The first stack offset, `0x83714`, calls canonical `waitable-set.wait` through shim function 7.
The same binary exports synchronous `wasi:cli/run@0.2.0`.
Its first async wait is `block_on(events::open_pull_consumer(...))`, after registration preparation.

Wasmtime 47.0.4 forbids that wait in a synchronous task.
The relevant locations are `concurrent.rs:3568`, `1979`, and `5584`.
The pinned host enters its P2 service path at `workload.rs:781` and retries the trapped instance.
Those retries produce `cannot enter component instance` errors.
The original log omits the inner cause. The binary and runtime source establish the restriction.

The repair uses the pinned upstream async `wasi:cli/run@0.3.0` contract and its existing service entry pattern.
The source keeps `src/main.rs`, package and artifact names, every existing Rust body, all assertions, credentials, and workload declarations.

[The recorded results](result.json) cover WIT resolution, Rust syntax, unchanged original binary bytes, and exact upstream interface text.
The repaired guest build, actual export inspection, and live run remain pending.
