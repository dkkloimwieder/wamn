The WMS browser demonstration test passed at `04b72bd0d1cf87a170e44e65266f69208f045978`.
The test took 397.74 seconds, and Cargo with its prerequisites took 493.863 seconds.
These are elapsed test times, not application performance measurements.
The [result](result.json) records unchanged source, resource cleanup, and private-file removal.
The sibling [Cargo record](../wms-browser-cargo-001/record.json) gives the exact command and its successful exit.

The [proxy result](demo.json) records HTTP 404 from `http://127.0.0.1:8080/` with route host `wms.localhost`.
The existing test accepts an HTTP response and then holds the environment for the requested one second.
This run demonstrates proxy reachability.
It does not exercise browser interaction or assert successful page rendering.
The raw results and logs retain their original bytes.
