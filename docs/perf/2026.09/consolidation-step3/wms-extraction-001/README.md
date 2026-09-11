The WMS tests below ran at `04b72bd0d1cf87a170e44e65266f69208f045978` after their extraction into the application test crate.
The [default route test](../wms-cluster-live-001/README.md) passed, including contention, replay, label storage, environment isolation, and the idle materializer.
The [terminal test](../wms-terminal-live-001/README.md) passed both successful and partial-completion requests.
The [browser demonstration](../wms-browser-live-001/README.md) passed its proxy-response and cleanup assertions without browser interaction.

The [startup test](../wms-startup-live-001/README.md) failed because its steady request overhead ratio was 23.809243876261803 against the retained limit of 12.
Cold and restarted requests completed before that failure.
The final cache comparison did not run.
The cause of the ratio is not established.
No benchmark code or threshold changed as part of this result publication.

All four [results](result.json) record unchanged source, resource cleanup, and private-file removal.
This note preserves the startup failure and does not claim that all WMS tests passed.
The [copy comparison](copied-files.json) records SHA256 hashes and modes for the 57 copied browser files.
