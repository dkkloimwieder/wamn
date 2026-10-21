# REST consumer fixture

This application owns `rest_consumer_fixture.item`, with the upstream key and item name.
Its registered `item.record` command requires the `rest-consumer-fixture:item/record` permission.
The integration principal is the application's service identity.
An administrator grants it that permission through the existing role administration.

The command accepts an array of envelopes.
Each envelope contains `request_id`, `value.idempotency_key`, and `value.items`.
Each item has a nonempty text `key` and `name`.
The command inserts the items in one transaction and ignores keys that already exist.
An empty item array succeeds and returns an empty `keys` array.
The result retains `request_id`, which lets the integration carry its schedule due time without storing temporary state.

Publish this package beside `rest_connector` and its `wamn_integration` base in one release.
The integration calls this command through its pinned workflow dependency.
The integration's event registration reads this package's item inserts.
Change data capture (CDC) includes this schema alone.

Build with `target/debug/wamn build apps/rest_consumer_fixture` after you set the [build database URL](../../docs/operations/building.md#wamn-build).
Run its publication test with `cargo test -p wamn-control --test rest_consumer_fixture_publication`.
