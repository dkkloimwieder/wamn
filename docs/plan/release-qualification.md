# Release qualification

Updated through: 2026-10-01, `main` at `01ca2425d`. Finding `wamn-ld93.33`. The owner ruled the four forks of §7 on 2026-10-01. The cutover of `docs/plan/kind-to-type.md` §3.2 waits on it, because no head is set without a qualification.

## 1. Goal

`wamn-ctl qualify-release` qualifies the release of any environment from its candidate. The kind cases take every release input from the candidate, never from constants. These are the tenant, the environment, the org, the project, the release id, the route host and the package set. They publish that release in the disposable stack, and the stack reproduces the bytes of the candidate.

After this work, B7 and B8 of the cutover can qualify release 2 of the wamn-dev Receiving and WMS environments.

## 2. Fixed rules

- The byte-equality contract stays. The fixture must reproduce the canonical bytes of the candidate manifest (`crates/control/lib/src/delivery.rs:170-178`).
- The fixture constants (`JOURNEY_PACKAGES`, `TENANT`, `ENVIRONMENT`, `RELEASE_ID`) become the default candidate of the fixture. They are not a gate.
- There is no second qualification path.
- No hand statement sets a head.

## 3. Current state

All of this is measured at `9f5331e25`. Nothing was built or run.

### 3.1 Case selection

`qualify-release` picks the case set from the package ids of the candidate (`crates/control/lib/src/delivery/qualification.rs:105-128`):

- WMS: the candidate has `wamn_wms` and no `wamn_receiving`.
- Receiving: the candidate has `wamn_receiving` and `client_acme_receiving`, and no `wamn_wms`.
- Any other candidate fails with "the candidate has no supported existing application qualification case".

wamn-dev Receiving release 2 has `wamn_receiving@2.0.0` only (`kind-to-type.md` §3.2 B7 step 4). So `qualify-release` refuses it before any case runs.

The six cases are listed at `qualification.rs:15-24`. Each one runs as `<exe> <case> --exact --ignored` with `WAMN_DELIVERY_CANDIDATE` and `WAMN_DELIVERY_RESULT` (`qualification.rs:303-327`).

| Application | Case | Defined at |
| --- | --- | --- |
| Receiving | `route_authentication_live::cluster::route_cases::command_histories` | `apps/wamn_receiving/tests/route_authentication_live/cluster/route_cases.rs:16` |
| Receiving | `…::postcommit_case::baseline_overlay_and_materializer_progress` | `…/cluster/postcommit_case.rs:19` |
| Receiving | `…::queue_recovery::interrupted_durable_queue_item_completes_after_host_restart` | `…/cluster/queue_recovery.rs:17` |
| WMS | `cluster::released_wms_routes` | `apps/wamn_wms/tests/cluster.rs:36` |
| WMS | `cluster::released_wms_routes_retain_committed_work_after_label_failure` | `apps/wamn_wms/tests/cluster.rs:50` |
| WMS | `cluster::restarted_wms_host_retains_compiled_code_and_serves_requests` | `apps/wamn_wms/tests/cluster.rs:66` |

### 3.2 The constants each case pins

Receiving. The three cases share one publish path:

- Org `acme`, project `receiving`, environment `dev`, tenant `receiving-route-auth` and release id 1 (`crates/control/lib/src/dev/environment.rs:237-245`). The publish uses them at `apps/wamn_receiving/tests/route_authentication_live/environment.rs:595-599`.
- The packages are `JOURNEY_PACKAGES` (`apps/wamn_receiving/tests/route_authentication_live.rs:375-389`). These are `wamn_receiving` and `client_acme_receiving`, with versions read from each `wamn.json`.
- The wiring is `client_acme_receiving::quality_create_inspection`, with version 1 written in the code (`route_authentication_live.rs:387`, `environment.rs:582-591`).
- The route host is `receiving.localhost` (`route_authentication_live/cluster.rs:173`).
- The candidate scope check is at `route_authentication_live/delivery.rs:13-24`.
- The postcommit case uses a copy of the base package in `compatibility-packages/receiving` (`postcommit_case.rs:50-54`). With a candidate, it allows only the baseline variant (`postcommit_case.rs:44-48`).

