# Identity and HTTP integration

`wamn-ctc8.22` combines the lane work at clean source `6f130c7259e8365035d059fc4444e44c747b1216`. The [captured source](integration-001/source-head) records that commit. The inputs are fixed to these commits.

| Input | Commit |
|---|---|
| Base main | `e9b315ac32bf228df465f8b172b408d2b97726ef` |
| Fresh authentication | `da57649bfc3bd702562e8c36551a1f9522bfdfc4` |
| Native HTTP probe | `98899909bde8ee7733798812e39a420b706ea3e8` |
| WASI HTTP probe | `802785f4918d081bcd801bb6f4eb5f7aad5f3c09` |

Two add/add conflicts involved partial evidence logs. Main's committed bytes are exact prefixes of the completed logs. Each resolved file matches its completed log.

All 3,341 fresh-auth [checksums](../ctc8-12-fresh-auth/SHA256SUMS) pass. The 62 files owned by the native probe and 25 owned by the WASI probe match their source commits. Authentication and runtime code, schemas, dependencies, guests, and the route fixture match the measured candidate, `a9e23b54`.

This integration adds no implementation beyond the identified lanes. It adopts neither HTTP probe and adds no session code.

## Integration tests

The [runner](run.sh) finished at 13:05:05 UTC on September 8, 2026. It ran each command separately, without inherited live-service credentials. The [command record](integration-001/commands.log) preserves the exact arguments.

| Test group | Reported passes | Failures | Exit |
|---|---:|---:|---:|
| [Workspace, all targets, ignored tests included](integration-001/workspace.log) | 1,847 | 54 | 101 |
| [Doc tests](integration-001/doctests.log) | 6 | 0 | 0 |
| [Existing contract runner](integration-001/contracts.log) | 33 | 0 | 0 |
| [Focused authentication and connection tests](integration-001/focused.log) | 46 | 0 | 0 |

Of the 54 workspace failures, 52 report missing live inputs or artifacts. The chart test cannot reach its Kubernetes endpoint. The remaining assertion matches open `wamn-362o.58`: the WMS overlay mounts an undeclared object-store Secret. Its test and deployment inputs are unchanged from base main. No other code assertion failed.

The workspace reports 160 test summaries. Some passing tests explicitly skip their live bodies when inputs are absent. The reported pass total is not a count of executed live proofs. The wrapper preserves the workspace failure with [exit 1](integration-001/exit).

The focused run uses the [unchanged capture script](quality.sh), with one fresh PostgreSQL 18 server per stateful suite. Identity, PAT, route authentication, nested permissions, HTTP connections, and blobstore candidates pass. The capture records [exit 0](quality-001/exit) and [container cleanup](quality-001/cleanup.log).

No benchmark or deployed-cluster rerun occurred during integration. The [existing deployed membership proof](../ctc8-12-fresh-auth/membership-001/journey/membershipproof.receipt) remains attributed to `a9e23b54`, not the integration commit.
