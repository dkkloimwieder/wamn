# Docs audit

This page is the 2026-10-24 read-only audit of docs/ (except docs/history), the repository README, CLAUDE.md and the directory READMEs, at main 10f06916f (platform 0.1.105). Six agents checked each claim against the source. Nothing was changed and no bead was filed. Citations are file:line in the repository or a commit.

Totals: 201 findings, 54 high. High means that an agent who follows the doc writes wrong code or runs a wrong command.

## 1. Decisions for the owner

The platform configuration plan (docs/plan/platform-config.md, here PC) is the newest rule on its subjects. Other docs still state older rules, and the plan itself disagrees with built code in places. Agents need a ruling on each item before they edit the docs.

The wamn-sync plan (docs/plan/wamn-sync.md) is the newest plan, and its subject is mostly outside the platform schema. Items 8 and 9 are the only places where it touches PC. The goal for them is one consistent rule in both plans, not a choice of the older text.

1. Old connection model. integrations.md:135-136, :187, :190 and platform-deploy.md:70-71 (R21, R22) say that credentials are copied into each environment, that account data lives in `wamn_integration.setting`, that each application gets one service principal, and that `environment.k` holds values. PC:29, :36, :66, :100-111, :280-287 replace all of this. The KCL schema crates/schema/generator/kcl/environment/environment.k:11-31 still matches R21. Recommendation: PC wins, and R21, R22 and integrations.md §3-§4 are rewritten to point at PC.
2. Tenant floor. PC:31 says that no platform table has a tenant column. But `role`, `user_role`, `permission`, `api_key` and `environment` keep `tenant_id`, a row policy and two-column foreign keys (deploy/sql/app-schema.sql:245-350, :403-428). data-access.md:748 states the code. Recommendation: rule whether the 0031 org-boundary ruling extends to these five tables.
3. `relations` in wamn.k. PC:77, :115 add a `relations` key, but R9 derives relations and the manifest says no schema has that key (crates/schema/generator/kcl/manifest/manifest.k:11-13). Recommendation: keep R9 (derive), and fix the PC example.
4. `user` shape. PC:35, :54 use `name` and `active`. The code uses `display_name` and `status IN ('active','disabled','invited')` (app-schema.sql:200-215). Recommendation: the code shape.
5. `setting` readable by the application. PC:43-44, :364 say no, but the code grants SELECT to wamn_app (app-schema.sql:718-725) and the numbering renderer reads the `timezone` row. PC:369 says `scope_id uuid`, but the code uses text. Recommendation: the code (readable, text).
6. Counter set. PC:231, :235, :237 name `set_counter(application, ...)`, allow a reset of a `per=` allocator, and say a set uses the bump lock and writes the actor to the write log. The code is `number_counter.set`, refuses any `per=` counter (numbering/set.rs:17-18), uses `platform.set_counter`, and the write log has no actor column. Recommendation: the code.
7. Peek semantics. PC:172, :187 describe a preview of the next number. `peek-number` returns the last number issued, without a lock (wamn-postgres-0.4/package.wit:212-218). Recommendation: the code.
8. One active role. PC:353-359 has one active role for each application in a session. platform-ui.md:44, :63-75 keep a union of roles, and wamn-sync.md:207, :513 say active role resolution is already built. The code derives a role only when the caller holds exactly one (claims.rs:94-106). Recommendation: one active role in both plans, and wamn-sync.md names it as PC C6 work instead of built behavior.
9. Sequence as record id. wamn-sync.md:462 (newer) says that every synced record id is a client-generated UUIDv7 and a sequence is never a record id. PC:225-227 allows `number = "<sequence>"` on the key field. The two rules meet only on a model that is synced and numbers its key. Recommendation: keep the wamn-sync rule for synced models, and PC states that a synced model cannot number its key field, so that the generator refuses it. Resolved 2026-10-25 (wamn-rbns9.3): no model numbers its key field, because every record id is client-generated, and the build refuses it.
10. Node and connector pins. workflows.md:25 (R2), :116 pin node versions for each environment. PC:108, :120 pin connectors for each application under `applications.<app>.connectors`. Recommendation: rule one, and tell the workflows lane, which adds the `applications` map now (wamn-o763w.4).
11. Service principals. workflows.md:56 keeps `materialize_application_principals` unchanged. PC:491 (C3) migrates them to one principal for each connection. Recommendation: PC, and workflows.md says C3 changes it.
12. Org-wide name uniqueness. PC says connection and external system names are unique for each org, but each database holds one (org, project, environment). Recommendation: name the control-plane table that enforces it.
13. Dependencies. PC:12, :450, :481, :496 still wait on A5, but `begin-operation` exists (wamn-operation-0.1/package.wit:47-55). PC:114 refuses a target type in env apply under a "now" row, but the C2 ruling defers that refusal to C10. Recommendation: drop the A5 tag, and move the target refusal to C10.
14. Terms. PC:20 promises one meaning for each word, but "connector", "binding" and "mode" are never defined. "Integration", "target", "operation", "transaction" and "connection" each have two to four meanings in PC (agent 1, finding 34). integrations.md:132-135 and data-access.md:700-703 also use "integration" for an application. Recommendation: one definition each in PC §Mechanism, and a rename of the other uses.
15. Archive. Twenty of the 28 plans have no unbuilt work. Their built rules must move to docs/architecture before the plans are archived. Two rules exist only in platform-deploy.md:9-20 (rule 5, read-only main checkout, and rule 6, the version bump). Recommendation: move these two rules to CLAUDE.md or docs/operations/building.md, then archive the plans.

