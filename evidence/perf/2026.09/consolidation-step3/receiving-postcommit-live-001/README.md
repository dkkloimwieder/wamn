The postcommit pair failed during baseline image preparation at source `25bece68bfe93b3948bc2a72903ff1c3c2af806f`.
The [failure record](failure.json) reports a command failure, and the [recovered image logs](../receiving-postcommit-image-001/README.md) identify the materializer link failure.
The [source order](source-order.json) places cluster creation after the failed image build, so neither installation reached application assertions and the additive installation did not start.
At [21:20:59.081130 UTC](../receiving-postcommit-image-001/post-run-observation.json), a separate read-only observation found no owned containers, cluster, host or gates image tag, or private directory.
That observation does not turn this failed test into a passing run or supply an automatic cleanup result.
