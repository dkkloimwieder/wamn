# Receiving cluster test failure

Source: `77ebc72a8aedacd965ef258cba9970df22ccaf3d`.

The selected Receiving cluster test compiled and ran. It failed with a refused connection to `http://172.28.0.7:31297/acme/purchase_order/update`. Cargo returned 101 after 2,108.072 seconds. The test result was 0 passed, 1 failed, and 0 ignored.

[record.json](record.json) records the command and environment. [cargo.log](cargo.log) retains the full output, including the eight passing P3 cases before CDC setup. The [cluster result](../receiving-cluster-live-001/README.md) records the stopping point. No later application case passed in this run.
