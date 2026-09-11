# Receiving cluster stopping point

Source: `77ebc72a8aedacd965ef258cba9970df22ccaf3d`.

The run failed at the first materializer NodePort mutation. The request to `http://172.28.0.7:31297/acme/purchase_order/update` returned `Connection refused (os error 111)`. [failure.json](failure.json) and [result.json](result.json) retain that result.

The [Cargo output](../receiving-cluster-cargo-001/cargo.log) records eight passing P3 cases and the completed CDC setup before the failure. [cdc-reader-ready.log](cdc-reader-ready.log) records the open replication session. [executor-lifecycle.json](executor-lifecycle.json) records passing SIGTERM and SIGINT cases before this stopping point.

The existing [flow HTTP response](flow-http-response.json) was the expected 404 through the earlier Job. It does not establish readiness of the later NodePort endpoint. The Receiving owner is restoring the existing exact 404 readiness request before the first NodePort mutation. This record does not establish a cause or a successful correction. Later application cases did not execute.

All original files remain byte-for-byte copies. Their copied filesystem modes are 0664, except [source.json](source.json) at 0600. Git stores only the executable mode bit.
