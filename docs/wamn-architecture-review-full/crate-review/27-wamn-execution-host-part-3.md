# 27 wamn-execution-host, part 3

Scope: the control routes of the host. Nothing built or run.

## Files read

- `crates/execution/host/src/control_route.rs` (non-test part, in full)
- `crates/execution/host/src/control_route/integration.rs` (in full)
- `crates/execution/host/src/control_route/connection_test.rs` (in full)

## Callers read

- `wamn_platform_identity::connections::administration` (`execute`, `execute_with_store`, `execute_in_environment`, `execute_in`, `configure`)
- `wamn_platform_identity::connections::references` (in full)
- `wamn_control_registry::sql::ENVIRONMENT_LOCK_SQL`, `ENVIRONMENT_SHARED_LOCK_SQL`
- `host_route.rs::HostRoutes` (`with_projection_pause`, `with_connection_health`)
- Callers of `execute_with_store` (all are tests) and of `ProjectionPause` (one test)

## One purpose per file

- `control_route.rs`: control-plane writes and reads for org, project, member, environment, release and invite routes; projection of identity rows into each environment database.
- `control_route/integration.rs`: integration, external system and connection administration; projection into each environment database.
- `control_route/connection_test.rs`: run a health check of a connection before or after save.

## Findings

**1. The same write sequence is written about 12 times.** Fact: about 10 handlers in `ControlRoutes::handle`, plus `invite` and `integration_administration_in`, repeat: lock `writer` → `begin_in` → identity write → `AuthDrops` → commit → `changes.publish` → `environments` → `write_applications`.

- A: one private function `write(attachment, caller, project, op)`. It runs the sequence. `op` is the identity write only.
- B: keep.
- Recommend A.

**2. The role check runs twice for writes.** Fact: `handle` calls `admits` on `self.control` before the match. `begin_in` calls `admits` again inside the transaction. The second check is the one that holds against a late revoke (test `delayed_grant_cannot_restore_revoked_admin`).

- A: for writes, check only in `begin_in`. Keep the first check for reads only. Same rule as tab 26 finding 4 A (one check).
- B: keep both.
- Recommend A.

**3. Environment database connections are opened per call, outside pools.** Fact: 6 sites call `tokio_postgres::connect`: `application_counts`, `write_application`, `release_profile`, `write_status`, `integration_administration_in`, `connection_test_in`. `logins` reads the login URL from a file on each call. Timeouts differ: 5 s in `integration.rs`, 2 s for counts, none in `connection_test_in` (only the outer 10 s). `write_application` and `write_status` repeat `set_config` + `require_platform_rows` + transaction.

- A: one function `environment_transaction(project, env, caller, operation)`. It reads the login, connects with one timeout, opens the transaction and sets `app.user_id`/`app.operation`. All 6 sites use it.
- B: A, plus a moka cache of one pool per environment (login read once). This also lets reads stop holding the `writer` mutex (`connection_test_in` and the read and authorize paths of `integration_administration_in` hold it today, then roll back or only read).
- Recommend A now. B goes with the held item "DB connections outside pools".

**4. The environment lock SQL is copied, and connection writes release it early.** Fact: `integration_administration_in` writes the `wamn-env/` lock SQL inline twice. The same text is `wamn_control_registry::sql::ENVIRONMENT_LOCK_SQL`. For a connection delete it takes the key twice (transaction form, then session form). Inference: for a connection write that is not a delete, the transaction form ends at the first commit; the projection into environment databases then runs without the environment lock.

- A: take `ENVIRONMENT_LOCK_SQL` (session form) once per environment for every connection write and every delete. The existing `pg_advisory_unlock_all` releases it. Delete both inline copies.
- B: keep.
- Recommend A. Confirm the lock order against `control/environment/src/environment/lock.rs` in the control tab.

**5. The request is parsed three times.** Fact: `integration_administration_in` parses `ProjectScope`, then a `Value`; `administration::execute_in` parses the payload again and runs `validate_request`. `connection_test_in` parses a `Value` and runs `validate_request`.

- A: parse once to `Value` in the host; `administration::execute` takes `&Value`. Ties to the held request-element parser item.
- B: keep.
- Recommend A.

**6. Three public `execute` functions; one exists only for tests.** Fact: `execute`, `execute_with_store` and `execute_in_environment` all wrap `execute_in` (7 parameters). Every caller of `execute_with_store` is a test (identity, runtime, control/environment tests).

- A: one public `execute` with `environment: Option<&Transaction>` and `store: Option<&dyn Store>`. Delete the other two.
- B: keep.
- Recommend A. Detail goes to the identity tab.

