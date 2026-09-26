Edge device owns package `edge_device`: the application that an edge box runs on each frame of its serial device ([edge plan](../../docs/plan/edge.md), section 4.2).
It declares no SQL: no model, no relation, no connection and no migration, so its component imports no `wamn:postgres`.
Its one command `sample.read` is stateless: it takes `{"request_id", "value": {"frame", "captured_at"}}`, trims the frame, and refuses a blank frame on `value.frame`.
The edge box stores the answer as the sample and forwards it to [`edge_samples`](../edge_samples/README.md).
`wamn dev edge-bundle` writes the edge release bundle from a developer session of this package.

[Manifest](wamn.json): Package identity and the declared command.
[Generated output](generated/): Derived contracts and client code.
[Component](component/): Application guest.
[Publication](publication/): The session route `/sample/read`.
[Running tests](../../docs/operations/running-tests.md): The edge tests, which load this bundle.