WMS. The three cases go through `run_case`:

- Org `acme`, project `wms`, environment `dev`, tenant `wms-route-auth` and release id 1 (`apps/wamn_wms/tests/environment.rs:37-45`).
- The package is read from `apps/wamn_wms/wamn.json` (`environment.rs:334-345`).
- The wirings are every `publication/wirings/*.json`, with the version from each document (`environment.rs:526-555`).
- The route host is `wms.localhost` (`apps/wamn_wms/tests/cluster.rs:437`).
- `pat_only_attachments` rewrites every `auth-policy.modes` to `["pat"]` (`environment.rs:394-425` at `01ca2425d`). The authored file has `["pat","session"]` (`apps/wamn_wms/publication/attachments.json:69-70`).
- The candidate scope check is at `apps/wamn_wms/tests/delivery.rs:13-21`.

Both applications take the publisher from the PAT Secret annotation of the fixture (`routes.rs:266-269`, `environment.rs:564-567`) and use the run schema `wamn_run`. The guests come from `candidate.target_directory` (`cluster/build.rs:28-32`). Each manifest component digest must be in the candidate files (`delivery.rs:229-239`).

### 3.3 What makes the bytes

The bytes are the RFC 8785 form of `ServingManifest` (`crates/catalog/model/src/serving_manifest.rs:948-957`). Production builds it at `crates/control/lib/src/publish_release.rs:1392-1410`.

| Field | Source |
| --- | --- |
| `format-version` | The constant 4 (`serving_manifest.rs:33`). |
| `release` | `tenant-id`, `effective-release-id`, `environment` and `packages`, from the flags (`serving_manifest.rs:129-134`). |
| `components` | Rows of `catalog.component_library`, with the sha256 of each component (`publish_release.rs:72-77`). |
| `routes` | The attachments, the route contracts of the package manifests and the component facts (`publish_release.rs:1372`). |
| `attachments` | The authored and generated attachments (`attachments.rs:184-203`). The route host and the auth policy are part of them (`attachments.rs:350-361`). |
| `workflow.wirings` | `wiring_hash` from `catalog.wirings` (`publish_release.rs:1903-1909`). |
| `workflow.registrations` | The package manifests and the wiring targets (`publish_release.rs:1382`). |

No field holds the org, the project, the publisher, the run schema, a time, a random id or the environment instance. The bytes do depend on store rows: `component_library`, `wirings`, and the `manifest_sha256` of `catalog.packages`. The route host and the auth policy of each attachment change the bytes.

### 3.4 Release id

`publish-release` uses `--effective-release-id` as given (`publish_release.rs:492-500` is the dev loop only). The schema checks only `effective_release_id > 0` (`deploy/sql/control-portable-store.sql:52`, `deploy/sql/catalog-schema.sql:79`). No rule requires release 1 before release 2. `select-release` claims the tenant and writes the head with no order check (`crates/control/lib/src/delivery/deployment.rs:62-96`, `:427-435`).

A fresh store needs these rows for release 2:

- In the project database, a `catalog.packages` row per package, the `component_library` and `wirings` rows, and the `wamn_run.environment_policies` row (`publish_release.rs:113-120`).
- In the control database, a `registry.env_policies` row for the same org with an equal hash (`verification_policy.rs:101-139`), and `catalog.tenant_environments` (`control-portable-store.sql:271-282`).

### 3.5 Route host

The cases reach a host through a NodePort with a `Host` header. There is no ingress, no DNS name and no TLS (`postcommit_case.rs:283`, `apps/wamn_wms/tests/cluster/deployment.rs:276-348`). The HTTP workload is rendered with the route host (`test-support/infrastructure/rendering.rs:131`, `:383-385`). The host matches the `Host` header by equality (`flow_http_routing.rs:776-778`). TLS appears only in `edge_case.rs:320-388`, which is not a qualification case.

### 3.6 Copy mechanisms