## 2. Built work that is described only in a plan

An agent who reads docs/architecture does not learn about these items.

- Numbering (wamn-83eth.1-.6): the renderer, the pattern tokens, `next-number` and `peek-number`, the wamn.k fields `sequences`, `number`, `role = "actor"` and `targets`, the build refusals, `number_counter.set`, and env apply creating the `platform.number_sequence` rows. These are in PC:124-238 only. data-access.md:145-178 (manifest map), :216-221 (generated files), :263-272 (statements) and :855-861 (env apply) omit them.
- The C1 platform schema: migrations 0024, 0025, 0027, 0028 and 0031, and platform_grants_live.rs. data-access.md:742, :747 and database-tests.md:86 point back to the plan.
- Workflows Epic 1 (828fdfa67, b12d04d09, 0cddfc25e): `catalog.workflows`, `author_workflow`, enable and disable, the workflow user, `workflow.start`, and the manual run columns. These are in workflows.md:42-71 only, and that plan still calls them unbuilt.
- The release chart and `env apply`, rollback and delete: platform-deploy.md §9.2 and §10-§12. deployment.md:5, :171, :537 and gcp.md:427, :1157 cite the plan.
- Org membership, the admin hierarchy, revocation and scoped deactivation: platform-ui.md §2.7-§2.11.
- The edge SQLite intent store (3d56f2a05). execution.md:251 links to the frozen history page.
- `wamn-ctl upgrade-schema` (upgrade_schema.rs). overview.md:66 says schema installation targets fresh databases only.
- The `workflow` service user (project-state/src/lib.rs:214-222). naming.md:63-73 does not list it.

## 3. High findings by area

### Architecture

- execution.md:414 and wamn-postgres package.wit:206: say numbering keys on the `app.application` claim. Since 5f996c73f (wamn-9e4ks) it uses host state.
- execution.md:472: the executor reads `tenant_id` from `platform."user"`. 0031 dropped that column (sql.rs:747).
- execution.md:88, :94-126: say no permission row grants a host route, and the route table omits `number_counter/set` and `workflow/start`. `HostRouteAuthority::Granted` exists (host_route.rs:207-211, :298-310).
- execution.md:438: `wamn-ctl workflow start` calls the database with project-admin authority. It calls the host route (b12d04d09). execution.md:1003 says so, so the page contradicts itself.
- data-access.md:542-544: only trigger functions read `app.user_id`. Generated create, update and delete statements read it too (8dba42830).
- data-access.md:421-427: nine refusal variants. wamn:operation@0.1.0 adds `authorize`, `finish-read` and `reads-claim-nothing`.
- components.md:81-84: says the composed entry does not serve. Publish records `manifest.composed_entry`, and the runtime and the edge serve it.

### Operations

- running-tests.md:1006: runs `queue::automation_live::` in wamn-execution-host without `--ignored`. The module moved to wamn-workflow (1954a8bf8), and its main cases are ignored.
- running-tests.md:325-330: `--skip stages::` and "five staged Receiving tests". The module is deleted (b8199fb4c), and the anchor is broken.
- gcp.md:731, :736, :749, :784: Receiving 2.2.5. It is 2.2.6.
- gcp.md:329-330: says `platform apply` stops at 4.19.1. It also runs 4.20 and 4.21 (install.rs:225-262).
- web-upload.md:56, :134: says the chart version is compiled into `wamn`. It is read at run time (dc35a922c, wamn-6qo7v).

### Testing and READMEs

- apps/netsuite/README.md:149, :195-200 and tools/netsuite/live.sh:73-76: pass `workflow start` flags that Epic 1 removed (workflow_verbs.rs:142-156). The owner command fails.
- application-tests.md:92-93: describes a dev command test on Receiving. dev_command.rs checks only `--help` and uses the overlay fixture.

### Plans other than PC

