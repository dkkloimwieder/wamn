# 78 Postgres client after U2

Scope: how host and control code reach PostgreSQL at `epic/dep7` (b94833fd), checked against owner ruling U2 (tab 34). The `wamn_postgres` plugin was reviewed in tabs 11 to 14; this tab reads only what U2 changes and what those tabs did not cover. Nothing built or run.

## Earlier work checked first

- Tab 34, owner ruling U2: SQLx at build for all SQL; the host and control code run all SQL on SQLx (replacing `tokio_postgres` and `rusqlite`); guests use only the `wamn:postgres` host interface.
- Tab 09: one login per database role; `CredentialGeneration` goes. Tab 11: findings on the plugin's pools and credentials (finding 1 credential resolved before pool lookup, 5 statement timeout set three times, 7 one URL parsed in five places).
- Tab 60 6 A3 and 5 B: SQLx `Migrator`. Tab 65 2 B: CNPG managed roles.

## Files read

- `runtime/src/plugins/wamn_postgres`: file sizes; headers of `types.rs` and `pool.rs`; `types.rs` function list (the NUMERIC decoder); `statements.rs::prepare_statement_set`.
- `deploy/infra/cnpg-cluster.yaml` (`postgresql.pg_hba`).
- Workspace and `wamn-runtime` `Cargo.toml` (database client crates).

## Callers read

- Repository grep: `tokio_postgres` and `deadpool_postgres` in non-test code; `rusqlite`; `NoTls`; TLS connectors for PostgreSQL; `sslmode`; `statement_timeout`.

## Facts

- Non-test files using `tokio_postgres` or `deadpool_postgres`: 140, in about 20 crates (most in `wamn-environment-control` 34, `wamn-runtime` 27, `wamn-package-control` 14, `wamn-platform-identity` 13). `deadpool-postgres` is used only by `wamn-runtime`. `rusqlite` is used by `wamn-run-state-sqlite` and `services/edge`.
- The `wamn_postgres` plugin keeps its own pool and credential layer (`pool.rs` 1,235, `claims/pools.rs` 316, `credential_exactness.rs` 840) and its own value codec (`types.rs` 568, with a hand-written binary NUMERIC decoder).

## Findings

**1. No PostgreSQL connection uses TLS.**

Fact: non-test code passes `NoTls` 145 times, and no PostgreSQL TLS connector exists in the repository (grep for `MakeRustlsConnect`, `tokio_postgres_rustls`, `MakeTlsConnector`). The CNPG cluster's `pg_hba` is `host all all all scram-sha-256`, which admits connections without TLS. Passwords are protected by SCRAM; query text and rows are not.

Options:

- A: TLS on every connection. `pg_hba` becomes `hostssl`; clients connect with `sslmode=verify-full` and the CNPG cluster CA. Done as part of the U2 move, since SQLx's `PgConnectOptions` carries the TLS settings.
- B: keep plaintext inside the cluster.

Recommend A.

## Follows from earlier rulings

U2 replaces, in the plugin:

- The pool and credential layer with one SQLx `PgPool` per login and database; `PgConnectOptions` is parsed once (settles tab 11 finding 7) and the statement timeout is set once in `after_connect` or the connect options (settles tab 11 finding 5). With tab 09, the credential generation key goes.
- Prepared statement sets with SQLx's per-connection statement cache.
- The value codec with SQLx types. Inference: NUMERIC needs an exact decimal type that keeps scale (SQLx's `bigdecimal` feature) to keep today's canonical text; `rust_decimal` holds at most 28 digits.
- Elsewhere: `deadpool-postgres` goes; `rusqlite` in `wamn-run-state-sqlite` and `services/edge` becomes SQLx SQLite.

## Fine as is

- The plugin's claims and transaction model (tabs 11 to 14 ruled them); only the client underneath changes.

## Tests deleted, changed or moved

- Finding 1 A: one live test that a connection without TLS is refused.
- Follows: `types.rs` NUMERIC decoder tests; pool and credential tests named in tab 11's list change to SQLx pools.

## Not read

- The plugin bodies beyond tab 11 to 14's reading; the other 113 `tokio_postgres` files outside the runtime.

## Rulings

- 2026-11-12: finding 1 A (TLS on every PostgreSQL connection: hostssl in pg\_hba, clients verify-full against the CNPG CA, done with the SQLx move).
