# REST connector

This integration application composes the `wamn_integration` base and owns no domain table.
It shares one release with the separate `rest_consumer_fixture` application.
Both dependencies use exact versions and component digests.
Build the base and consumer before this package.

The minute schedule supplies `due`.
The first node reads `last_synced_at` through the base's `setting.read` operation.
The `http-page` node collects the fixture pages and carries `/due` into its output.
JSONata maps the items into one consumer command envelope, including when the item array is empty.
The consumer result retains the due time in `request_id`.

After every result succeeds, the integration writes that due time to `last_synced_at` through `setting.write`.
A refusal prevents that write.
The checkpoint value is a JSON string containing an RFC 3339 timestamp.
An absent checkpoint means that no sync completed.

Bind the `http-page` requirement `upstream` to the fixture API before deployment.
Grant the integration principal a role with the consumer's `item.record` permission.
The fixture uses `/pages?limit=2`.

The second workflow starts on a consumer item insert.
It calls the base's `log.append` with outcome `pushed`, the item key, and the node context's delivery ID.
The log primary key prevents duplicate rows for the same delivery and item.
Change data capture (CDC) includes only the consumer schema because its event registration reads that schema.

Run `cargo test -p wamn-control --test rest_connector_publication` after building all three packages.
The test admits every component and resolves both workflows in one release.
