# REST connector

This integration application composes the `wamn_integration` base and owns no domain table.
It shares one release with the separate `rest_consumer_fixture` application.
Both dependencies use exact versions and component digests.
Build the base and consumer before this package.

The item pull is an environment workflow: a project admin authors and enables `rest_item` on the application host. Its entry declares a minute schedule. Each due moment supplies `due`.
The first node reads `last_synced_at` through the base's `setting.read` operation.
The `http-page` node collects the fixture pages and carries `/due` into its output.
JSONata maps the items into one consumer command envelope, including when the item array is empty.
The consumer result retains the due time in `request_id`.
The `completed` mapping refuses a failed consumer result and forwards the successful batch to `finish`.

After every result succeeds, the integration writes that due time to `last_synced_at` through `setting.write`.
A refusal prevents that write.
The checkpoint value is a JSON string containing an RFC 3339 timestamp.
An absent checkpoint means that no sync completed.

Bind `applications.<app>.connections.upstream` to an enabled org HTTP connection before deployment.
Keep the endpoint and credentials in that connection row.
The HTTP host supplies the bound external system ID with every page.
The page collector carries it into the consumer batch as `external_system_id`.
Workflow parameters cannot select that source identity.
The pull runs as the `workflow` user with its execution role. Grant that role the consumer's `item.record` permission.
The fixture uses `/pages?limit=2`.

The second workflow starts on a consumer item insert.
It calls the base's `log.append` with outcome `pushed`, the item key, and the node context's delivery ID.
The log primary key prevents duplicate rows for the same delivery and item.
Change data capture (CDC) includes only the consumer schema because its event registration reads that schema.

Run `cargo test -p wamn-env --test rest_connector_publication` after building all three packages.
The test admits the components for the release.
