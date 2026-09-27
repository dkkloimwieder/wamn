# Route intents

A cloud route of kind create, update, delete, or command logs one intent for each input item in `wamn_run.intents` before its export runs.
An intent that began and never finished is uncertain, and its key answers `intent-uncertain` until an operator resolves it.
The [execution architecture](../architecture/execution.md) states the rules.

## List and resolve

Run these commands with project-admin authority. `WAMN_PG_ADMIN_URL` or `--admin-database-url` names the project database.
Use the same `--tenant` and `--environment` flags for each command.

```bash
wamn-ctl intents list --tenant acme --environment dev
wamn-ctl intents resolve --tenant acme --environment dev "$ID" external-evidence
```

The list prints one tab-separated line for each uncertain intent: id, operation, key, package, release, and tenant.
The output matches `wamn-edge intents list`.
The basis is `external-evidence`, `counterparty-confirmation`, or `operator-judgment`.
After a resolve, the key answers `intent-resolved` with the basis, and the caller sends a new key.
A resolve of an intent that is not uncertain in that tenant and environment fails and changes nothing.