- package-upgrade.md:117-135, :289-300, :395-429, :574, :691-736, :781-797: use deleted verbs (`push-component`, `prepare-release`, `qualify-release`, `select-release`, `deploy-release`) and say that qualification reads pods. The code uses `env apply`, `--predecessor-release` and contract.rs.
- platform-deploy.md:351, :415-439, :506: cite files and a head table that are gone.
- environment-teardown.md:51-68: `delete-project-env --confirm`. The verb is `env delete <coordinate> [--data]`.
- workflows.md:26, :55 and workflow-feature.md:54, :100: `queued_service_caller` and the old `list` meaning. The code has `queued_run_caller`, `list` for workflows and `runs` for runs.
- perf-arch.md:36-57, :125-146, :258-282: the pre-A5 request path, router taps (deleted in 65de376c5) and shared row triggers (replaced in c64f57fd2).
- wamn-sync.md:75-84, :146, :197, :630: `row_version` (the bindings say `rowVersion`, wamn-7e86p) and a per-model version table that does not exist.
- integrations.md:21-28, :140, :152: says only blobstore is provisionable, and that no scheduler, HMAC or OAuth exists. All exist, and Epic 5 is closed.
- manifest-authoring.md:132, :136, :206: `generated/wamn.json` is checked in. It left git at 2f7271361.
- observability.md:3, :143-210: Parquet output. operations/observability.md says gzip OTLP JSON.
- trx-pkg-plan.md:78-106, :176: `wamn:postgres` 0.3 and an open `client` hole. Only 0.4 exists, and the deny row landed.
- platform-ui-app.md:7, :31, :110: Solid rc.14 and wfe 45faa4aed. The code has rc.18 and fdeaacc.
- platform-install.md:31: the verb runs two gcloud commands. pools.rs (wake and sleep) adds more.
- operator-ui.md:91: TypeScript generation is deferred. client_ts.rs exists.

## 4. Medium and low findings, grouped

- Status in docs. docs/README.md:3, :8 and docs/plan/README.md give each plan a done, live or deferred status. Five of those entries are wrong (wamn-lcaxg, wamn-z7j99, wamn-whdih, wamn-n8q9m.20, wamn-stxy9). Ten plans and gcp.md:1167, building.md:439-455 and database-tests.md:45-46 also carry status. Beads owns status.
- Missing README entries. docs/plan/README.md has no entry for platform-config.md, workflows.md, observability.md or perf-arch.md. The repository README omits tools/, apps/netsuite, apps/platform_ui and the two platform fixtures. web/README.md omits host-route-clients.mjs and sync-bench.
- kache. No doc names kache. .cargo/config.toml:7-28 and Dockerfile:44-48 still describe sccache, and the config comment names a `root-recipe` stage that does not exist.
- Image counts. gcp.md:427, running-tests.md:589, :599 say four or six images. The source has five images and two role components.
- Stale commands and flags. development-loop.md:296-298 (PAT flags on `wamn dev`), benchmark.md:68 (`--lines` on local.sh), queued-automation.md:45, :61 and edge.md:87-128 (missing required flags), deployment.md:163 (`provision-org`), gcp.md:444-446 (no `--cluster`), gcp.md:896, :1208 (`operator-pat.json` that no step writes).
- Stale behavior. deployment.md:236-237 (publish-release reuses the pushed entry, wamn-uhjx2), web-upload.md:105-142 (a package with no web build is skipped, wamn-21cms), gcp.md:195, :302, :429 (work directory reuse, 4.10 skip, stamp-only early return), building.md:157, :408 (build script hashing, separate edge-web stage), running-tests.md:115-146 (target paths that disagree with lane-start).
- Stale facts in PC. Thirteen medium items: grant counts, `callable = True`, `FieldRole`, the timezone scope, the refusal list, assumption numbers, the C-epic order, the canary file names, and "Open before C1 starts".
- Stale facts in architecture. wfe pin (execution.md:920), extensions (data-access.md:64 omits pg_stat_statements), the manual run caller (execution.md:395-399, :447), "authored in wamn.json" (four places, it is wamn.k), the Receiving version example, and links into frozen history (overview.md:120, apps/edge_*/README.md:1, operations/edge.md:85, deployment.md:289).
- Stale facts in other plans. Pins and paths in cdc-reader-slot, component-pull and nats-outage. Counts in environment-teardown. Gone paths in generated-operations, table-adv-data, table-adv-ui and request-round-trips. Write log scope in write-log.md:115, :127. The CDN path in web-deployment.md:154-158.
- Broken links. delivery.md:8, application-tests.md:8, deployment.md:320, execution.md:67, and three links in apps/wamn_receiving/web/README.md:33, :44, :63 with one `../` too many.
- Archive links. data-access.md:270, components.md:54, platform-ui.md:62, :90 and deploy/gcp/operator/mint-pat.yaml:1 link into plans that are archive candidates.

## 5. Plan status, one line per plan

Live (8): delivery, identity, integrations (Epic 6 and later), nats-outage, package-upgrade (§7.3, §7.4), platform-install (E5), wamn-sync (Epics 2a-5), workflows (Epics 2-4). platform-config is also live.

Archive candidates (20): cdc-reader-slot, component-pull, environment-teardown, generated-operations, manifest-authoring, observability, operator-image, operator-ui, perf-arch, platform-deploy, platform-ui-app, platform-ui, request-round-trips, routes-router, table-adv-data, table-adv-ui, trx-pkg-plan, web-deployment, workflow-feature, write-log.

## 6. Not checked

- No test, build or live run. Printed output, timings and GCP state are not verified.
- Bead states partly came from .beads/issues.jsonl, because `bd show` stalled under load.
- Parts of execution.md (632-795), data-access.md (85-127, 350-435, 674-696), capabilities.md, native-alignment.md, platform-deploy R1-R22 and §5-§8 were not checked line by line.
- The `applications` map of wamn-o763w.4 is not on origin/lane/workflows-2 yet, so it was not compared.
