The [command record](record.json) reports exit 101 after 87.4118751839851 seconds at source `965cd41835bd6e28b3f24b080392dac2dc53213e`.
Cargo completed compilation, then the test stopped before creating a cluster.
The launcher pre-created `receiving-postcommit-live-002`, which violated the test's requirement for a new result directory.

[The test output](cargo.log) reports exactly: `Receiving evidence must use a new absolute directory`.
It records zero passing tests, one failed test, and 0.00 seconds of test execution.
Both installations and all application assertions remained unexecuted.
This launch input error does not establish a repository regression.

[The controller log](../receiving-postcommit-controller-002.log) retains the same completed command result.
The empty live result directory contains no files to publish.
