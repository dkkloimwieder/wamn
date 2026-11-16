# Epic 2 report: phase 2 restructure (wamn-2axvn)

Branch `worktree-restructure`, head c762b6d0e. Main is not changed. The owner reviews the branch before any merge to main.

Scope commits: db43a5347 (scope), 2dcc86383 (owner rulings of 2026-11-16 on the four target questions).

## Issues

### 2.1 Remove change notices (wamn-2axvn.1)

Commits: 6813fc1ba, 0f92e9fe7. Merges: f7258bd5a, 9ef68ec1b.

- Moved: nothing.
- Deleted: `Publisher`, `Changes`, `Platform::auth_changes`, `Platform::auth_nats` and the carriers that only sent notices.
- Deleted: four broker dependencies that the removal left unused, and the unused `wamn-broker` line in crates/platform-install/Cargo.toml (0f92e9fe7).

### 2.2 Split wamn-execution-host (wamn-2axvn.2)

Commit: f73d2e9a2. Merge: 6c80d114c.

- Moved from crates/execution-host/src: `file_route.rs` to crates/blobstore, `number_counter.rs` to crates/sql, `oauth_callback.rs` to crates/http, `credential_cleanup.rs` to crates/connections.
- Moved to services/control: `control_route.rs` and `control_route/`, `host_route.rs`, `application_route.rs`, `environment_status.rs`, `connection_health.rs`.
- Deleted: the crate manifest, `lib.rs`, `operation.rs`, `platform_http.rs`, `file_collector.rs`, `file_collection.rs`.
- Made `pub`: module `sql::number_counter`, `FileRoutes::handle`, `FileRefusal::code`.

### 2.3 Split crates/identity (wamn-2axvn.3)

Commit: 3b35c9b1e. Merge: 86cd45915.

- Moved: `connections.rs` and `connections/` to crates/connections.
- Moved: `application.rs` and `configuration.rs` to crates/environment.
- Moved: `environment.rs` merged into `wamn_environment::environment` (`fill`, `fill_platform`).
- Moved: test-support/resources/credentials.rs to `wamn_connections::credentials`, behind the `test-util` feature (owner ruling).
- Made `pub`: `IdentityError::new`, `database_error`.
- Deleted: nothing beyond the old file locations.

### 2.4 Remove wamn-release-control, and move build (wamn-2axvn.4)

Commit: 87b79f151. Merge: 8cc5616f9.

- Moved: xtask/build to crates/build. component-composer `lib.rs` to `crates/build/src/composer.rs`, with its tests.
- Moved: `release_chart::{uninstall, recover_pending, recovery, Recovery}` to `wamn_kubernetes::helm`. The private `helm` helper became `wamn_kubernetes::helm::run_text`, `pub`.
- Moved: `chart_version`, `read_platform_version`, `ImageSet`, `image_set`, `stamp` to `wamn_platform_install::release_chart`.
- Moved: values, roles, names, application value types and `TELEMETRY_VARIABLES` to `wamn_environment::release_chart`.
- Moved: `env_policies.rs` to crates/provision (owner ruling).
- Moved tests: `release_chart_render.rs` to crates/environment/tests, `release_permissions_live.rs` to crates/identity/tests.
- Deleted: the release crate manifest and `lib.rs`, the composer manifest, `release_chart::upgrade` (no caller), the `Target` re-export.

### 2.5 Move the wamn.json model (wamn-2axvn.5)

Commit: 5ecf537d5. Merge: 088b492b7.

- Moved: the old `wamn_catalog::manifest` types, unchanged, to `wamn_catalog::manifest::installation` (owner ruling).
- Moved: the compiler `manifest.rs` to `wamn_catalog::manifest`, with `manifest_error.rs` (private, errors re-exported) and `source_name.rs` (`pub` child module).
- Made `pub`: `parse_authored`, `refuse_authored_stamps`, `validate`.
- Deleted: nothing.

### 2.6 Merges (wamn-2axvn.6)

Commit: a97f971c8. Merge: 7b0bcbc33.

- Moved: wamn-component-artifact and wamn-package-artifact into wamn-oci in crates/oci, as modules `component_artifact` and `package_artifact`. The `docker-credential-wamn` binary keeps its name.
- Moved: wamn-postgres-statements and wamn-data-access into wamn-guest in crates/guest, as modules `postgres_statements` and `data_access`. The export macros stay at the crate root.
- Not merged: wamn-events and wamn-event-wire (owner ruling, waits for 3.4).
- Deleted: the manifests of the merged crates.

### 2.7 Renames (wamn-2axvn.7)

Commit: 46cb1350a. Merge: c762b6d0e.

