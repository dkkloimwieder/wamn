`apps/client_acme_receiving/` owns package `client_acme_receiving`, guest component `client-acme-receiving`, data crate `wamn-client-acme-receiving-data-access`, generated native UI crate `wamn-generated-client-acme-receiving-tui`, and test crate `wamn-client-acme-receiving-tests`.

From the repository root, run the local SQLx test:

```bash
SQLX_OFFLINE=true cargo test --locked --offline -p wamn-client-acme-receiving-tests --test client_acme_sqlx_verifier
```

SQLx reads the committed metadata in `tests/.sqlx/` without a database.
The [build and test runbook](../../docs/operations/build-and-test.md) gives the database and metadata regeneration commands.
