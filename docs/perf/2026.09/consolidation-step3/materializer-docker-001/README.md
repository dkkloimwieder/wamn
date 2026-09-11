The Docker component build passed at `e389c6fd9d8e04b7a563fecd6488b3eb544cb99f` after 40.979 seconds.
The [build result](build-result.json) includes the source fix `73d258fc9baa3cf4fea9f978ab02fe2038096f83`.
[The build log](build.stderr) records Rust 1.98.0 for materializer and Rust 1.97.0 for the other four guests.
Each guest uses one Cargo invocation and the `wasm32-wasip2` target.

[Hash capture](artifact-hashes-result.json) passed after 0.386 seconds, using a temporary container with networking disabled.
[These exact hashes](artifact-hashes.stdout) identify the five files under `/component-output/`.

| Component file | SHA-256 |
| --- | --- |
| `http_route.wasm` | `c5e17c9e49dafd83b38881b0f75b5cc44a0e54fb7c81e0bdb20c35609ba3132a` |
| `materializer.wasm` | `244386d31a2c42ef42909add6b98b7ef24fd6e193084df3d131943a71bd10e31` |
| `busyloop.wasm` | `6b0917036b68cb34b12d11eab1550f29b5eb081a6449daa2dc34eee5d1285340` |
| `connection_http_standard.wasm` | `6dc76cd2d6ec62bedeb3d4fa50ba0df01cf0995c3964fbfe0123ae3caa39927f` |
| `sockprobe.wasm` | `45a85d4b19ac7fe7403953650ad7a9d98f81ab3e260ecb8f7acbef6129cbfffc` |

[Image cleanup](image-cleanup-result.json) exited 0 and [removed the owned image](image-cleanup.stdout).
The hash container used `--rm`.
[All command results](results.json) retain the actual arguments, exit codes, and durations.

The [standalone materializer build](../../native-c-materializer-async/cli-001/after.json) used another build path and recorded SHA-256 `983efac480a63b61ce5f0c44b7a8a100af32ba1e148e18d8ca03b4d2e5b94c2f`.
The Docker hash differs, so these results establish no byte equality between those build paths.
This test covers the Docker component build and artifact capture, with no cluster or message delivery execution.