- Renamed: `wamn-ctl` to `wamn-cli` in services/cli (binary `wamn`), `wamn-control-provision` to `wamn-provision`, `wamn-compiler` to `wamn-codegen` in crates/codegen, `wamn-connections` to `wamn-connection` in crates/connection.
- Renamed: `wamn-web-front` to `wamn-web` in services/web (binary `wamn-web`), the identity service to `wamn-identity-service` in services/identity-service (binary `wamn-identity`), then `wamn-platform-identity` to `wamn-identity`.
- Renamed: guests/jsonata to guests/jsonata-expression, package `wamn-modbus` to `modbus`.
- Updated: the Dockerfile, xtask, current docs, and comments that named crates removed in 2.2 to 2.6.
- Deleted: the `wamn-execution-host` entry in the xtask wamn-edge refused list.

## Tests and imports that could not be kept

No import was dropped. These tests were deleted because they assert change notices that 2.1 removed, and an import change cannot repair them:

- crates/connections/src/connection_facts/tests.rs (3 tests).
- crates/environment/src/apply_package/tests.rs (1 test).
- crates/environment/src/provision_project_env/pat_secrets/tests.rs (3 tests).
- `signature.rs::rotation_refuses_the_old_signature_on_the_first_call_after_the_delete` in crates/sql/tests/support/route_authentication.

In 2.2, execution-host `operation/tests.rs` went with `operation.rs`, which had no caller after the split.

## Stops and choices

1. Scout stop: four targets made a dependency cycle or a name clash. The owner ruled: `credentials.rs` to wamn-connections under `test-util`, `env_policies.rs` to wamn-control-provision, the old catalog types to `manifest::installation`, and no events merge.
2. Root decisions that the owner accepted: delete the xtask execution-host check entry, make an item `pub` where a moved module needs it, allow dev-dependency cycles.
3. 2.1 stop: some carriers sent a notice and also did other work. Root chose to keep the other work (rule 1, bead note on .1). The integration enable and disable response still drops the connections key. Principal validation stays. `resolve_or_create_service` still returns the `Principal`.
4. 2.4 stop: `default_values` and the Helm recovery code shared the private `helm` helper. Root chose `wamn_kubernetes::helm::run_text`, `pub`.
5. 2.3 choice: `environment.rs` merged into the existing `wamn_environment::environment` module, and `credentials` is a top-level module behind `test-util`.
6. 2.5 choice: `manifest_error` stays private with re-exports. `source_name` is a `pub` child module.
7. 2.6 choice: one named module per merged library, and the export macros stay at the crate root.
8. 2.4 choice: the two release tests moved with the code that they test, to crates/environment/tests and crates/identity/tests.
9. 2.7 reading: a package rename also renames the Rust crate name. Runtime names stay, for example the `wamn-ctl` NATS certificate and broker user, image labels and test scratch names.
10. 2.7 review: the lane put false "deleted in Epic 2" comments in docs/operations/running-tests.md, in place of commands for tests that Epic 1 deleted (0943187e1). Root had the comments and the dead commands removed.
11. 2.7 review: the lane changed `with_file_name("wamn-ctl")` to `"wamn"` in crates/dev/tests/support. Root reverted it, because it changes which binary a test runs.

## Left for later

Bead wamn-wlu7o holds these items:

- crates/dev/tests/support/platform_schema.rs:264 and platform_upgrade.rs:244 run the binary `wamn-ctl`, which Epic 1 deleted.
- docs/operations/running-tests.md still has paragraphs that describe the deleted execution-host test targets.
- crates/environment/tests/support/schedule_fixture.rs is not compiled. It imports the deleted `OperationHost` and old crate names.

Other items:

- Unused dependency warnings at head: `anyhow` and `wamn-identity` (the old `wamn-platform-identity` warning under its new name). They are older than Epic 2.
- Historical records, docs/plan/wamn-restructure.md, .beads/issues.jsonl and the captured snapshots under web/wfe/examples keep the old names.

## Checks

Root ran the full check after each merge. The final run (e27) is at c762b6d0e. No tests, kind runs, cluster runs or GCP runs took place.

- Root `cargo build --workspace`: exit 0.
- Root `cargo clippy --workspace --all-targets`: no new error site compared with the base. The run shows four sites as new only because their paths changed in 2.7: `connection_credentials/test_util.rs`, `pod_identity.rs`, `password.rs` and `https_surface.rs`. All four are in the base list under their old paths.
- apps/ `wamn build`, with one local Postgres: the same four digest pin failures as the base (wamn-eulfr.39). The other eight packages build.
- apps/ build and clippy: the same errors as the base, under the new paths. The 3 unsafe errors of the guest example no longer show, because the merged `wamn-guest` library errors stop clippy first. Root ran clippy on `wamn-guest` with `-A clippy::implicit_clone` and found all 3 errors.
- apps/wamn_wms/tests and apps/wamn_receiving/tests clippy: the same errors as the base.
