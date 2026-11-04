# Audit cleanup part 2a: review report (epic wamn-hchst)

Branch `worktree-fixtures` on origin, based on main b7aff8e09. It is not merged. The epic stays open until the reviewer accepts it.

## Issues

| Issue | Commits |
| --- | --- |
| .1 one shared cluster fixture | e5113bf99, b7e0f8472, 54045d4ee, 7841cb622, ebc0f9d16, 7988ead75 |
| .2 kept snapshot files | 29130418d |
| .3 fixture upgrade support, floors, environment file loader | abec40b9f |

The branch changes 94 files: 3,760 lines added and 4,831 lines removed.

## Issue 1

The `Cluster` type in `test-support/infrastructure/cluster.rs` owns kind cluster creation, candidate image import, operator installation, inspection and teardown. The Receiving, WMS, RC and identity JWKS journeys and the Kubernetes Job runner call it from Rust.

These files are deleted: `tools/receiving-cluster-journey-run`, `tools/wms-cluster-journey-run`, `tools/rc-gate-run`, `tests/integration/src/rc/Dockerfile`, `deploy/gates/m1-postgres.Dockerfile` (its only caller was the RC builder), `tools/kubernetes-gate-run`, `tools/identity-jwks-journey-run` and `tools/kind-cluster-delete-test`. `tools/delivery-owned` keeps `build-native`, `receiving`, `wms` and `remove-kept`. `remove-kept` runs the new `wamn-test-cluster` binary.

The lane mapped every assertion of every deleted script to a Rust test. The map is in /var/tmp/wamn-fixtures/i1-report.txt, section 2. All 14 Job verdict cases stay, and four new cases cover malformed specifications and command failure classes. The teardown cases of `kind-cluster-delete-test` are now unit tests of the fixture with a fake command runner.

These assertions are retired:

- The repository `evidence/` output rule goes (hygiene.md H1-5 owner answer).
- The RC debug images, the separate RC host installation and their assertions go (D3-6).
- The identity journey no longer builds images or checks that Docker labels match the current HEAD. It imports the pinned candidate images.
- The identity journey uses a PostgreSQL 18 sidecar and the production provisioning functions, not a provisioner Pod. The provisioner command line is no longer tested by this journey.

Root found and fixed three more defects during the run:

- `test_tiers` requires an owner for every ignored input. The identity journey needs `WAMN_DELIVERY_CANDIDATE`, so `tests/selection.toml` now names it (b7e0f8472).
- The Receiving and WMS cluster builds set `RUSTC_WRAPPER` to an empty value, a rule from the sccache era. Kache restores read-only files into the shared target, so the build without the wrapper could not write `libring` (54045d4ee). This defect was already on main.
- A failed fixture command printed only the program name. It now prints its arguments (7841cb622).
- The shared helper replaced two helpers that printed standard error, so run 4 failed with no cause. It now prints standard error. Standard output stays private (ebc0f9d16).

## Issue 2

These files are deleted:

- `deploy/gcp/wms-flow-http.yaml` and its writer `test-support/infrastructure/examples/workload_files.rs`
- `deploy/gcp/values-host-base.yaml` and `deploy/gcp/values-host.yaml`
- `deploy/platform/values-host-default.yaml`, `values-host-wms-pat.yaml` and `values-host-receiving-pat.yaml`
- their writer `test-support/infrastructure/examples/host_values_files.rs`

Main had already deleted `flow-http.yaml`, `materializer.yaml` and `wms-materializer.yaml`. Each test that read a snapshot as its expected host values now reads the release chart defaults and the values that `env apply` renders. The new helper is `render_check::host_values` in the harness. No test was deleted.

`deploy/infra/cnpg-cluster.yaml` stays, because the hard stop `crates/control/lib/src/platform/journey.rs:103` reads it. The comment at `services/host/src/host.rs:1276`, also a hard stop, still names `values-host-default.yaml`. The owner moved both to part 2b on 2026-11-03.

## Issue 3

`write_upgrade_package` had no caller, so it is deleted. The `floors` field had one writer and no reader. Root decided to delete it everywhere: the Rust field, its check, its render, the KCL declaration and the fixture lines. `ENVIRONMENTS`, `EnvironmentFile::path` and `EnvironmentFile::read` go with their two tests. `EnvironmentFile` stays, because `platform/build.rs` and `platform/packages.rs` read it. `predecessor_version` stays in every `wamn.k`.

