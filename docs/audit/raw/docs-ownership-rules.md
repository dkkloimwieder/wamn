| Source file:line | Resolution | Owner or evidence |
| --- | --- | --- |
| docs/plan/cdc-reader-slot.md:14 | D | The no-new-switch constraint belongs to the retired implementation target. Source inspection cannot establish a standing ban. |
| docs/plan/cdc-reader-slot.md:53 | B | docs/operations/gcp.md:1372. source crates/control/provision/src/sql/cdc.rs:152 |
| docs/plan/cdc-reader-slot.md:84 | B | docs/operations/gcp.md:1331. The current section names recover-capture-gap and close-capture-gap. |
| docs/plan/cdc-reader-slot.md:96 | D | The 4GB setting exists, but the full paragraph also asserts historical disk usage and free space. Current source cannot establish those measurements. |
| docs/plan/cdc-reader-slot.md:106 | D | The branch order and workspace-test budget describe a completed implementation campaign, not current executable behavior. |
| docs/plan/cdc-reader-slot.md:114 | D | This is an unbuilt resync proposal, including delete loss and event synthesis. Current source does not establish its acceptance or implementation. |
| docs/plan/cdc-reader-slot.md:123 | D | The no-operator-write and no-recorded-loss ruling describes one historical WMS incident. Current source cannot establish its outcome. |
| docs/plan/cdc-reader-slot.md:124 | D | Issue ordering and the historical cause of the lost slot need retained incident evidence, not a source-only classification. |
| docs/plan/cdc-reader-slot.md:125 | B | docs/operations/gcp.md:1345. source services/cdc-reader/src/lib.rs:624 |
| docs/plan/cdc-reader-slot.md:127 | D | The unbuilt resync target refers to the retired plan. No current implemented resync establishes its disposition. |
| docs/plan/cdc-reader-slot.md:129 | B | docs/operations/gcp.md:1388. The section-number reference already becomes “as the gap record states”. |
| docs/plan/component-pull.md:9 | B | docs/architecture/capabilities.md:362. source Cargo.toml:102 |
| docs/plan/component-pull.md:13 | C | Cargo.toml:102. The pin is wasmCloud v2.11.0, not fd2bbc0a. |
| docs/plan/component-pull.md:96 | A | docs/architecture/capabilities.md:381. source services/edge/Cargo.toml:21. unchanged |
| docs/plan/environment-teardown.md:13 | C | services/ctl/src/env_verbs.rs:115. The command is env delete with one coordinate and optional --data. It has no old coordinate flags or --confirm plan mode. |
| docs/plan/environment-teardown.md:14 | B | docs/architecture/naming.md:83. The environment lifecycle section also states that deletion is safe to repeat.. source crates/control/lib/src/environment/delete.rs:108 |
| docs/plan/environment-teardown.md:21 | C | crates/control/lib/src/environment/delete.rs:173. Deletion calls Kubernetes and Helm itself. It does not print a Kubernetes deletion plan for an operator. |
| docs/plan/environment-teardown.md:22 | C | services/ctl/src/env_verbs.rs:115. The command is env delete with one coordinate and optional --data. It has no old coordinate flags or --confirm plan mode. |
| docs/plan/environment-teardown.md:51 | C | services/ctl/src/env_verbs.rs:115. The command is env delete with one coordinate and optional --data. It has no old coordinate flags or --confirm plan mode. |
| docs/plan/environment-teardown.md:55 | C | services/ctl/src/env_verbs.rs:115. The command is env delete with one coordinate and optional --data. It has no old coordinate flags or --confirm plan mode. |
| docs/plan/environment-teardown.md:56 | C | crates/control/lib/src/environment/mod.rs:106. env delete reads WAMN_CONTROL_DATABASE_URL, not the old --system-database-url input. |
| docs/plan/environment-teardown.md:57 | C | crates/control/lib/src/environment/delete.rs:357. The verb derives its admin connection from the platform database URL. It exposes no --admin-database-url flag. |
| docs/plan/environment-teardown.md:58 | C | services/ctl/src/env_verbs.rs:71. Current input names use WAMN_EVENT_NATS_*, not WAMN_EVT_NATS_* or the old NATS flags. |
| docs/plan/environment-teardown.md:59 | C | services/ctl/src/env_verbs.rs:115. The command is env delete with one coordinate and optional --data. It has no old coordinate flags or --confirm plan mode. |
| docs/plan/environment-teardown.md:63 | C | crates/control/lib/src/environment/delete.rs:173. The current order begins with Helm uninstall and drain, before reading registry rows. |
| docs/plan/environment-teardown.md:65 | C | crates/control/lib/src/environment/delete.rs:173. Deletion calls Kubernetes and Helm itself. It does not print a Kubernetes deletion plan for an operator. |
| docs/plan/environment-teardown.md:67 | B | docs/architecture/overview.md:186. source crates/control/lib/src/environment/delete.rs:347 |
| docs/plan/environment-teardown.md:68 | C | crates/control/lib/src/environment/delete.rs:17. The default preserves the database. --data delegates database deletion to CloudNativePG, rather than issuing DROP DATABASE WITH (FORCE). |
| docs/plan/environment-teardown.md:69 | C | crates/control/lib/src/environment/delete.rs:420. Generation roles are dropped only with --data, not on every delete. |
| docs/plan/environment-teardown.md:70 | A | docs/architecture/overview.md:209. source crates/control/lib/src/environment/delete.rs:49. unchanged |
| docs/plan/environment-teardown.md:71 | C | crates/control/lib/src/environment/delete.rs:441. The control and environment rows now share one transaction, rather than the two transactions in steps 6 and 7. |
| docs/plan/environment-teardown.md:75 | C | crates/control/lib/src/environment/delete.rs:179. A refusal after uninstall can leave earlier deletion steps applied. Refusals do not all change nothing. |
| docs/plan/environment-teardown.md:77 | C | crates/control/lib/src/environment/delete.rs:198. A missing environment row returns success, not a refusal. |
| docs/plan/environment-teardown.md:78 | C | crates/control/lib/src/environment/delete.rs:357. No supplied admin URL is validated. The platform connection supplies the cluster and database. |
| docs/plan/environment-teardown.md:79 | A | docs/architecture/overview.md:213. source crates/control/lib/src/environment/delete.rs:369. unchanged |
| docs/plan/environment-teardown.md:80 | C | crates/control/lib/src/environment/delete.rs:17. Deletion retains the database by default and has no pg_stat_activity refusal for other sessions. |
| docs/plan/environment-teardown.md:81 | C | crates/control/lib/src/environment/delete.rs:475. A matching retained catalog row is deleted conditionally. A mismatched row no longer triggers the old refusal. |
| docs/plan/environment-teardown.md:85 | C | crates/control/lib/src/environment/delete.rs:198. The retirement trigger survives, but a second finished delete returns success. The full rule says it refuses. |
| docs/plan/environment-teardown.md:89 | A | docs/architecture/overview.md:217. source crates/control/lib/src/environment/delete.rs:49. unchanged |
| docs/plan/environment-teardown.md:90 | A | docs/architecture/overview.md:218. source crates/control/lib/src/environment/delete.rs:81. unchanged |
| docs/plan/environment-teardown.md:91 | C | crates/control/lib/src/environment/delete.rs:262. Current deletion revokes provisioning PATs and deletes environment connections. The old blanket preservation rule no longer holds. |
| docs/plan/environment-teardown.md:104 | D | Mint-time refusal of retired suffixes is an excluded proposal. The SQL reader exists, but source does not establish a later owner decision on mint behavior. |
| docs/plan/environment-teardown.md:106 | C | crates/control/lib/src/environment/delete.rs:173. Deletion calls Kubernetes and Helm itself. It does not print a Kubernetes deletion plan for an operator. |
| docs/plan/environment-teardown.md:113 | C | crates/control/lib/src/environment/delete.rs:173. Deletion calls Kubernetes and Helm itself. It does not print a Kubernetes deletion plan for an operator. |
| docs/plan/environment-teardown.md:114 | C | crates/control/lib/src/environment/delete.rs:433. The current verb deletes Kubernetes Secrets itself and also removes the environment key from the shared control administration Secret. The old manual plan no longer applies. |
| docs/plan/environment-teardown.md:115 | C | crates/control/lib/src/environment/delete.rs:433. The current verb deletes Kubernetes Secrets itself and also removes the environment key from the shared control administration Secret. The old manual plan no longer applies. |
| docs/plan/environment-teardown.md:116 | D | The rule describes the old teardown live-test fixture and re-provision acceptance. Current source does not establish the retained test result or an equivalent fixture. |
| docs/plan/environment-teardown.md:117 | D | The rule describes the old teardown live-test fixture and re-provision acceptance. Current source does not establish the retained test result or an equivalent fixture. |
| docs/plan/operator-image.md:18 | C | crates/control/lib/src/platform/job.rs:12. Control verbs now create and manage Kubernetes Jobs through kubectl. |
| docs/plan/operator-image.md:28 | C | services/ctl/src/identity_verbs.rs:221. invite now prints a principal/org result, not “invitation for <id>”. |
| docs/plan/operator-image.md:29 | C | services/identity/src/password.rs:253. POST /users also requires operator authority. The old “only POST /pats and POST /invitations” list is incomplete. |
| docs/plan/operator-image.md:30 | A | docs/operations/gcp.md:858. source deploy/platform/identity/templates/service.yaml:16. unchanged |
| docs/plan/operator-image.md:42 | C | crates/control/lib/src/platform_build.rs:32. build-platform publishes host, runner, conditions, identity and edge-web. It no longer publishes a standalone operator image. |
| docs/plan/operator-image.md:44 | C | Dockerfile:268. The runner carries qualification case binaries, so a test edit can change the runner as well as conditions. |
| docs/plan/operator-image.md:48 | A | docs/operations/gcp.md:355. source crates/control/lib/src/platform/job.rs:664. unchanged |
| docs/plan/operator-image.md:52 | C | crates/control/lib/src/platform/job.rs:63. Jobs use typed requests generated by platform apply, not hand-filled private manifest copies. |
| docs/plan/operator-image.md:54 | B | docs/operations/gcp.md:1273. source crates/control/lib/src/platform/invite.rs:38 |
| docs/plan/operator-image.md:56 | C | crates/control/lib/src/platform/job.rs:798. The Job owner reads emitted Secrets automatically. The old hand-run mint-pat procedure and management-author deployment boundary no longer describe the install path. |
| docs/plan/operator-image.md:58 | C | crates/control/lib/src/platform/invite.rs:43. The invitation Job takes the existing CNPG superuser input and derives the system URL. It does not require the described fixed wamn-system-admin Secret. |
| docs/plan/operator-image.md:62 | D | The rule includes one-time removal of the owner’s hosts-file line and retention of other port-forwards. Repository source cannot establish those machine changes. |
| docs/plan/operator-image.md:72 | C | crates/control/lib/src/platform/job.rs:12. Control verbs now create and manage Kubernetes Jobs through kubectl. |
| docs/plan/operator-image.md:81 | C | crates/control/lib/src/platform_build.rs:32. The deployed Job image is the stamped runner image. The old ctl-only publication/cache split is not current. |
| docs/plan/operator-image.md:82 | C | crates/control/lib/src/platform/job.rs:1315. Install Jobs now run in platform, not identity. |
| docs/plan/operator-image.md:85 | D | This is historical live-run or build-time acceptance work. Source alone cannot establish its measured outcome or a later owner disposition. |
| docs/plan/operator-image.md:86 | D | This is historical live-run or build-time acceptance work. Source alone cannot establish its measured outcome or a later owner disposition. |
| docs/plan/request-round-trips.md:18 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:39. Logged writes add platform claim and finish SQL. The current three-trip update contradicts the blanket zero-added-round-trip rule. |
| docs/plan/request-round-trips.md:21 | D | This is a pg_stat_statements measurement target. Current source does not establish the measured calls-per-get count. |
| docs/plan/request-round-trips.md:76 | B | docs/architecture/execution.md:681. source crates/platform/runtime/src/auth_cache.rs:1 |
| docs/plan/request-round-trips.md:77 | C | crates/platform/runtime/src/plugins/wamn_postgres/operation_transaction.rs:193. The host waits for statements, then guest SQL, then finish/commit. A logged update takes three trips, not one. |
| docs/plan/request-round-trips.md:79 | D | The per-merge benchmark budget and latency targets need an owner measurement ruling. Current source cannot establish the historical timings. |
| docs/plan/routes-router.md:17 | C | crates/execution/workflow/src/queue/automation_live/jsonata.rs:5. Single-operation environment workflows are valid one-node graphs. They are no longer universally refused. |
| docs/plan/routes-router.md:33 | C | crates/catalog/model/src/serving_manifest.rs:406. ServingRoute has package, component, operation and type, and no terminal field. |
| docs/plan/routes-router.md:35 | C | services/host/src/host.rs:1510. Reviewed trusted components can reuse instances. A fresh Store per request is not universal. |
| docs/plan/routes-router.md:37 | D | This is a no-change promise for the retired implementation, not a current behavior rule. Source cannot establish that historic compatibility statement. |
| docs/plan/routes-router.md:47 | A | docs/architecture/execution.md:544. source crates/execution/workflow/router/src/walk.rs:7. unchanged |
| docs/plan/routes-router.md:50 | C | crates/catalog/model/src/wiring.rs:114. Nodes use admitted component operations, not the proposed fixed set including an approval primitive. |
| docs/plan/routes-router.md:51 | C | crates/execution/workflow/src/contract.rs:9. Current park affects queued runs only. The later workflow ruling says no run waits on a user (docs/plan/workflows.md:25). |
| docs/plan/routes-router.md:68 | D | The old test-relocation promise needs test identity and history evidence. Current source does not establish that the invocation path gets coverage exactly once. |
| docs/plan/routes-router.md:72 | C | services/edge/src/lib.rs:35. The accepted edge runtime does link transitive NATS and OTLP dependencies. The old absolute exclusion is false. |
| docs/plan/routes-router.md:74 | D | The local routes and device loop exist, but the full rule also assigns future edge workflow support. No current owner ruling settles that future dependency. |
| docs/plan/routes-router.md:82 | C | crates/execution/workflow/src/queue/automation_live/jsonata.rs:5. Single-operation environment workflows are valid one-node graphs. They are no longer universally refused. |
| docs/plan/routes-router.md:85 | C | deploy/sql/run-state.sql:401. Cron and schedule workflows now have users. The old “Cron exists with no user” statement is obsolete. |
| docs/plan/routes-router.md:88 | B | docs/architecture/data-access.md:322. Read bypass and cloud/edge adapters remain documented runtime owners.. source crates/platform/engine/src/operation/intent.rs:76 |
| docs/plan/routes-router.md:90 | D | The serial epic sequence is a completed campaign instruction. It does not establish a present runtime contract. |
| docs/plan/web-deployment.md:20 | C | crates/control/lib/src/publish_release/attachments.rs:389. Publication no longer seals --route-host into a release. Environment/workload routing supplies the hostname. |
| docs/plan/web-deployment.md:34 | C | crates/schema/generator/src/tofu.rs:190. The GCP load balancer forwards all traffic through nginx. It does not send page paths directly to a backend bucket as old option A defined. |
| docs/plan/web-deployment.md:43 | C | deploy/gcp/environments/dkk--receiving--dev.k:9. Receiving and WMS use separate route hosts, not one host for every application (deploy/gcp/environments/dkk--wms--dev.k:9). |
| docs/plan/web-deployment.md:45 | C | deploy/gcp/environments/dkk--wms--dev.k:9. Separate application hosts are already deployed. They are not a future request. |
| docs/plan/web-deployment.md:47 | B | docs/operations/gcp.md:3. The current GCP owner records the per-application hostname arrangement. |
| docs/plan/web-deployment.md:57 | D | The statement conditions proposed cache options B/C on a measurement. The retired plan supplies no current measurement or later owner disposition for that condition. |
| docs/plan/workflow-feature.md:19 | B | docs/architecture/execution.md:617. Scheduling is documented in this same Workflows owner. The editor remains proposed in docs/plan/workflows.md:68. |
| docs/plan/workflow-feature.md:49 | B | docs/operations/queued-automation.md:83. source crates/execution/run-state/src/queue/workflow.rs:179 |
| docs/plan/workflow-feature.md:50 | A | docs/architecture/execution.md:548. source crates/execution/run-state/src/queue/workflow.rs:196. unchanged |
| docs/plan/workflow-feature.md:54 | C | crates/execution/workflow/src/contract.rs:23. The contract has park, release and list only. Manual starts now use workflow.start, not a Workflows::start method or the old workflow list command for runs. |
| docs/plan/workflow-feature.md:58 | C | deploy/sql/run-state.sql:401. Workflows are environment rows in catalog.workflows, not declarations in package wamn.json. docs/architecture/execution.md environment workflows records the new owner. |
| docs/plan/workflow-feature.md:70 | C | crates/catalog/model/src/wiring.rs:30. Environment workflows replace package registration identity and apply-package registration writes. |
| docs/plan/workflow-feature.md:72 | C | deploy/sql/run-state.sql:401. Event runs now have execution principals and roles, and former handlers run as queued one-node workflows. The old callerless/palette-only/inline-handler rule is false. |
| docs/plan/workflow-feature.md:74 | A | docs/architecture/execution.md:550. source crates/control/provision/src/sql.rs:741. unchanged |
| docs/plan/workflow-feature.md:80 | D | This rule requires crate-selection measurements and expression acceptance results. Current source shows the selected crate but cannot establish those historical measurements. |
| docs/plan/workflow-feature.md:84 | B | docs/architecture/execution.md:621. The WMS graph keeps its identity and starts after the row change.. source apps/wamn_wms/publication/wirings/inventory_move_and_label.json:3 |
| docs/plan/workflow-feature.md:94 | D | The per-issue test/landing order is a retired campaign instruction. Source cannot establish a current requirement or historical test outcome. |
| docs/plan/workflow-feature.md:107 | B | docs/architecture/execution.md:615. source services/edge/Cargo.toml:9 |
| docs/plan/workflow-feature.md:108 | D | This excludes the palette-component development condition under wamn-hw3n. Source alone does not establish that issue’s accepted disposition. |
| docs/plan/write-log.md:15 | B | docs/architecture/data-access.md:318. source crates/platform/engine/src/operation/intent.rs:53 |
| docs/plan/write-log.md:19 | C | crates/platform/engine/src/operation/intent.rs:76. Update and delete now log intent and claim their request_id. |
| docs/plan/write-log.md:23 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:39. Claim/read/finish SQL belongs to the host write-log module, not emitted per-package guest SQL or contract statement digests. |
| docs/plan/write-log.md:24 | D | This constrains future log replay and requires no implementation. No source fact or later ruling settles the future replay proposal. |
| docs/plan/write-log.md:25 | B | docs/architecture/data-access.md:322. Cloud SQL uses its write-log adapter. Edge intent state remains SQLite. the withdrawn epic is historical. |
| docs/plan/write-log.md:26 | D | The shared engine owns current intent rules, but the full rule also requires guest copies and a generator equality test. No retained test establishes every clause. |
| docs/plan/write-log.md:28 | A | docs/architecture/data-access.md:483. source crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:482. unchanged |
| docs/plan/write-log.md:53 | B | docs/architecture/data-access.md:317. source crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:88 |
| docs/plan/write-log.md:54 | B | docs/architecture/data-access.md:317. source deploy/sql/app-schema.sql:425 |
| docs/plan/write-log.md:55 | C | crates/platform/engine/src/operation/intent.rs:130. The request column stores hash bytes and the key remains in the hashed intent. The old canonical-request-bytes-without-key rule is false. |
| docs/plan/write-log.md:56 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:8. Two-step claims commit with null results until finish. A committed row can have a null result. |
| docs/plan/write-log.md:57 | C | crates/control/provision/src/audit_retention.rs:32. Retention is implemented now, not deferred “for retention later”. |
| docs/plan/write-log.md:63 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:1. The host owns the transaction and claim, rather than the generated codec. |
| docs/plan/write-log.md:72 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:39. Claim/read/finish SQL belongs to the host write-log module, not emitted per-package guest SQL or contract statement digests. |
| docs/plan/write-log.md:77 | C | crates/schema/generator/src/key.rs:83. UUID creates require a client id. Only identity/sequence-backed integer keys omit the id. The old blanket DEFAULT gen_random_uuid rule is false. |
| docs/plan/write-log.md:78 | C | crates/platform/runtime/src/plugins/wamn_postgres/write_log.rs:39. Claim/read/finish SQL belongs to the host write-log module, not emitted per-package guest SQL or contract statement digests. |
| docs/plan/write-log.md:91 | D | The one-branch/one-agent and per-issue test rules belong to a completed campaign. Source cannot establish a present requirement. |
| docs/plan/write-log.md:100 | D | This proposes future replay requirements including commit order and determinism. No current implementation or later ruling settles them. |
| docs/plan/write-log.md:101 | C | crates/control/provision/src/audit_retention.rs:32. Successful write-log rows expire after 30 days. The log no longer grows without any retention period. |
| docs/plan/docs-audit.md:9 | C | 82df826ce. the later ownership change assigns integration contracts to integration-schema.md, so platform-config.md is no longer the universal newest authority on those subjects. |
| docs/plan/docs-audit.md:11 | D | This is a precedence instruction between dated plans, not executable behavior. Source cannot establish that the old newest-plan rule still controls every unresolved sync decision. |
| docs/plan/docs-audit.md:21 | B | docs/architecture/data-access.md:161. Numbered primary keys are refused in the Numbering section.. source crates/schema/generator/src/key.rs:49 |
| docs/plan/docs-audit.md:27 | B | CLAUDE.md:131. The version procedure is docs/operations/building.md, Platform version procedure. Completed-plan deletion is recorded in docs/history/README.md. |