- `wamn-ctl-ops copy-project-env` (`crates/control/lib/src/copy_project_env.rs`) copies the rows of one data schema with `pg_dump -Fd` and `pg_restore --data-only` (`crates/control/provision/src/copy.rs:193-235`). It does not copy `catalog`. The destination must already be provisioned (`copy_project_env.rs:31-32`).
- The dev loop clones a database with `CREATE DATABASE … TEMPLATE` (`dev/environment.rs:474-520`).
- `docs/plan/upgrades.md:11` describes a representative copy of the prior database as deferred design.

## 4. Design

The candidate already carries the manifest, and the manifest names the tenant, the environment, the release id and the packages. The design adds what the manifest does not name, and it moves every case input from a constant to the candidate.

- `prepare-release` takes `--org` and `--project`, and writes them into the candidate. They are not in the bytes, but the policy row and the publication record use them (§3.4).
- The Receiving and WMS fixtures provision the stack with the org, the project, the tenant and the environment of the candidate. They publish the release id of the candidate, with its packages, its wirings and its route host. The scope checks of `delivery.rs` compare with the candidate instead of the constants.
- With no candidate, each fixture builds its default candidate from today's constants, so the existing cases run unchanged.

### 4.1 Fresh store

The fixture publishes the release id of the candidate directly into a fresh store. The schema checks only `effective_release_id > 0` (`deploy/sql/control-portable-store.sql:52`). The next-id rule of `publish_release.rs:492-500` belongs to the dev loop only. No placeholder release 1 is published. A copy of an installed database stays with the package-upgrade epic.

### 4.2 Route host

The route host comes from the candidate. The cases send it as the `Host` header, as `postcommit_case.rs:283` does today. Nothing else changes.

### 4.3 Case selection

`application` at `qualification.rs:108-125` matches the package set of the candidate exactly, in three arms:

| Package set | Cases |
| --- | --- |
| `{wamn_receiving}` | The Receiving cases that touch no overlay, listed below. |
| `{wamn_receiving, client_acme_receiving}` | All three Receiving cases of §3.1. |
| `{wamn_wms}` | The three WMS cases of §3.1. |

Any other set fails as it does today. The Receiving-alone publish is part of this change: with the set `{wamn_receiving}`, the Receiving fixture publishes `wamn_receiving` alone and no Acme wiring. No release is qualified by a case that needs a package that the release does not carry.

These Receiving cases touch no overlay, measured at `01ca2425d`:

| Case | Defined at | What it calls |
| --- | --- | --- |
| `route_cases::command_histories` | `apps/wamn_receiving/tests/route_authentication_live/cluster/route_cases.rs:16` | The base routes `/receiving/record_receipt` and `/purchase_order/update` (`apps/wamn_receiving/tests/receiving_command_histories_live.rs:27`, `:356`). |

`queue_recovery::interrupted_durable_queue_item_completes_after_host_restart` is not in the Receiving-alone set. Its durable queue item starts only from the wiring `receiving_record_receipt` of `BASE_PACKAGE_ID` (`queue_recovery.rs`). Receiving alone carries no wiring since `0fae1369d`, and a test-only wiring would break the byte contract of the qualification. The case stays in the Acme arm. Owner ruling of 2026-10-01 on `wamn-ld93.33.11`.

Acme takes part in `record_receipt` only through its own route `/acme/receiving/record_receipt` (`apps/client_acme_receiving/publication/attachments.json:11`). `postcommit_case::baseline_overlay_and_materializer_progress` exercises the overlay (`postcommit_case.rs:44-54`), so it runs only in the Acme arm.

`command_histories` calls no Acme route. But its result record reads Acme files: the digest of `client_acme_receiving.wasm` (`route_cases.rs:29`), and the schema identity and provenance of `apps/client_acme_receiving/generated/package-weld.json` (`receiving_command_histories_live.rs:953-960`, `:1020`). The owner ruled on 2026-10-01 that the case stays in the Receiving-alone set. The record follows the candidate: it holds the digest and package identity of each package that the candidate carries. The hard-coded Acme entries are a fixture constant that issue 1 replaces.

