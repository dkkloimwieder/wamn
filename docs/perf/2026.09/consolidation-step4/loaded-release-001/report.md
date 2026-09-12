All 80 executed tests passed. Three live cases stayed ignored. All affected native callers compiled.

Source `b6fbb9a4e23ce73118d9de1b770304af041ce231` names the runtime object `LoadedRelease`. It changes 28 native Rust files under `wamn-47wm.5.5`.

Constructors, error variants, refusal text, manifest bytes, and external identities remain unchanged. The [source comparison](source-comparison.json) records the permitted name and comment changes. The [test mapping](test-name-map.json) records 13 renamed test identities.

Each command used Rust 1.98.0, debug mode, two jobs, and the existing isolated target. Cargo used `--locked --offline`. Live inputs stayed unset.

| Command record | Passed | Ignored | Elapsed seconds |
| --- | ---: | ---: | ---: |
| [release-manifest](release-manifest-result.json) | 7 | 0 | 33.979216 |
| [expected-router](expected-router-result.json) | 4 | 0 | 0.566398 |
| [flow-http](flow-http-result.json) | 21 | 0 | 0.546136 |
| [manifest-source](manifest-source-result.json) | 8 | 1 | 20.922880 |
| [session-route](session-route-result.json) | 0 | 0 | 0.676562 |
| [router-delivery](router-delivery-result.json) | 12 | 0 | 153.167918 |
| [native-policy](native-policy-result.json) | 7 | 1 | 0.902850 |
| [runtime-conformance](runtime-conformance-result.json) | 21 | 0 | 36.015139 |
| [native-callers](native-callers-result.json) | compile only |  | 249.707285 |
| [host-caller](host-caller-result.json) | compile only |  | 227.730498 |
| [session-route-feature](session-route-feature-result.json) | 0 | 1 | 25.393307 |

The first session command selected zero tests because `test-util` was unset. The second command compiled the test body with that feature. Its PostgreSQL case stayed ignored.

The other ignored cases require an authenticated registry and fresh PostgreSQL authority inputs. The [summary](summary.json) preserves their exact names and reasons. These runs establish no live service result.

Both captures kept the same source hash and all 31,329 tracked paths outside `.beads`. The captured bytes and modes match before and after. The [first comparison](source-stability.json) and [second comparison](session-feature-stability.json) contain the results.

Raw stdout and stderr remain unchanged. Each command result records their hashes and modes. The [artifact record](compiled-artifacts.json) hashes the four compiled caller executables.

The compressed source maps contain supporting comparison data. They use lossless gzip. The [archive record](archives.json) preserves each original size and hash.
