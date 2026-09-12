At source `25bece68bfe93b3948bc2a72903ff1c3c2af806f`, the [postcommit pair command](command.json) exited 101 after [600.305604677 seconds](record.json).
The [test output](cargo.log#L282) reports one failed test, zero passed tests, and 517.61 seconds of test execution.
The failure occurred while building the baseline installation's gates image, before cluster creation or application assertions, as recorded in the [recovered Docker logs](../receiving-postcommit-image-001/README.md).
The [failed run](../receiving-postcommit-live-001/README.md) remains separate from the later resource observation.
