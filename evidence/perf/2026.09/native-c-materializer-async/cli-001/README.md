# Async materializer build result — 2026-09-11

At `5e7b68cc3d35bf14c3b8da5828246033ed11da71`, the [guest build](build-result.json) exited 0 after 1.435 seconds and the [unit test run](tests-result.json) passed all ten tests after 21.659 seconds.
The [after record](after.json) reports clean tracked source and these component bytes.

| Component | Bytes | SHA-256 |
| --- | ---: | --- |
| [Before](before.json) | 551,480 | `04ddf5788331f15833db31a8274f05ed5e41469783c8bd9ede3ab2401162132f` |
| [After](after.json) | 552,863 | `983efac480a63b61ce5f0c44b7a8a100af32ba1e148e18d8ca03b4d2e5b94c2f` |

All four inspection commands exited 0, with [WIT](after-wit.stdout) exporting `wasi:cli/run@0.3.0` as `run: async func() -> result` and [WAT](after-print.stdout.gz) line 209469 showing its async canonical lift and callback.
The world imports remain unchanged, including the named native `events` binding and WAMN interfaces, and all original capture bytes remain intact.
These build, unit, and inspection results do not execute native materializer delivery, so the full Receiving rerun remains required after the [retained failed run](../../consolidation-step3/receiving-cluster-live-003/README.md).

The [gzip record](after-print-gzip.json) records both file hashes and confirms exact decompression. From this directory, run `gzip -dc after-print.stdout.gz` to read the complete WAT.