**7. `deletion_identity` lives in the host but reads identity rows.** Fact: it calls `administration::configure` per group; for `external_system` it lists all rows and filters by name in Rust.

- A: move it to `wamn_platform_identity::connections::references`, next to `refuse_environment_references`.
- B: keep.
- Recommend A.

**8. `connection_test` reports the wrong error type.** Fact: a host without `connection_health` answers `invalid_input` / `integration_type` (a user input error). Every `Refusal::Failed` (for example a database error) becomes `{"ok":false,"error":"connection health unavailable"}`.

- A: no `connection_health` → `Refusal::Failed`. Let `Failed` pass as a refusal. Only the health result maps to `ok:false`.
- B: keep.
- Recommend A.

**9. Inline SQL in `connection_test_in`.** Fact: the selected version is read with inline SQL on `catalog.release_connection`, next to the identity call `required_connection_versions`.

- A: move it to `administration` as one function `release_version(tx, env_key, digest, connection)`.
- B: keep.
- Recommend A.

**10. A test pause lives in the product.** Fact: `ProjectionPause` (feature `test-util`) is a field of `ControlRoutes`, a builder `HostRoutes::with_projection_pause`, a re-export in `lib.rs`, and two `arrive` calls in `write_applications`. One test uses it: `delayed_grant_cannot_restore_revoked_admin`.

- A: delete `ProjectionPause`, the builder, the field, the re-export and the test. Test the lock contract once at the identity tier (`wamn_platform_identity::org::lock_membership`).
- B: keep.
- Recommend A (test rule 1: lowest tier).

**11. Thin wrappers.** Fact: `begin` only calls `begin_in(.., None)`. `environments` only calls `org_environments`. "Environment exists" (`project_envs(..).any(..)`) is written twice.

- A: delete `begin` and `environments`; call the callee. Write the environment check once.
- B: keep.
- Recommend A.

**12. SQL in the host crate.** Fact: `APPLICATION_COUNTS_SQL`, `RELEASE_SQL`, `RELATION_COLUMNS_SQL` and inline queries on `identity.project_env_memberships` and `identity.project_roles`.

- Added to the held SQL scatter item. No separate option.

## Tests deleted, changed or moved

- Delete: `tests/control_route_live.rs::delayed_grant_cannot_restore_revoked_admin` (finding 10). A replacement at the identity tier goes to the identity tab.
- Change: callers of `execute_with_store` (finding 6): `crates/identity/platform/tests/integration_administration.rs`, `connection_drops_live.rs`, `crates/platform/runtime/src/connection_facts/tests.rs`, `route_authentication/tests/signature.rs`, `connection_http/tests/oauth2_tests.rs`, `crates/control/environment/tests/environment_delete_live.rs`, `integration_restart_live.rs`, `support/exclusive_resources.rs`, `support/files.rs`.
- Change: `tests/support/connection_test.rs` and `control_route_live.rs::connection_routes_refuse_mismatches_and_project_only_enabled_rows` if they expect `invalid_input`/`integration_type` or `ok:false` for a failure (finding 8). Not read yet.
- Keep: unit tests in `control_route.rs` (`control_paging_…`, `member_list_cursor_…`, `status_…` ×3).

## Not read

- Test files and `tests/support/*` bodies (on hold with the other test lists).
- `wamn_platform_identity::org`, `application`, `connection_projection` bodies (identity tab).
- `control/environment/src/environment/lock.rs` (control tab, finding 4).

## Rulings

- Finding 3: A plus moka (one pool per environment, login read once). Pending more research: pool library, pool size, how the cache handles a changed login.
- Findings 4 and 10: A. The user noted these have one real option ("keep" is not one).
- All other findings accepted as recommended (1, 2, 5, 6, 7, 8, 9, 11: A; 12 added to SQL scatter).

## Check results, finding 3 (2026-11-11)

- **Pool library.** Resolved by tab 34 U2: SQLx `PgPool` (the host runs every query on SQLx).
- **Changed login.** Fact: `logins` reads the login from a file on every call; the files are a mounted secret. Resolved: key the moka cache by (project, environment, SHA-256 of the login text). A rotated login opens a new pool. The old one leaves by `time_to_idle`, and the eviction listener closes it.
- **Pool size.** A number for the owner. Proposal: `max_connections` 2, `min_connections` 0, a short idle timeout. Control traffic is low, and a pool per environment multiplies.

Ruling (2026-11-11), pool size: a configuration option of the host (maximum connections, minimum connections, idle timeout per environment pool). The proposal above (2, 0, short) is only the default.