### 4.4 WMS auth policy

The WMS fixture publishes the authored attachments unchanged, so the bytes keep `["pat","session"]`. The change deletes `pat_only_attachments` (`apps/wamn_wms/tests/environment.rs:394-425` at `01ca2425d`) and its caller. The fixture installs the session issuer the way the Receiving cluster cases do, with `prepare_application`, `prepare` and `adjust_host`. These functions move from `route_authentication_live/cluster/session_cluster.rs` to `test-support`, and both fixtures call them. Without a candidate, the WMS fixture builds `wamn-identity:<name>` itself. With a candidate, it takes the image of `--identity-image`. The WMS cases keep calling PAT routes.

Every candidate of the cutover carries three images, all built by B0 from the cutover commit: `--host-image`, `--identity-image` and `--gates-image` (`docs/plan/kind-to-type.md` §3.2 B0, B7 and B8).

## 5. Issues

All issues land on one branch. They change the workspace and the kind cases only. No issue runs against wamn-dev.

The owner set this order on 2026-10-01. The deployment agent builds all four in order, on one branch, in the workspace and kind only, and every commit is green.

1. The fixture identity comes from the candidate: the tenant, the environment, the org, the project, the release id, the route host and the package set (§4, §4.1, §4.2). The result record of `command_histories` follows the candidate too (§4.3).
2. The three-arm selection and the Receiving-alone cases (§4.3).
3. The WMS session issuer and the deletion of the rewrite (§4.4).
4. A kind dry run of `prepare-release` and `qualify-release` for both wamn-dev release 2 candidates. The run compares the candidate bytes with a publish from the same inputs.

## 6. Out of scope

- The cutover steps of `kind-to-type.md` §3.2, which wait on this spec.
- Package migrations of an application (`docs/plan/upgrades.md`).
- `deploy-release` and its deployment documents.

## 7. Design forks

The owner ruled on 2026-10-01 (recorded on `wamn-ld93.33`):

1. Fresh store. The copy mechanism stays with the package-upgrade epic (§4.1).
2. The route host comes from the candidate and is sent as the `Host` header. Nothing else changes (§4.2).
3. Case selection matches the package set exactly, in three arms. The Receiving-alone publish is part of the change (§4.3).
4. The WMS fixture installs the session issuer, and the rewrite is deleted (§4.4).

The forks as they were put to the owner:

1. Release id. No rule requires release 1 before release 2 (§3.4). So the source does not support the premise that release 2 needs a release 1.
   - Fresh store. The fixture publishes release 2 directly. It costs nothing beyond issues 3 and 4.
   - Copy of the installed project database, restored into the kind stack, then `upgrade-schema` on the copy. This rehearses B3. `copy-project-env` copies one data schema only and needs a provisioned destination (§3.6), so the copy needs a whole-database dump and restore. The copy brings the `dkk` policy row, which needs a matching `registry.env_policies` row in the kind `wamn_system`. It also brings the application rows of wamn-dev, which hold the owner's data, into the kind stack.
   - Placeholder release 1. It is not needed, because no rule asks for it.
2. Route host. The cases send a `Host` header to a NodePort, with no TLS (§3.5). If the cases take the host from the candidate, kind can serve `receiving.wamn.dev` and `wms.wamn.dev`. The cases need no change to how they name their host beyond that.
3. Receiving package set. Acme is not on wamn-dev, and release 2 is `wamn_receiving@2.0.0` alone (`kind-to-type.md` §3.2 "No overlay", `gcp.md` §5.3 "Receiving ran the same"). Case selection refuses that set (§3.1), and `baseline_overlay_and_materializer_progress` exercises the overlay. The owner decides which cases qualify a Receiving release without Acme.
4. WMS auth policy. The WMS fixture rewrites every route to `["pat"]` because the kind stack installs no session issuer (`apps/wamn_wms/tests/environment.rs:375-380`). The wamn-dev release keeps `["pat","session"]`, so its bytes differ. The owner decides between two options. The WMS fixture installs a session issuer, or the cases publish the authored policy and call only the PAT routes.