## Test results

`tools/test-changes --base b7aff8e09 run` ran on 29130418d. The branch changes `tools/delivery-owned`, so the run included the full workspace. It ran before the owner rule of 2026-11-03. Log: /var/tmp/wamn-fixtures/full.log. Four suites failed:

| Suite | Failure | Cause |
| --- | --- | --- |
| root | `test_tiers` | This branch. Fixed in b7e0f8472, and the target passes on rerun. |
| root | `wamn-test-postgres` `process_server_directory_is_removed_after_its_test_process_panics` | Open bead wamn-i33kt. |
| jetstream | `live_derived_publish_replay_converges_through_jetstream_dedup` | Open bead wamn-6oi30. |
| wms-local | `a_waiting_shell_reloads_after_pending_and_sending_entries_finish` | Open bead wamn-fszfy. |

After 7841cb622, `cargo fmt`, the clippy run for `wamn-test-infrastructure` and the 16 fixture unit tests pass. The lane checks of each issue are in /var/tmp/wamn-fixtures/i1-report.txt and /var/tmp/wamn-fixtures/i23.out.

## The kind journeys did not pass

The epic named one kind journey each for Receiving and WMS through the new fixture. Root ran `tools/delivery-owned receiving` four times, on the existing 0.1.142 chart and its images in `wamn-local-registry`. WMS did not run, because it runs only after Receiving passes.

| Run | Result |
| --- | --- |
| 1 and 2 | The package build failed on the read-only `libring` file. Fixed in 54045d4ee. |
| 3 | The Receiving build record was stale. Root rebuilt the packages. |
| 4 | A `kubectl` command failed after 58 seconds. The error named no arguments. Fixed in 7841cb622, so the next run names the failed step. |
| 5 (owner-approved) | The fixture created the cluster, installed the platform and passed the step that failed in run 4. The Receiving build then failed after 1,312 seconds: `receiving.wasm` was not in the target. Root removed that target before the run, but the package build records stayed, so `wamn build` reported the packages as current and wrote no components. Root removed the records and rebuilt the packages. |
| 6 (owner-approved rerun) | The cluster, platform, builds and qualification passed. The Receiving case failed at publish after 250 seconds: `publish_release` read `WAMN_COMPONENT_ARTIFACT_BASE` from the process environment since d55002b3d on main, and the Receiving and WMS cases carry that address in their inputs. Fixed in 7988ead75: the caller passes the address, and the command line still reads the variable. |
| 7 (owner-approved, at 1703b5e58) | The cluster, platform, builds and qualification passed again. The Receiving case failed at publish after 303 seconds with `read the publisher's pinned HTTP route guest: No such file or directory`. `entry_publication.rs` reads the route guest path from `WAMN_FLOW_HTTP_COMPONENT`, or else from `/usr/local/share/wamn/http_route.wasm`, which exists only in the image. This is the same defect class as run 6 and is on main. The case builds that guest at `wasm32-wasip2/release/http_route.wasm`. Not fixed: a fix and a rerun wait for approval. Log: `/var/tmp/wamn-fixtures/kind-receiving.log`. |

Each run removed its cluster. No cluster, container or volume of this epic remains.

The owner rule of 2026-11-03 stops new cluster runs, so the epic acceptance on wamn-hchst now reads: merge on the per-merge selection, and the deferred run lands when the owner approves one. The journeys through the new fixture are therefore not proven on a cluster.

## Request for the deferred run

Purpose: prove the Receiving and WMS delivery cases through the new fixture, and find the `kubectl` step that failed in run 4.

Command: `tools/delivery-owned receiving`, then `tools/delivery-owned wms`, then `tools/delivery-owned remove-kept wamn-delivery-fx`, on the existing 0.1.142 chart, with no platform build.

Expected duration: root estimates 30 to 60 minutes if both cases pass, because no complete run exists to measure. A failure in Receiving ends the run sooner. If run 4 failed in this branch's fixture code, a fix and a second run follow, and each needs approval.

## Merge

Fast-forward after the reviewer accepts. Main is still b7aff8e09.
