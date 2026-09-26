Edge samples owns package `edge_samples`: the platform application that receives the samples an edge box forwards ([edge plan](../../docs/plan/edge.md), section 4.8).
Its command `sample.record` takes one item per sample, `{"request_id", "value": {"idempotency_key", "frame", "captured_at"}}`, and answers the `sample_id` it recorded.
The edge sample key is the idempotency key, so a repeated forward answers the first `sample_id` and records no second row.

[Manifest](wamn.json): Package identity and declared operations.
[Migrations](migrations/): Authored application schema.
[Command SQL](command/): The claim, replay and record statements of `sample.record`.
[Data access](data/): SQL-backed operation implementations.
[Generated output](generated/): Derived contracts, SQL, and client code.
[Component](component/): Application guest.
[Publication](publication/): The two routes, `/sample/get` and `/sample/record`.
[Running tests](../../docs/operations/running-tests.md): The edge forward run against this application.
