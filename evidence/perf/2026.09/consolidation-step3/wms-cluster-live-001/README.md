# WMS released routes on a private cluster

Source: `04b72bd0d1cf87a170e44e65266f69208f045978`.

The Rust test `cluster::released_wms_routes` passed on a fresh, uniquely named kind cluster.
It provisioned and published the declared WMS release through the existing platform libraries.
It exercised the released routes, contention, replay, label storage, environment isolation, and the idle materializer.

The test removed its cluster, containers, image, and private files.
The tested source stayed unchanged.
`result.json` records those assertions.
The sibling `wms-cluster-cargo-001` directory records the actual command, output, and successful exit after 641.019 seconds.

This run does not execute the generated terminal, store-failure, startup, or browser demonstration cases.
