# R6 SQL scope at `6b4db53e1`

**F** marks observed source. **I** marks a proposed spelling or an implication. Counts in the first table are matching source lines for the qualified name in tracked SQL and Rust, including comments. They exclude unqualified references and generated names.

## 1. Tables

**F.** The checked-in platform SQL declares **57 base tables, 25 plural**. R6 says 54 and 24, but its own plural roster contains 25. The 57 comprise registry 18, identity 13, catalog 3, provisioning 1, platform 19, `wamn_run` 2 and `wamn_history` 1. `platform.environment` is singular and still requires the short form. See the declarations below and `docs/plan/review-restructure.md:142`.

**I.** The platform pass has **25 definite renames and one drop**. Five additional singular names are flagged for abbreviation expansion, which would bring the count to **30 renames and one drop**. `registry.retired_project_envs` is the drop ordered by R6. `docs/plan/review-restructure.md:146`, `docs/plan/review-restructure.md:156`

| Old name (F) | New name or disposition (I) | Hits and source (F) |
|---|---|---|
| `registry.integration_type` | Unchanged | 11 · `deploy/sql/system-schema.sql:71`, `deploy/sql/apply-grants.sql:14` |
| `registry.integration_type_version` | Unchanged | 10 · `deploy/sql/system-schema.sql:78`, `deploy/sql/system-schema.sql:968` |
| `registry.auth_type` | `registry.authentication_type` (unsure: catalog mixes authentication methods with OAuth grants) | 7 · `deploy/sql/system-schema.sql:89`, `deploy/sql/system-schema.sql:1029` |
| `registry.integration_type_auth` | `registry.integration_type_authentication` (unsure: same terminology as `auth_type`) | 5 · `deploy/sql/system-schema.sql:95`, `deploy/sql/system-schema.sql:1049` |
| `registry.target` | Unchanged | 11 · `deploy/sql/system-schema.sql:101`, `deploy/sql/system-schema.sql:940` |
| `registry.meta` | `registry.platform_configuration` (unsure: `meta` may be treated as an established name) | 10 · `deploy/sql/system-schema.sql:123`, `deploy/sql/apply-grants.sql:15` |
| `registry.orgs` | `registry.org` | 25 · `deploy/sql/system-schema.sql:146`, `deploy/sql/apply-grants.sql:11` |
| `registry.projects` | `registry.project` | 23 · `deploy/sql/system-schema.sql:187`, `deploy/sql/apply-grants.sql:16` |
| `registry.environments` | `registry.env` | 40 · `deploy/sql/system-schema.sql:601`, `deploy/sql/apply-grants.sql:33` |
| `registry.platform_event_type` | Unchanged | 3 · `deploy/sql/system-schema.sql:618`, `deploy/sql/system-schema.sql:648` |
| `registry.platform_event_outcome` | Unchanged | 3 · `deploy/sql/system-schema.sql:632`, `deploy/sql/system-schema.sql:649` |
| `registry.environment_projects` | `registry.env_project` | 49 · `deploy/sql/system-schema.sql:686`, `deploy/sql/apply-grants.sql:17`, `crates/environment/src/environment_sql.rs:76` |
| `registry.environment_applications` | `registry.env_app` | 10 · `deploy/sql/system-schema.sql:711`, `deploy/sql/apply-grants.sql:23`, `crates/environment/src/registry/configuration.rs:171` |
| `registry.environment_integrations` | `registry.env_integration` | 10 · `deploy/sql/system-schema.sql:728`, `deploy/sql/apply-grants.sql:24` |
| `registry.environment_application_connections` | `registry.env_app_connection` | 8 · `deploy/sql/system-schema.sql:738`, `deploy/sql/apply-grants.sql:24`, `crates/environment/src/registry/configuration.rs:173` |
| `registry.retired_project_envs` | Drop | 6 · `deploy/sql/system-schema.sql:803`, `deploy/sql/system-schema.sql:823`, `deploy/sql/apply-grants.sql:22` |
| `registry.event_readers` | `registry.event_reader` | 10 · `deploy/sql/system-schema.sql:865`, `deploy/sql/apply-grants.sql:26` |
| `registry.capture_gap` | Unchanged | 11 · `deploy/sql/system-schema.sql:892`, `deploy/sql/apply-grants.sql:27` |
| `identity.principals` | `identity.principal` | 60 · `deploy/sql/system-schema.sql:241`, `deploy/sql/apply-grants.sql:13` |
| `identity.project_roles` | `identity.project_role` | 18 · `deploy/sql/system-schema.sql:281`, `deploy/sql/apply-grants.sql:12` |
| `identity.org_memberships` | `identity.org_membership` | 31 · `deploy/sql/system-schema.sql:307`, `deploy/sql/apply-grants.sql:31` |
| `identity.org_roles` | `identity.org_role` | 16 · `deploy/sql/system-schema.sql:358`, `deploy/sql/apply-grants.sql:12` |
| `identity.password_credentials` | `identity.password_credential` | 11 · `deploy/sql/system-schema.sql:382`, `crates/identity/src/identity.rs:46` |
| `identity.password_tokens` | `identity.password_token` | 16 · `deploy/sql/system-schema.sql:399`, `deploy/sql/system-schema.sql:414` |
| `identity.password_logins` | `identity.password_login` | 25 · `deploy/sql/system-schema.sql:422`, `deploy/sql/system-schema.sql:441` |
| `identity.renewal_credentials` | `identity.renewal_credential` | 13 · `deploy/sql/system-schema.sql:448`, `deploy/sql/system-schema.sql:457` |
| `identity.session_keys` | `identity.session_key` | 14 · `deploy/sql/system-schema.sql:491`, `deploy/sql/system-schema.sql:557` |
| `identity.nats_keys` | `identity.nats_key` | 15 · `deploy/sql/system-schema.sql:513`, `deploy/sql/system-schema.sql:544` |
| `identity.session_signing_state` | Unchanged | 12 · `deploy/sql/system-schema.sql:549`, `crates/identity/src/identity/session_keys.rs:47` |
| `identity.project_env_memberships` | `identity.project_env_membership` | 18 · `deploy/sql/system-schema.sql:758`, `deploy/sql/apply-grants.sql:25` |
| `identity.password_attempts` | `identity.password_attempt` | 7 · `deploy/sql/system-schema.sql:929`, `crates/identity/src/identity/issuer.rs:79` |
| `provisioning.platform_event` | Unchanged | 16 · `deploy/sql/system-schema.sql:644`, `deploy/sql/apply-grants.sql:38` |
| `catalog.packages` | `catalog.package` | 14 · `deploy/sql/catalog-schema.sql:7`, `crates/environment/src/apply_package/package_version.rs:11` |
| `catalog.connection_requirement` | Unchanged | 4 · `deploy/sql/catalog-schema.sql:204`, `crates/environment/src/release_requirements.rs:37` |
| `catalog.workflows` | `catalog.workflow` | 24 · `deploy/sql/catalog-schema.sql:215`, `crates/plugin/src/workflow.rs:78` |
| `platform."user"` | Unchanged, quoted | 56 · `deploy/sql/app-schema.sql:93`, `deploy/sql/app-schema.sql:136` |
| `platform.pat` | `platform.personal_access_token` (unsure: PAT is an established product credential name) | 16 · `deploy/sql/app-schema.sql:142`, `crates/identity/src/identity/application/service.rs:95` |
| `platform.role` | Unchanged | 30 · `deploy/sql/app-schema.sql:170`, `crates/authentication/src/administration/sql.rs:66` |
| `platform.user_role` | Unchanged | 30 · `deploy/sql/app-schema.sql:191`, `crates/authentication/src/administration/sql.rs:21` |
| `platform.permission` | Unchanged | 27 · `deploy/sql/app-schema.sql:218`, `crates/authentication/src/administration/sql.rs:72` |
| `platform.environment` | `platform.env` | 17 · `deploy/sql/app-schema.sql:247`, `crates/authentication/src/administration/sql.rs:25`, `crates/authentication/src/authorization/project_reads.rs:18` |
| `platform.write_log` | Unchanged | 27 · `deploy/sql/app-schema.sql:326`, `crates/environment/src/audit_retention.rs:28` |
| `platform.file` | Unchanged | 41 · `deploy/sql/app-schema.sql:362`, `crates/plugin/src/file_catalog.rs:55` |
| `platform.number_sequence` | Unchanged | 15 · `deploy/sql/app-schema.sql:403`, `deploy/sql/app-schema.sql:540` |
| `platform.number_counter` | Unchanged | 11 · `deploy/sql/app-schema.sql:418`, `deploy/sql/app-schema.sql:544` |
| `platform.setting` | Unchanged | 12 · `deploy/sql/app-schema.sql:434`, `apps/crates/guest/src/postgres_statements/numbering.rs:11` |
| `platform.external_system` | Unchanged | 21 · `deploy/sql/catalog-schema.sql:60`, `apps/rest_consumer_fixture/migrations/0001_initial.sql:5` |
| `platform.connection` | Unchanged | 43 · `deploy/sql/catalog-schema.sql:65`, `deploy/sql/app-schema.sql:365` |
| `platform.credential` | Unchanged | 15 · `deploy/sql/catalog-schema.sql:82`, `deploy/sql/catalog-schema.sql:132` |
| `platform.oauth_token` | Unchanged | 11 · `deploy/sql/catalog-schema.sql:93`, `deploy/sql/catalog-schema.sql:200` |
| `platform.oauth_authorization` | Unchanged | 8 · `deploy/sql/catalog-schema.sql:111`, `deploy/sql/catalog-schema.sql:200` |
| `platform.connection_generation` | Unchanged | 19 · `deploy/sql/catalog-schema.sql:123`, `deploy/sql/catalog-schema.sql:143` |
| `platform.tx_identity` | `platform.transaction_identity` (unsure: `tx` expansion affects the shared authority row type) | 57 · `deploy/sql/record-history.sql:27`, `deploy/sql/record-history.sql:70`, `crates/postgres/src/transaction_identity.rs:47` |
| `platform.login_principal` | Unchanged | 23 · `deploy/sql/record-history.sql:58`, `crates/postgres/src/migrations.rs:106` |
| `wamn_run.runs` | `wamn_run.run`, plus the unqualified `runs` | 19 qualified · `deploy/sql/run-state.sql:78`, `deploy/sql/run-queue.sql:53`, `crates/plugin/src/run_store.rs:41` |
| `wamn_run.run_queue` | Unchanged | 10 · `deploy/sql/run-queue.sql:36`, `deploy/sql/run-queue.sql:67` |
| `wamn_history.pruning_watermarks` | `wamn_history.pruning_watermark` | 7 · `deploy/sql/record-history.sql:132`, `deploy/sql/record-history-app-grants.sql:16`, `crates/codegen/src/sql.rs:280` |

### Authored, fixture and runtime tables outside that count

**F.** There are **28 additional fixed base-table declarations in `.sql` files**, listed here. **I.** Their table names are all unchanged. Counts are qualified-name source lines, using the same convention as above.

| Table (unchanged) | Hits and source (F) |
|---|---|
| `receiving.quality_inspection` | 3 · `apps/client_acme_receiving/migrations/0001_initial.sql:15`, `apps/client_acme_receiving/history/0002_record_history.sql:35` |
| `edge_samples.sample` | 2 · `apps/edge_samples/migrations/0001_initial.sql:1`, `apps/edge_samples/history/0002_record_history.sql:26` |
| `inventory.widget_maker` | 6 · `apps/platform_fixture/migrations/0001_initial.sql:1`, `apps/platform_fixture/history/0002_record_history.sql:436` |
| `inventory.feed_order` | 5 · `apps/platform_fixture/migrations/0001_initial.sql:8`, `apps/platform_fixture/history/0002_record_history.sql:273` |
| `inventory.feed_item` | 5 · `apps/platform_fixture/migrations/0001_initial.sql:13`, `apps/platform_fixture/history/0002_record_history.sql:146` |
| `inventory.feed_line` | 6 · `apps/platform_fixture/migrations/0001_initial.sql:18`, `apps/platform_fixture/history/0002_record_history.sql:217` |
| `inventory.widget` | 17 · `apps/platform_fixture/migrations/0001_initial.sql:35`, `apps/platform_fixture/history/0002_record_history.sql:338` |
| `inventory.widget_tag` | 5 · `apps/platform_fixture/migrations/0001_initial.sql:48`, `apps/platform_fixture/history/0002_record_history.sql:499` |
| `inventory.numbered_label` | 3 · `apps/platform_fixture/migrations/0001_initial.sql:54`, `apps/platform_fixture/history/0002_record_history.sql:308` |
| `inventory.counter` | 7 · `apps/platform_fixture/migrations/0001_initial.sql:65`, `apps/platform_fixture/history/0002_record_history.sql:27` |
| `rest_consumer_fixture.item` | 4 · `apps/rest_consumer_fixture/migrations/0001_initial.sql:1`, `crates/codegen/tests/provenance_live.rs:25` |
| `wamn_integration.setting` | 1 · `apps/wamn_integration/migrations/0001_initial.sql:1` |
| `wamn_integration.log` | 1 · `apps/wamn_integration/migrations/0001_initial.sql:5` |
| `receiving.item` | 10 · `apps/wamn_receiving/migrations/0001_initial.sql:1`, `apps/wamn_receiving/history/0002_record_history.sql:57` |
| `receiving.location` | 8 · `apps/wamn_receiving/migrations/0001_initial.sql:6`, `apps/wamn_receiving/migrations/0001_initial.sql:80` |
| `receiving.supplier` | 11 · `apps/wamn_receiving/migrations/0001_initial.sql:11`, `apps/wamn_receiving/history/0002_record_history.sql:321` |
| `receiving.purchase_order` | 24 · `apps/wamn_receiving/migrations/0001_initial.sql:17`, `apps/client_acme_receiving/migrations/0001_initial.sql:1` |
| `receiving.purchase_order_line` | 12 · `apps/wamn_receiving/migrations/0001_initial.sql:34`, `apps/wamn_receiving/history/0002_record_history.sql:262` |
| `receiving.receipt` | 8 · `apps/wamn_receiving/migrations/0001_initial.sql:56`, `apps/client_acme_receiving/migrations/0001_initial.sql:19` |
| `receiving.receipt_line` | 3 · `apps/wamn_receiving/migrations/0001_initial.sql:69`, `tests/receiving-tests/fixtures/receiving-seed.sql:40` |
| `wms.product` | 10 · `apps/wamn_wms/migrations/0001_initial.sql:1`, `apps/wamn_wms/history/0002_record_history.sql:324` |
| `wms.location` | 9 · `apps/wamn_wms/migrations/0001_initial.sql:9`, `apps/wamn_wms/history/0002_record_history.sql:64` |
| `wms.packaging` | 15 · `apps/wamn_wms/migrations/0001_initial.sql:18`, `apps/wamn_wms/history/0002_record_history.sql:171` |
| `wms.packaging_quantity` | 9 · `apps/wamn_wms/migrations/0001_initial.sql:39`, `apps/wamn_wms/history/0002_record_history.sql:296` |
| `wms.inventory_transaction` | 9 · `apps/wamn_wms/migrations/0001_initial.sql:57`, `apps/wamn_wms/history/0002_record_history.sql:35` |
| `file_fixture.document` | 10 · `crates/environment/tests/support/file_package/migrations/0001_documents.sql:2`, `crates/codegen/tests/file_reference_live.rs:72` |
| `digest_extra.entry` | 1 · `tests/journeys/fixtures/installed_digest/0003_digest_schema.sql:1` |
| `public.receipt_slot` | 6 · `xtask/bench/pg-minimum/receipt-slots.sql:8`, `xtask/bench/pg-minimum/receipt-slots.sql:7` |

**F.** Rust SQL declares **29 additional fixed base-table spellings**, for **114 fixed spellings** across SQL files and Rust before generated histories and schema-dependent names. **I.** Three additional table identifiers need singularization, for **28 definite fixed-table renames across the inventory** plus the one drop. Seven more qualified names change if the fixture schema `application` becomes `app`. The counts below are declarations.

| Rust-declared table (F) | Disposition (I) | Declarations and source (F) |
|---|---|---|
| `app.orders` | `app."order"` | 3 · `crates/codegen/tests/feed_live.rs:118`, `crates/codegen/tests/feed_live.rs:159`, `crates/codegen/tests/feed_live.rs:195` |
| SQLite `claims` | `claim` | 1 · `services/edge/src/store.rs:15`, `services/edge/src/store.rs:148` |
| SQLite `samples` | `sample` | 1 · `services/edge/src/samples.rs:10`, `services/edge/src/store.rs:188` |
| `application.item` | `app.item` (unsure: isolated catalog fixture schema) | 9 · `crates/codegen/tests/catalog_live.rs:18`, `crates/codegen/tests/catalog_live.rs:92`, `crates/codegen/tests/catalog_live.rs:157` |
| `application.dock` | `app.dock` (unsure: isolated fixture schema) | 4 · `crates/codegen/tests/catalog_live.rs:76`, `crates/codegen/tests/catalog_live.rs:84` |
| `application.unused_data` | `app.unused_data` (unsure: isolated fixture schema) | 1 · `crates/codegen/tests/catalog_live.rs:21` |
| `application.other` | `app.other` (unsure: isolated fixture schema) | 1 · `crates/codegen/tests/catalog_live.rs:158`, `crates/codegen/tests/catalog_live.rs:160` |
| `application.parent` | `app.parent` (unsure: isolated fixture schema) | 1 · `crates/codegen/tests/catalog_live.rs:218`, `crates/codegen/tests/catalog_live.rs:219` |
| `application.selected` | `app.selected` (unsure: isolated fixture schema) | 1 · `crates/codegen/tests/catalog_live.rs:219`, `crates/codegen/tests/catalog_live.rs:220` |
| `application.child` | `app.child` (unsure: isolated fixture schema) | 1 · `crates/codegen/tests/catalog_live.rs:220` |
| `files_fixture.attachment` | Unchanged | 1 · `crates/codegen/src/build_database.rs:411` |
| `key_row` | Unchanged | 1 · `crates/codegen/tests/codegen_sql_live.rs:65` |
| `page_row` | Unchanged | 1 · `crates/codegen/tests/codegen_sql_live.rs:143` |
| `app.item` | Unchanged | 1 · `crates/codegen/tests/feed_live.rs:117`, `crates/codegen/tests/feed_live.rs:119` |
| `app.line` | Unchanged | 3 · `crates/codegen/tests/feed_live.rs:120`, `crates/codegen/tests/feed_live.rs:158`, `crates/codegen/tests/feed_live.rs:194` |
| `platform.other` | Unchanged | 2 · `crates/codegen/tests/file_reference_live.rs:108`, `crates/codegen/tests/provenance_live.rs:71` |
| `gadget` | Unchanged | 1 · `crates/codegen/tests/generation.rs:22` |
| `inventory.gadget` | Unchanged | 1 · `crates/codegen/tests/numbering.rs:536` |
| `public.gadget` | Unchanged | 1 · `crates/codegen/tests/numbering.rs:620` |
| `platform.record_source` | Unchanged | 1 · `crates/connection/tests/integration_references.rs:15` |
| `fixture_app.item` | Unchanged | 1 · `crates/environment/tests/apply_package_live.rs:9`, `crates/environment/tests/apply_package_live.rs:78` |
| `public.migration_effect` | Unchanged | 1 · `crates/environment/tests/migrations_live.rs:49`, `crates/environment/tests/migrations_live.rs:107` |
| `registry.migration_probe` | Unchanged | 1 · `crates/environment/tests/platform_migrations_live.rs:60`, `crates/environment/tests/platform_migrations_live.rs:92` |
| Temporary `spelled` | Unchanged | 2 · `crates/environment/tests/project_schema.rs:510`, `crates/environment/tests/project_schema.rs:561` |
| `public.history_probe` | Unchanged | 1 · `crates/environment/tests/project_schema.rs:604`, `crates/environment/tests/project_schema.rs:646` |
| `public.keyless_history_probe` | Unchanged | 1 · `crates/environment/tests/project_schema.rs:736` |
| `overlap_probe.appointment` | Unchanged | 1 · `crates/environment/tests/provision.rs:5` |
| `inventory.item` | Unchanged | 1 · `crates/release/src/release_source.rs:278` |
| `wamn_grant_probe` | Unchanged | 1 temporary declaration, 1 schema-dependent form · `test-support/grant-tests/tests/common/mod.rs:106`, `test-support/grant-tests/tests/common/mod.rs:112` |

| Runtime table or pattern (F) | Disposition (I) | Source (F) |
|---|---|---|
| `<model_schema>.wamn_entities` | `<model_schema>.wamn_entity` | 1 generator · `crates/environment/src/capture_sql.rs:116`, `services/cdc-reader/src/replication.rs:403`, `crates/package/src/manifest.rs:20` |
| `<model_schema>.wamn_cdc_exclusions` | `<model_schema>.wamn_change_data_capture_exclusion` (unsure: `cdc` may be retained as a subsystem term; singularization is definite) | 1 generator · `crates/environment/src/capture_sql.rs:71`, `services/cdc-reader/src/replication.rs:404`, `crates/package/src/manifest.rs:20` |
| `registry._sqlx_migrations` | `registry._sqlx_migration` (unsure: configurable SQLx convention and system completion marker) | 1 target · `crates/postgres/src/migrations.rs:112`, `crates/platform-install/src/migration_history.rs:6`, `deploy/sql/apply-grants.sql:62` |
| `platform._sqlx_migrations` | `platform._sqlx_migration` (unsure: same SQLx convention) | 1 target · `crates/environment/src/platform_migrations.rs:19`, `crates/postgres/src/migrations.rs:99`, `crates/postgres/src/migrations.rs:112` |
| `wamn_history.migrations_<40 hex>` | `wamn_history.migration_<40 hex>` | 1 generator · `crates/environment/src/project_migration.rs:4`, `crates/environment/src/environment_sql_effects.rs:515`, `crates/environment/src/apply_package.rs:155` |
| Legacy SQLite `intents` | Unchanged (unsure: legacy-file rejection sentinel, with no current declaration) | 1 probe · `services/edge/src/store.rs:105`, `services/edge/src/store.rs:110` |

## 2. Columns and SQL parameters

**F.** The definite column inventory is seven `application` columns and two `environment` columns. Existing `org` and `env` columns already use the short forms. The concrete owners below use the bare words. `identity.has_project_access` also has a long-form SQL parameter. `deploy/sql/system-schema.sql:602`, `deploy/sql/system-schema.sql:715`, `deploy/sql/system-schema.sql:1187`, `deploy/sql/catalog-schema.sql:216`, `deploy/sql/run-state.sql:81`

| Owning table or function (F) | Old → new (I) | Source (F) |
|---|---|---|
| `registry.environment_applications` | `application` → `app` | `deploy/sql/system-schema.sql:715`, `deploy/sql/system-schema.sql:721` |
| `registry.environment_application_connections` | `application` → `app` | `deploy/sql/system-schema.sql:742`, `deploy/sql/system-schema.sql:745`, `deploy/sql/system-schema.sql:747` |
| `provisioning.platform_event` | `application` → `app` | `deploy/sql/system-schema.sql:658`, `crates/platform-install/src/platform_event.rs:101` |
| `catalog.workflows` | `application` → `app` | `deploy/sql/catalog-schema.sql:217`, `deploy/sql/catalog-schema.sql:237`, `deploy/sql/catalog-schema.sql:272` |
| `catalog.workflows` | `environment` → `env` | `deploy/sql/catalog-schema.sql:216`, `deploy/sql/catalog-schema.sql:261`, `deploy/sql/catalog-schema.sql:277` |
| `wamn_run.runs` | `environment` → `env` | `deploy/sql/run-state.sql:81`, `deploy/sql/run-state.sql:159`, `deploy/sql/run-state.sql:182` |
| `platform.file` | `application` → `app` | `deploy/sql/app-schema.sql:364`, `crates/plugin/src/file_catalog.rs:80` |
| `platform.number_sequence` | `application` → `app` | `deploy/sql/app-schema.sql:405`, `deploy/sql/app-schema.sql:413`, `apps/crates/guest/src/postgres_statements/numbering.rs:13` |
| `platform.tx_identity` | `application` → `app` | `deploy/sql/record-history.sql:31`, `deploy/sql/record-history.sql:100`, `crates/postgres/src/transaction_identity.rs:47` |
| `identity.has_project_access` | Parameter `environment` → `env_name`, or use `$3` at the comparisons (unsure: bare `env` collides with visible columns) | `deploy/sql/system-schema.sql:1187`, `deploy/sql/system-schema.sql:1193`, `deploy/sql/system-schema.sql:1198` |

**I.** Other abbreviation candidates, with their owners and contract questions:

| Old → proposed | Owner and source (F) |
|---|---|
| `auth_type` → `authentication_type` (unsure: mixed authentication and OAuth grant terminology) | `registry.integration_type_auth`, `platform.connection` · `deploy/sql/system-schema.sql:97`, `deploy/sql/catalog-schema.sql:69`, `crates/connection/src/project_connections/store.rs:43` |
| `ref` → `reference` (unsure: SQL also emits the credential JSON key `ref`) | `platform.credential` · `deploy/sql/catalog-schema.sql:86`, `crates/connection/src/project_connections/administration.rs:261`, `crates/connection/src/project_connections/administration.rs:366` |
| `ops` → `operations` | `catalog.workflows` · `deploy/sql/catalog-schema.sql:228`, `deploy/sql/catalog-schema.sql:249`, `deploy/sql/catalog-schema.sql:274` |
| `max_projects` → `maximum_projects` | `registry.meta` · `deploy/sql/system-schema.sql:127`, `deploy/sql/apply-grants.sql:15`, `crates/platform-install/src/platform_sql.rs:12` |
| `max_attempts` → `maximum_attempts` | `wamn_run.run_queue` · `deploy/sql/run-queue.sql:50`, `crates/plugin/src/run_store/queue.rs:101` |
| `xid` → `transaction_id` (unsure: PostgreSQL transaction-ID terminology, already used as a distinct name for a different representation in history) | `platform.tx_identity` · `deploy/sql/record-history.sql:28`, `deploy/sql/record-history.sql:134`, `crates/plugin/src/operation_sql/write_log.rs:53` |
| `kid` → `key_id` (unsure: SQL column shares the standardized JWT header spelling) | `identity.session_keys` · `deploy/sql/system-schema.sql:493`, `deploy/sql/system-schema.sql:502`, `crates/identity/src/identity/session_keys.rs:96` |
| `active_kid` → `active_key_id` (unsure: same JWT terminology) | `identity.session_signing_state` · `deploy/sql/system-schema.sql:551`, `deploy/sql/system-schema.sql:556`, `crates/identity/src/identity/session_keys.rs:57` |
| `start_lsn` → `start_log_sequence_number` (unsure: PostgreSQL replication vocabulary) | `registry.capture_gap` · `deploy/sql/system-schema.sql:897`, `crates/environment/src/environment_sql.rs:31` |
| `end_lsn` → `end_log_sequence_number` (unsure: PostgreSQL replication vocabulary) | `registry.capture_gap` · `deploy/sql/system-schema.sql:900`, `crates/environment/src/environment_sql.rs:31`, `test-support/grant-tests/tests/system.rs:350` |
| `enable_lsn` → `enable_log_sequence_number` (unsure: PostgreSQL replication vocabulary) | `catalog.workflows` · `deploy/sql/catalog-schema.sql:230`, `deploy/sql/catalog-schema.sql:243`, `crates/plugin/src/workflow/admission.rs:97` |
| `sub` → `subject` (unsure: the SQL column mirrors the JWT `sub` claim) | `platform."user"` · `deploy/sql/app-schema.sql:96`, `crates/authentication/src/authorization/project_reads.rs:75` |
| `graph_json` → `graph`; `input_json` → `input`; `result_json` → `result`; `state_json` → `state`; `deadline_adjustments_json` → `deadline_adjustments` (unsure: JSON is a format name and the suffix distinguishes stored representations) | `catalog.workflows`, `wamn_run.runs` · `deploy/sql/catalog-schema.sql:220`, `deploy/sql/run-state.sql:101`, `deploy/sql/run-state.sql:104` |
| `id` / `*_id`, `relation_oid`, `url`, `http_only`, `sha256`, `line_no` (unsure: identifier, protocol and algorithm vocabulary, or fixture-local spelling; mechanical expansion would reach contracts beyond the short-form change) | `deploy/sql/system-schema.sql:102`, `deploy/sql/system-schema.sql:93`, `crates/environment/src/capture_sql.rs:72`, `deploy/sql/app-schema.sql:369`, `deploy/sql/app-schema.sql:373`, `crates/codegen/tests/numbering.rs:620` |

**F.** Six external SQL files insert the old transaction-identity column, one statement each: `tests/receiving-tests/fixtures/receiving-seed-small.sql:35`, `tests/receiving-tests/fixtures/receiving-seed.sql:37`, `tests/wms-tests/fixtures/wms-seed-small.sql:36`, `tests/wms-tests/fixtures/wms-seed.sql:39`, `xtask/bench/pg-minimum/receipt.sql:24`, `xtask/bench/pg-minimum/update.sql:29`.

## 3. Derived names, stored literals and grants

### Explicit SQL objects

Counts are matching source lines for each exact old identifier. A shared function/trigger spelling appears once below.

| Old → new or disposition (I) | Hits and source (F) |
|---|---|
| `orgs_id_charset_check` → `org_id_charset_check` | 1 · `deploy/sql/system-schema.sql:152` |
| `projects_id_charset_check` → `project_id_charset_check` | 1 · `deploy/sql/system-schema.sql:197` |
| `principals_type_check` → `principal_type_check` | 1 · `deploy/sql/system-schema.sql:255` |
| `principals_subject_check` → `principal_subject_check` | 1 · `deploy/sql/system-schema.sql:257` |
| `principals_email_check` → `principal_email_check` | 5 · `deploy/sql/system-schema.sql:261`, `crates/identity/src/principal.rs:175`, `crates/identity/src/principal.rs:185` |
| `principals_display_name_check` → `principal_display_name_check` | 1 · `deploy/sql/system-schema.sql:266` |
| `principals_platform_principal_check` → `principal_platform_principal_check` | 3 · `deploy/sql/system-schema.sql:218`, `deploy/sql/system-schema.sql:268`, `crates/identity/tests/identity_live.rs:575` |
| `project_roles_role_check` → `project_role_role_check` | 1 · `deploy/sql/system-schema.sql:294` |
| `org_memberships_user_check` → `org_membership_user_check` | 1 · `deploy/sql/system-schema.sql:320` |
| Error constraint `org_memberships_single_org` → `org_membership_single_org` | 2 · `deploy/sql/system-schema.sql:345`, `crates/identity/src/identity/org.rs:37` |
| `org_roles_role_check` → `org_role_role_check` | 1 · `deploy/sql/system-schema.sql:369` |
| `password_tokens_principal_idx` → `password_token_principal_idx` | 1 · `deploy/sql/system-schema.sql:414` |
| `password_logins_principal_idx` → `password_login_principal_idx` | 1 · `deploy/sql/system-schema.sql:441` |
| `password_logins_expiry_idx` → `password_login_expiry_idx` | 1 · `deploy/sql/system-schema.sql:442` |
| `renewal_credentials_login_idx` → `renewal_credential_login_idx` | 1 · `deploy/sql/system-schema.sql:457` |
| Function and trigger `revoke_disabled_password_logins` → `revoke_disabled_password_login` (unsure: plural describes the rows revoked and also embeds the old table name) | 4 · `deploy/sql/system-schema.sql:465`, `deploy/sql/system-schema.sql:474`, `deploy/sql/system-schema.sql:477` |
| `session_keys_retirement_check` → `session_key_retirement_check` | 1 · `deploy/sql/system-schema.sql:503` |
| `project_envs_instance_suffix_check` → `env_project_instance_suffix_check` | 1 · `deploy/sql/system-schema.sql:701` |
| `project_env_memberships_user_check` → `project_env_membership_user_check` | 1 · `deploy/sql/system-schema.sql:773` |
| `project_envs_retire_instance` → drop | 1 · `deploy/sql/system-schema.sql:830` |
| `registry.retire_project_env()` → drop | 3 · `deploy/sql/system-schema.sql:820`, `deploy/sql/system-schema.sql:828`, `deploy/sql/system-schema.sql:832` |
| `registry.keep_environment_target()` → `registry.keep_env_target()` | 3 · `deploy/sql/system-schema.sql:941`, `deploy/sql/system-schema.sql:948`, `deploy/sql/system-schema.sql:950` |
| `environment_target_immutable` → `env_target_immutable` | 1 · `deploy/sql/system-schema.sql:949` |
| `packages_pkey` → `package_pkey` | 1 · `deploy/sql/catalog-schema.sql:18` |
| `packages_package_id_check` → `package_package_id_check` | 1 · `deploy/sql/catalog-schema.sql:19` |
| `packages_package_version_check` → `package_package_version_check` | 1 · `deploy/sql/catalog-schema.sql:21` |
| `packages_artifact_digest_check` → `package_artifact_digest_check` | 1 · `deploy/sql/catalog-schema.sql:27` |
| `packages_schema_guard` → `package_schema_guard` | 1 · `deploy/sql/catalog-schema.sql:49` |
| `packages_delete_immutable` → `package_delete_immutable` | 1 · `deploy/sql/catalog-schema.sql:52` |
| `workflows_pkey` → `workflow_pkey` | 1 · `deploy/sql/catalog-schema.sql:236` |
| `workflows_execution_identity_check` → `workflow_execution_identity_check` | 1 · `deploy/sql/catalog-schema.sql:238` |
| `workflows_request_path_check` → `workflow_request_path_check` | 1 · `deploy/sql/catalog-schema.sql:241` |
| `workflows_enable_lsn_check` → `workflow_enable_lsn_check` (unsure: also follows any column expansion) | 1 · `deploy/sql/catalog-schema.sql:243` |
| `workflows_schedule_check` → `workflow_schedule_check` | 1 · `deploy/sql/catalog-schema.sql:245` |
| `workflows_data_change_check` → `workflow_data_change_check` | 1 · `deploy/sql/catalog-schema.sql:247` |
| `workflows_one_enabled` → `workflow_one_enabled` | 1 · `deploy/sql/catalog-schema.sql:260` |
| `workflows_enabled_request_path` → `workflow_enabled_request_path` | 1 · `deploy/sql/catalog-schema.sql:263` |
| `workflows_controlled_update` → `workflow_controlled_update` | 1 · `deploy/sql/catalog-schema.sql:292` |
| `workflows_delete_immutable` → `workflow_delete_immutable` | 1 · `deploy/sql/catalog-schema.sql:295` |
| `runs_updated_after_created` → `run_updated_after_created` | 1 · `deploy/sql/run-state.sql:135` |
| `runs_graph_identity_check` → `run_graph_identity_check` | 1 · `deploy/sql/run-state.sql:137` |
| `runs_execution_grain_check` → `run_execution_grain_check` | 1 · `deploy/sql/run-state.sql:143` |
| `runs_idempotency` → `run_idempotency` | 1 · `deploy/sql/run-state.sql:158` |
| `runs_event_root` → `run_event_root` | 1 · `deploy/sql/run-state.sql:162` |
| `runs_identity` → `run_identity` | 1 · `deploy/sql/run-state.sql:166` |
| `runs_platform` → `run_platform` | 1 · `deploy/sql/run-state.sql:170` |
| `runs_event_lineage_immutable` → `run_event_lineage_immutable` | 1 · `deploy/sql/run-state.sql:175` |
| `runs_admission_pins_immutable` → `run_admission_pins_immutable` | 1 · `deploy/sql/run-state.sql:181` |
| `wamn_authority.current_application()` → `wamn_authority.current_app()` | 10 · `deploy/sql/record-history.sql:98`, `deploy/sql/apply-grants.sql:8`, `apps/crates/guest/src/postgres_statements/numbering.rs:13` |
| `platform.prune_tx_identity()` → `platform.prune_transaction_identity()` (unsure: follows table expansion) | 11 · `deploy/sql/record-history.sql:38`, `crates/environment/src/project_setup.rs:88`, `services/control/src/control_sql.rs:205` |
| `wamn_authority.check_identity_xid()` → `wamn_authority.check_identity_transaction_id()` (unsure: follows column expansion) | 3 · `deploy/sql/record-history.sql:45`, `deploy/sql/record-history.sql:54`, `deploy/sql/record-history.sql:56` |
| `identity_xid` → `identity_transaction_id` (unsure: follows column expansion) | 1 · `deploy/sql/record-history.sql:55` |
| `pat_user_idx` → `personal_access_token_user_idx` (unsure: follows table expansion) | 1 · `deploy/sql/app-schema.sql:156` |
| `meta_platform_domain_check` → `platform_configuration_platform_domain_check` (unsure: follows table expansion) | 1 · `deploy/sql/system-schema.sql:132` |
| `orders_id_pkey` → `order_id_pkey` | 3 · `crates/codegen/tests/feed_live.rs:118`, `crates/codegen/tests/feed_live.rs:159`, `crates/codegen/tests/feed_live.rs:195` |
| `orders_item_id_fkey` → `order_item_id_fkey` | 2 · `crates/codegen/tests/feed_live.rs:119`, `crates/codegen/tests/feed_live.rs:128` |
| `orders_line_id_fkey` → `order_line_id_fkey` | 2 · `crates/codegen/tests/feed_live.rs:160`, `crates/codegen/tests/feed_live.rs:196` |

**F/I.** Catalog creates two policies per base table from `relation_name`. The four affected names are `packages_identity` → `package_identity`, `packages_platform` → `package_platform`, `workflows_identity` → `workflow_identity`, and `workflows_platform` → `workflow_platform`. The fixed system policies `login_org`, `login_org_select`, `login_org_insert` and `login_org_update` keep their spellings and move with their relations. The generic stamp and log trigger names also stay. `deploy/sql/catalog-schema.sql:301`, `deploy/sql/catalog-schema.sql:313`, `deploy/sql/system-schema.sql:1137`, `deploy/sql/system-schema.sql:1147`, `deploy/sql/system-schema.sql:1003`

**F/I.** PostgreSQL derives unnamed primary, unique, foreign-key, check and not-null constraint names from the defining table and sometimes its columns. Apply the table-prefix substitutions to those generated names. Column-only cases include `file_application_check` → `file_app_check`, `number_sequence_application_check` → `number_sequence_app_check` and `number_sequence_application_name_key` → `number_sequence_app_name_key`. If the column expansions land, include the FKs on `integration_type_auth.auth_type` and `session_signing_state.active_kid`. A renamed FK target alone does not imply a new name on its unchanged child. This is a source-derived list of naming inputs, without a live catalog dump. `deploy/sql/app-schema.sql:364`, `deploy/sql/app-schema.sql:405`, `deploy/sql/app-schema.sql:413`, `deploy/sql/system-schema.sql:97`, `deploy/sql/system-schema.sql:556`, `deploy/sql/run-queue.sql:53`

**F/I.** Table row types follow the table names. The explicit type reference to change if `tx_identity` expands is `wamn_authority.current_identity() RETURNS platform.tx_identity`. The event type and outcome catalogs are lookup tables, and the other affected enumerated values are text checks. The authored views `inventory.feed_screen`, `receiving.receipt_screen_line` and `receiving.purchase_order_receiving_summary` keep their names. Fixture `app.screen` keeps its name and changes its `orders` references. If the fixture schema `application` becomes `app`, move `item_view`, `inner_view`, `outer_view`, `expanded` and the function `authored` with it (unsure: fixture-only namespace). The ordinary identity sequences `provisioning.platform_event_id_seq` and `inventory.counter_id_seq` keep their source names by inference. `deploy/sql/record-history.sql:69`, `deploy/sql/system-schema.sql:618`, `deploy/sql/system-schema.sql:632`, `deploy/sql/system-schema.sql:645`, `apps/platform_fixture/migrations/0001_initial.sql:28`, `apps/platform_fixture/migrations/0001_initial.sql:66`, `apps/wamn_receiving/migrations/0001_initial.sql:90`, `apps/wamn_receiving/migrations/0001_initial.sql:112`, `crates/codegen/tests/feed_live.rs:122`, `crates/codegen/tests/catalog_live.rs:19`, `crates/codegen/tests/catalog_live.rs:23`, `crates/codegen/tests/catalog_live.rs:159`

### History relations and length limits

**F.** `wamn_history.create_history_table` creates `<source_schema>.<source_table>_history`. It derives the sequence `_history_position_seq`, index `_history_transaction_id_idx`, checks `_history_type_check` and `_history_operation_check`, primary key `_history_pkey`, and one `_history_<column>_not_null` constraint per history column. `log_row_change` looks up the history table by the **current** trigger table name and serializes source row keys and images. **I.** A live rename needs matching history relations and derived names. Existing JSON keys such as `application` need an explicit transition decision if retained history must use the new schema. `deploy/sql/record-history.sql:184`, `deploy/sql/record-history.sql:204`, `deploy/sql/record-history.sql:226`, `deploy/sql/record-history.sql:245`, `deploy/sql/record-history.sql:304`, `deploy/sql/record-history.sql:325`

Each row below denotes one generated history table.

| Old → new (I) | Generator or explicit reference (F) |
|---|---|
| `registry.orgs_history` → `registry.org_history` | `deploy/sql/system-schema.sql:146`, `deploy/sql/system-schema.sql:993` |
| `registry.projects_history` → `registry.project_history` | `deploy/sql/system-schema.sql:187`, `deploy/sql/system-schema.sql:1086` |
| `registry.environments_history` → `registry.env_history` | `deploy/sql/system-schema.sql:601`, `deploy/sql/system-schema.sql:1087` |
| `registry.environment_projects_history` → `registry.env_project_history` | `deploy/sql/system-schema.sql:686`, `deploy/sql/system-schema.sql:993` |
| `registry.environment_applications_history` → `registry.env_app_history` | `deploy/sql/system-schema.sql:711`, `deploy/sql/system-schema.sql:993` |
| `registry.environment_integrations_history` → `registry.env_integration_history` | `deploy/sql/system-schema.sql:728`, `deploy/sql/system-schema.sql:993` |
| `registry.event_readers_history` → `registry.event_reader_history` | `deploy/sql/system-schema.sql:865`, `deploy/sql/system-schema.sql:993` |
| `registry.retired_project_envs_history` → drop | `deploy/sql/system-schema.sql:803`, `deploy/sql/system-schema.sql:993`, `docs/plan/review-restructure.md:156` |
| `registry.env_app_connection_history` → eligible new history (unsure: confirm removal of the existing exclusion in the rename pass) | `deploy/sql/system-schema.sql:738`, `deploy/sql/system-schema.sql:995` |
| `identity.principals_history` → `identity.principal_history` | `deploy/sql/system-schema.sql:998`, `deploy/sql/system-schema.sql:1084` |
| `identity.org_memberships_history` → `identity.org_membership_history` | `deploy/sql/system-schema.sql:998`, `deploy/sql/system-schema.sql:1084` |
| `identity.org_roles_history` → `identity.org_role_history` | `deploy/sql/system-schema.sql:998`, `deploy/sql/system-schema.sql:1085` |
| `identity.project_roles_history` → `identity.project_role_history` | `deploy/sql/system-schema.sql:998`, `deploy/sql/system-schema.sql:1085` |
| `identity.project_env_memberships_history` → `identity.project_env_membership_history` | `deploy/sql/system-schema.sql:999`, `deploy/sql/system-schema.sql:1086` |
| `identity.password_credentials_history` → `identity.password_credential_history` | `deploy/sql/system-schema.sql:382`, `deploy/sql/system-schema.sql:999` |
| `platform.environment_history` → `platform.env_history` | `deploy/sql/app-schema.sql:269`, `crates/environment/tests/project_authority.rs:351` |
| `catalog.packages_history` → `catalog.package_history` | `deploy/sql/catalog-schema.sql:353`, `deploy/sql/catalog-schema.sql:357` |
| `catalog.workflows_history` → `catalog.workflow_history` | `deploy/sql/catalog-schema.sql:355`, `deploy/sql/catalog-schema.sql:357` |
| `registry.auth_type_history` → `registry.authentication_type_history` (unsure: follows base-table expansion) | `deploy/sql/system-schema.sql:89`, `deploy/sql/system-schema.sql:993` |
| `registry.integration_type_auth_history` → `registry.integration_type_authentication_history` (unsure: follows base-table expansion) | `deploy/sql/system-schema.sql:95`, `deploy/sql/system-schema.sql:993` |
| `registry.meta_history` → `registry.platform_configuration_history` (unsure: follows base-table expansion) | `deploy/sql/system-schema.sql:123`, `deploy/sql/system-schema.sql:993` |
| `platform.pat_history` → `platform.personal_access_token_history` (unsure: follows base-table expansion) | `deploy/sql/app-schema.sql:269`, `crates/environment/src/project_setup.rs:63` |
| `registry._sqlx_migrations_history` → `registry._sqlx_migration_history` (unsure: follows the completion-table decision) | `crates/platform-install/src/provision_system.rs:312`, `crates/platform-install/src/provision_system.rs:318` |
| Fixture `app.orders_history` → `app.order_history` | `crates/codegen/tests/feed_live.rs:27`, `crates/codegen/tests/feed_live.rs:118` |

**F/I.** The unchanged generated histories are the registry histories for `integration_type`, `integration_type_version`, `target`, `platform_event_type`, `platform_event_outcome` and `capture_gap`; platform histories for `user`, `role`, `user_role`, `permission`, `file`, `number_sequence`, `setting`, `connection`, `connection_generation` and `external_system`; `catalog.connection_requirement_history`; and the authored histories for `inventory.counter`, `feed_item`, `feed_line`, `feed_order`, `widget`, `widget_tag`, `receiving.item`, `purchase_order`, `purchase_order_line`, and `wms.location`, `packaging`, `product`. Authored history migrations call the shared constructor or emit functions in `wamn_history`; the `wms__*` function prefixes follow any authored schema rename (unsure: `wms` is an authored package schema). `deploy/sql/system-schema.sql:993`, `deploy/sql/app-schema.sql:267`, `deploy/sql/app-schema.sql:469`, `deploy/sql/catalog-schema.sql:350`, `apps/platform_fixture/history/0002_record_history.sql:2`, `apps/platform_fixture/history/0002_record_history.sql:439`, `apps/wamn_receiving/history/0002_record_history.sql:2`, `apps/wamn_receiving/history/0002_record_history.sql:187`, `apps/wamn_wms/history/0002_record_history.sql:38`, `apps/wamn_wms/history/0002_record_history.sql:299`

**I.** The old excluded name yields a **67-byte** `environment_application_connections_history_transaction_id_not_null`. The proposed `env_app_connection_history_transaction_id_not_null` is **50 bytes**. The longest proposed history name among the candidates above is **63 bytes**, `integration_type_authentication_history_transaction_id_not_null`. No proposed explicit history name exceeds the helper limit. The 50-byte `migration_<40 hex>` base can produce overlength **default** PostgreSQL names if written out literally, for example `migration_<40 hex>_execution_time_not_null` at **74 bytes**. Let PostgreSQL derive a shortened name or supply a short explicit name. This package migration ledger does not call `create_history_table`. `deploy/sql/system-schema.sql:995`, `deploy/sql/record-history.sql:204`, `deploy/sql/record-history.sql:226`, `crates/environment/src/project_migration.rs:9`, `crates/environment/src/apply_package.rs:155`, `crates/postgres/src/migrations.rs:144`

### Stored values, grants and delete rules

| Old value or surface (F) | Disposition (I) | Source (F) |
|---|---|---|
| `environment_apply` | `env_apply` | 4 occurrences · `deploy/sql/system-schema.sql:624`, `crates/platform-install/src/platform_event.rs:28`, `services/control/src/platform_event/tests.rs:83`, `test-support/grant-tests/tests/system.rs:302` |
| `environment_delete` | `env_project_deactivate` | 2 occurrences · `deploy/sql/system-schema.sql:625`, `crates/platform-install/src/platform_event.rs:29` |
| NATS key kind `environment` | `env` for the stored wamn discriminator; retain the NATS name | `deploy/sql/system-schema.sql:518`, `deploy/sql/system-schema.sql:534`, `crates/identity/src/identity/nats_store.rs:144` |
| NATS key kind `environment-signing` | `env-signing` | `deploy/sql/system-schema.sql:518`, `crates/identity/src/identity/nats_store.rs:167`, `crates/identity/src/identity/nats_store.rs:218` |
| File `exposure = 'application'` | `'app'`, including default, check, writer and row decoding | `deploy/sql/app-schema.sql:371`, `deploy/sql/app-schema.sql:372`, `crates/plugin/src/file_catalog.rs:80` |
| Setting `target_type = 'application'` | `'app'`, including the numbering join | `deploy/sql/app-schema.sql:446`, `apps/crates/guest/src/postgres_statements/numbering.rs:12` |
| `environment_target_changed` | `env_target_changed` | 1 occurrence · `deploy/sql/system-schema.sql:944` |
| `wamn-application/` | `wamn-app/` (unsure: advisory-lock namespace, so mixed images would acquire different locks) | 1 SQL occurrence · `crates/environment/src/registry/lifecycle_lock.rs:86` |
| `admin:revoke-environment-member` | `admin:revoke-env-member` | 1 fixture audit value · `crates/environment/tests/environment_fill.rs:98`, `deploy/sql/record-history.sql:305` |
| `Create organization`, `Apply environment`, `Delete environment`, `Deploy application`, `Roll back application` | Display labels (unsure: human-facing words; `Delete environment` needs a new label to describe deactivation) | 1 seed each · `deploy/sql/system-schema.sql:623`, `deploy/sql/system-schema.sql:625`, `deploy/sql/system-schema.sql:627` |

- **F/I.** `apply-grants.sql` contains affected direct grants at lines 6–35, the trigger-derived history grants at 45–59 and the conditional SQLx grant at 62–63. Remove the retired-table grant and the app and project-in-env delete privileges required by the old paths. Update the explicit control grants, the `login_org` protected-relation roster and its `registry.orgs` predicate branch, identity issuer and system reader grants, catalog relation/policy arrays, project runtime grants, PAT exclusions if expanded, and the pruning-watermark grant. `deploy/sql/apply-grants.sql:6`, `deploy/sql/apply-grants.sql:20`, `deploy/sql/apply-grants.sql:23`, `deploy/sql/apply-grants.sql:45`, `deploy/sql/apply-grants.sql:62`, `deploy/sql/system-schema.sql:541`, `deploy/sql/system-schema.sql:1081`, `deploy/sql/system-schema.sql:1104`, `deploy/sql/system-schema.sql:1123`, `deploy/sql/catalog-schema.sql:301`, `deploy/sql/record-history-app-grants.sql:16`, `crates/environment/src/project_setup.rs:58`, `crates/platform-install/src/identity_role.rs:169`, `crates/platform-install/src/system_reader.rs:11`, `test-support/grant-tests/tests/system.rs:354`
- **F.** There are **27 current `ON DELETE CASCADE` clauses** in `deploy/sql`. The one with a surviving parent delete path is `renewal_credentials.login_id` → `password_logins.id`, whose issuer cleanup calls `prune_expired`. Env apply directly deletes application-connection and integration rows. The FK **to** the integration has default delete behavior, and the cascade on the application-connection row points **to the app**, whose delete path R6 removes. `deploy/sql/system-schema.sql:450`, `crates/identity/src/identity/issuer.rs:87`, `crates/identity/src/identity/password_login.rs:257`, `deploy/sql/system-schema.sql:747`, `deploy/sql/system-schema.sql:749`, `crates/environment/src/registry/configuration.rs:171`, `crates/environment/src/registry/configuration.rs:173`, `crates/environment/src/registry/configuration.rs:247`
- **I.** The other **26** become `RESTRICT`. System parents and affected FK lines are org `188/311/520/602`, principal `283/319/770`, project `293/699`, org membership `368`, env `700/735`, project-in-env `724/772/880`, app `747`, event reader `908`, all in `deploy/sql/system-schema.sql`. Project parents are user and role at `deploy/sql/app-schema.sql:200`, `deploy/sql/app-schema.sql:202`, `deploy/sql/app-schema.sql:227`, number sequence at `deploy/sql/app-schema.sql:429`, connection at `deploy/sql/catalog-schema.sql:84`, `deploy/sql/catalog-schema.sql:94`, `deploy/sql/catalog-schema.sql:113`, `deploy/sql/catalog-schema.sql:124`, and run at `deploy/sql/run-queue.sql:53`. The existing delete-cascade test is `crates/identity/tests/identity_live.rs:238`. `docs/plan/review-restructure.md:160`
- **F.** Current HTTP, integration and file invocation paths check the connection’s active status. **I.** The requested credential deletion while keeping both connection and generation rows needs a schema decision: `credential_immutable` currently rejects deletion while the parent connection exists, and `connection_generation` references the credential with an FK. Renaming the objects and changing cascades alone cannot implement that transaction. `crates/plugin/src/bound_http.rs:135`, `crates/plugin/src/bound_http.rs:210`, `crates/plugin/src/integrations.rs:170`, `crates/plugin/src/file_connections.rs:42`, `deploy/sql/catalog-schema.sql:131`, `deploy/sql/catalog-schema.sql:145`, `deploy/sql/catalog-schema.sql:158`

### Not renamed

- **F/I.** `wasmcloud:blobstore/*`, `k8s`, `helm_revision`, `oauth_token`, `oauth_authorization`, `oauth2_*`, OCI digest and registry terminology, and `NATS` in `nats_key` refer to upstream technologies or protocols. Preserve the upstream portion of those names. `k8s` is also the existing storage discriminator (unsure: wamn persists it, but it names Kubernetes). `deploy/sql/system-schema.sql:1023`, `deploy/sql/system-schema.sql:660`, `deploy/sql/system-schema.sql:1036`, `deploy/sql/catalog-schema.sql:13`, `deploy/sql/catalog-schema.sql:85`, `deploy/sql/catalog-schema.sql:93`, `deploy/sql/catalog-schema.sql:111`, `deploy/sql/system-schema.sql:511`
- **F/I.** PostgreSQL catalog names, `pg_lsn`, `xid8`, `pg_current_xact_id`, `regclass`, `jsonb`, `uuid`, `application_name`, SQLite `sqlite_master`, and conventional generated-object suffixes such as `_pkey`, `_fkey`, `_idx` and `_seq` retain their technical spellings. `application_name` is PostgreSQL session metadata (unsure: contains `application`, with no wamn app meaning). MIME `application/json` has the same exclusion. `deploy/sql/record-history.sql:28`, `deploy/sql/system-schema.sql:897`, `crates/environment/tests/environment_lock_live.rs:136`, `crates/environment/tests/history_retention_live.rs:298`, `services/edge/src/store.rs:105`, `crates/codegen/src/generate/publication.rs:537`
- **F/I.** `orders` in the authored feed-screen view and in the receiving benchmark is a SQL alias or CTE (unsure: same spelling as the separate `app.orders` fixture). `wms` and `edge_samples` are authored schema names (unsure: product abbreviation and plural schema respectively). The singular-table rule leaves those schema spellings intact unless the owner includes authored namespaces in the abbreviation pass. `apps/platform_fixture/migrations/0001_initial.sql:32`, `services/cli/tests/fixtures/receiving_ui.sql:18`, `xtask/src/bench.rs:872`, `apps/wamn_wms/migrations/0001_initial.sql:1`, `apps/edge_samples/migrations/0001_initial.sql:1`

## 4. Rust SQL strings

**F.** Counts are affected Rust **string literals**, so a multiline batch counts once. `+N` counts additional literals for the flagged abbreviation expansions. The counts include the fixture schema and SQLx metadata candidates above. SQL object names supplied separately to formatters or row decoders follow the table and are called out separately.

| File | Count | Example literal starts |
|---|---:|---|
| `apps/crates/guest/src/postgres_statements/numbering.rs` | 1 | `apps/crates/guest/src/postgres_statements/numbering.rs:9` |
| `crates/authentication/src/administration/sql.rs` | 2 | `crates/authentication/src/administration/sql.rs:25`, `crates/authentication/src/administration/sql.rs:299` |
| `crates/authentication/src/authorization/project_reads.rs` | 1 +1 | `crates/authentication/src/authorization/project_reads.rs:18`, `crates/authentication/src/authorization/project_reads.rs:75` |
| `crates/codegen/src/feed.rs` | 8 | `crates/codegen/src/feed.rs:754`, `crates/codegen/src/feed.rs:920`, `crates/codegen/src/feed.rs:1000` |
| `crates/codegen/src/sql.rs` | 2 | `crates/codegen/src/sql.rs:280`, `crates/codegen/src/sql.rs:530` |
| `crates/codegen/tests/catalog_live.rs` | 16 | `crates/codegen/tests/catalog_live.rs:17`, `crates/codegen/tests/catalog_live.rs:40`, `crates/codegen/tests/catalog_live.rs:217` |
| `crates/codegen/tests/feed_live.rs` | 9 | `crates/codegen/tests/feed_live.rs:117`, `crates/codegen/tests/feed_live.rs:128`, `crates/codegen/tests/feed_live.rs:205` |
| `crates/codegen/tests/numbering.rs` | 2 | `crates/codegen/tests/numbering.rs:553`, `crates/codegen/tests/numbering.rs:582` |
| `crates/connection/src/project_connections/administration.rs` | 2 +7 | `crates/connection/src/project_connections/administration.rs:110`, `crates/connection/src/project_connections/administration.rs:125`, `crates/connection/src/project_connections/administration.rs:441` |
| `crates/connection/src/project_connections/credential_cleanup.rs` | +1 | `crates/connection/src/project_connections/credential_cleanup.rs:6` |
| `crates/connection/src/project_connections/store.rs` | +2 | `crates/connection/src/project_connections/store.rs:43`, `crates/connection/src/project_connections/store.rs:65` |
| `crates/environment/src/apply_package/entity_maps.rs` | 2 | `crates/environment/src/apply_package/entity_maps.rs:48`, `crates/environment/src/apply_package/entity_maps.rs:130` |
| `crates/environment/src/apply_package/package_version.rs` | 3 | `crates/environment/src/apply_package/package_version.rs:11`, `crates/environment/src/apply_package/package_version.rs:36`, `crates/environment/src/apply_package/package_version.rs:48` |
| `crates/environment/src/capture_sql.rs` | 5 | `crates/environment/src/capture_sql.rs:55`, `crates/environment/src/capture_sql.rs:71`, `crates/environment/src/capture_sql.rs:141` |
| `crates/environment/src/environment_configuration.rs` | 1 | `crates/environment/src/environment_configuration.rs:6` |
| `crates/environment/src/environment_fill.rs` | 1 | `crates/environment/src/environment_fill.rs:26` |
| `crates/environment/src/environment_sql.rs` | 8 +1 | `crates/environment/src/environment_sql.rs:6`, `crates/environment/src/environment_sql.rs:31`, `crates/environment/src/environment_sql.rs:76` |
| `crates/environment/src/environment_sql_effects.rs` | 10 | `crates/environment/src/environment_sql_effects.rs:123`, `crates/environment/src/environment_sql_effects.rs:515`, `crates/environment/src/environment_sql_effects.rs:1095` |
| `crates/environment/src/package_catalog.rs` | 1 | `crates/environment/src/package_catalog.rs:40` |
| `crates/environment/src/project_setup.rs` | 2 | `crates/environment/src/project_setup.rs:58`, `crates/environment/src/project_setup.rs:83` |
| `crates/environment/src/prune_record_history/plan.rs` | 1 | `crates/environment/src/prune_record_history/plan.rs:10` |
| `crates/environment/src/registry/configuration.rs` | 9 | `crates/environment/src/registry/configuration.rs:26`, `crates/environment/src/registry/configuration.rs:171`, `crates/environment/src/registry/configuration.rs:247` |
| `crates/environment/src/registry/deployment.rs` | 8 | `crates/environment/src/registry/deployment.rs:8`, `crates/environment/src/registry/deployment.rs:22`, `crates/environment/src/registry/deployment.rs:130` |
| `crates/environment/src/registry/lifecycle_lock.rs` | 1 | `crates/environment/src/registry/lifecycle_lock.rs:86` |
| `crates/environment/tests/apply_package_live.rs` | 1 | `crates/environment/tests/apply_package_live.rs:78` |
| `crates/environment/tests/environment_fill.rs` | 1 +3 | `crates/environment/tests/environment_fill.rs:15`, `crates/environment/tests/environment_fill.rs:87`, `crates/environment/tests/environment_fill.rs:179` |
| `crates/environment/tests/files_live.rs` | 1 | `crates/environment/tests/files_live.rs:41` |
| `crates/environment/tests/history_retention_live.rs` | 1 | `crates/environment/tests/history_retention_live.rs:171` |
| `crates/environment/tests/platform_migrations_live.rs` | 1 | `crates/environment/tests/platform_migrations_live.rs:100` |
| `crates/environment/tests/project_authority.rs` | 3 | `crates/environment/tests/project_authority.rs:91`, `crates/environment/tests/project_authority.rs:176`, `crates/environment/tests/project_authority.rs:313` |
| `crates/environment/tests/project_schema.rs` | 6 | `crates/environment/tests/project_schema.rs:165`, `crates/environment/tests/project_schema.rs:328`, `crates/environment/tests/project_schema.rs:887` |
| `crates/environment/tests/registry_deployment_live.rs` | 2 +1 | `crates/environment/tests/registry_deployment_live.rs:21`, `crates/environment/tests/registry_deployment_live.rs:92`, `crates/environment/tests/registry_deployment_live.rs:166` |
| `crates/environment/tests/support/files.rs` | 4 +1 | `crates/environment/tests/support/files.rs:50`, `crates/environment/tests/support/files.rs:139`, `crates/environment/tests/support/files.rs:384` |
| `crates/identity/src/identity.rs` | 9 | `crates/identity/src/identity.rs:37`, `crates/identity/src/identity.rs:42`, `crates/identity/src/identity.rs:63` |
| `crates/identity/src/identity/application/service.rs` | +4 | `crates/identity/src/identity/application/service.rs:16`, `crates/identity/src/identity/application/service.rs:95`, `crates/identity/src/identity/application/service.rs:191` |
| `crates/identity/src/identity/application/user.rs` | +1 | `crates/identity/src/identity/application/user.rs:57` |
| `crates/identity/src/identity/control.rs` | 1 | `crates/identity/src/identity/control.rs:21` |
| `crates/identity/src/identity/issuer.rs` | 8 +1 | `crates/identity/src/identity/issuer.rs:54`, `crates/identity/src/identity/issuer.rs:252`, `crates/identity/src/identity/issuer.rs:329` |
| `crates/identity/src/identity/nats_store.rs` | 8 | `crates/identity/src/identity/nats_store.rs:62`, `crates/identity/src/identity/nats_store.rs:90`, `crates/identity/src/identity/nats_store.rs:254` |
| `crates/identity/src/identity/org.rs` | 15 | `crates/identity/src/identity/org.rs:21`, `crates/identity/src/identity/org.rs:26`, `crates/identity/src/identity/org.rs:636` |
| `crates/identity/src/identity/password.rs` | 8 | `crates/identity/src/identity/password.rs:60`, `crates/identity/src/identity/password.rs:124`, `crates/identity/src/identity/password.rs:282` |
| `crates/identity/src/identity/password_login.rs` | 10 | `crates/identity/src/identity/password_login.rs:53`, `crates/identity/src/identity/password_login.rs:93`, `crates/identity/src/identity/password_login.rs:261` |
| `crates/identity/src/identity/session_keys.rs` | 10 +5 | `crates/identity/src/identity/session_keys.rs:57`, `crates/identity/src/identity/session_keys.rs:74`, `crates/identity/src/identity/session_keys.rs:343` |
| `crates/identity/src/identity_sql.rs` | 6 | `crates/identity/src/identity_sql.rs:3`, `crates/identity/src/identity_sql.rs:8`, `crates/identity/src/identity_sql.rs:33` |
| `crates/identity/src/session_exchange.rs` | 1 | `crates/identity/src/session_exchange.rs:195` |
| `crates/identity/tests/identity_live.rs` | 16 | `crates/identity/tests/identity_live.rs:38`, `crates/identity/tests/identity_live.rs:238`, `crates/identity/tests/identity_live.rs:636` |
| `crates/identity/tests/issuer_admission.rs` | 3 | `crates/identity/tests/issuer_admission.rs:37`, `crates/identity/tests/issuer_admission.rs:74`, `crates/identity/tests/issuer_admission.rs:135` |
| `crates/identity/tests/password_live.rs` | 18 | `crates/identity/tests/password_live.rs:53`, `crates/identity/tests/password_live.rs:72`, `crates/identity/tests/password_live.rs:492` |
| `crates/identity/tests/password_login_live.rs` | 11 | `crates/identity/tests/password_login_live.rs:85`, `crates/identity/tests/password_login_live.rs:91`, `crates/identity/tests/password_login_live.rs:337` |
| `crates/platform-install/src/identity_role.rs` | 3 | `crates/platform-install/src/identity_role.rs:169`, `crates/platform-install/src/identity_role.rs:209`, `crates/platform-install/src/identity_role.rs:210` |
| `crates/platform-install/src/migration_history.rs` | 1 | `crates/platform-install/src/migration_history.rs:6` |
| `crates/platform-install/src/platform_event.rs` | 1 | `crates/platform-install/src/platform_event.rs:101` |
| `crates/platform-install/src/platform_sql.rs` | +1 | `crates/platform-install/src/platform_sql.rs:12` |
| `crates/platform-install/src/provision_org.rs` | 1 | `crates/platform-install/src/provision_org.rs:235` |
| `crates/platform-install/src/provision_system.rs` | 1 +1 | `crates/platform-install/src/provision_system.rs:118`, `crates/platform-install/src/provision_system.rs:312` |
| `crates/platform-install/src/system_reader.rs` | 1 | `crates/platform-install/src/system_reader.rs:11` |
| `crates/platform-install/tests/provision_system_live.rs` | +1 | `crates/platform-install/tests/provision_system_live.rs:31` |
| `crates/plugin/src/file_catalog.rs` | 4 | `crates/plugin/src/file_catalog.rs:55`, `crates/plugin/src/file_catalog.rs:80`, `crates/plugin/src/file_catalog.rs:132` |
| `crates/plugin/src/operation_sql/transaction.rs` | 1 | `crates/plugin/src/operation_sql/transaction.rs:137` |
| `crates/plugin/src/operation_sql/write_log.rs` | +1 | `crates/plugin/src/operation_sql/write_log.rs:53` |
| `crates/plugin/src/run_store.rs` | 4 | `crates/plugin/src/run_store.rs:39`, `crates/plugin/src/run_store.rs:54`, `crates/plugin/src/run_store.rs:127` |
| `crates/plugin/src/run_store/queue.rs` | 3 | `crates/plugin/src/run_store/queue.rs:12`, `crates/plugin/src/run_store/queue.rs:65`, `crates/plugin/src/run_store/queue.rs:101` |
| `crates/plugin/src/run_store/transitions.rs` | 3 | `crates/plugin/src/run_store/transitions.rs:13`, `crates/plugin/src/run_store/transitions.rs:58`, `crates/plugin/src/run_store/transitions.rs:78` |
| `crates/plugin/src/workflow.rs` | 10 | `crates/plugin/src/workflow.rs:78`, `crates/plugin/src/workflow.rs:115`, `crates/plugin/src/workflow.rs:294` |
| `crates/plugin/src/workflow/admission.rs` | 6 | `crates/plugin/src/workflow/admission.rs:77`, `crates/plugin/src/workflow/admission.rs:97`, `crates/plugin/src/workflow/admission.rs:214` |
| `crates/postgres/src/transaction_identity.rs` | 1 | `crates/postgres/src/transaction_identity.rs:47` |
| `services/cdc-reader/src/replication.rs` | 1 | `services/cdc-reader/src/replication.rs:400` |
| `services/control/src/control_route/oauth_authorization_store.rs` | +1 | `services/control/src/control_route/oauth_authorization_store.rs:88` |
| `services/control/src/control_sql.rs` | 6 +1 | `services/control/src/control_sql.rs:9`, `services/control/src/control_sql.rs:184`, `services/control/src/control_sql.rs:205` |
| `services/control/src/host_route/tests.rs` | 2 | `services/control/src/host_route/tests.rs:82`, `services/control/src/host_route/tests.rs:207` |
| `services/control/src/platform_event/tests.rs` | 1 | `services/control/src/platform_event/tests.rs:186` |
| `services/control/src/retention/tests.rs` | 1 | `services/control/src/retention/tests.rs:35` |
| `services/edge/src/samples.rs` | 8 | `services/edge/src/samples.rs:9`, `services/edge/src/samples.rs:78`, `services/edge/src/samples.rs:130` |
| `services/edge/src/store.rs` | 8 | `services/edge/src/store.rs:14`, `services/edge/src/store.rs:143`, `services/edge/src/store.rs:207` |
| `services/edge/tests/forward.rs` | 1 | `services/edge/tests/forward.rs:574` |
| `services/host/src/host/host_route/tests.rs` | +1 | `services/host/src/host/host_route/tests.rs:118` |
| `services/identity-service/src/password.rs` | 3 | `services/identity-service/src/password.rs:503`, `services/identity-service/src/password.rs:509`, `services/identity-service/src/password.rs:679` |
| `services/identity-service/src/users.rs` | 1 | `services/identity-service/src/users.rs:319` |
| `test-support/grant-tests/tests/credential_tables.rs` | +1 | `test-support/grant-tests/tests/credential_tables.rs:21` |
| `test-support/grant-tests/tests/project.rs` | 3 +4 | `test-support/grant-tests/tests/project.rs:61`, `test-support/grant-tests/tests/project.rs:123`, `test-support/grant-tests/tests/project.rs:150` |
| `test-support/grant-tests/tests/system.rs` | 1 | `test-support/grant-tests/tests/system.rs:567` |
| `test-support/grant-tests/tests/transaction_identity.rs` | 1 | `test-support/grant-tests/tests/transaction_identity.rs:11` |
| `tests/journeys/tests/installed_digest.rs` | 2 | `tests/journeys/tests/installed_digest.rs:131`, `tests/journeys/tests/installed_digest.rs:223` |
| `tests/journeys/tests/request_path.rs` | 1 | `tests/journeys/tests/request_path.rs:216` |
| `tests/journeys/tests/workflows.rs` | 2 | `tests/journeys/tests/workflows.rs:98`, `tests/journeys/tests/workflows.rs:198` |
| `tests/receiving-tests/receiving_data_access.rs` | 1 | `tests/receiving-tests/receiving_data_access.rs:558` |
| `xtask/src/bench.rs` | 2 | `xtask/src/bench.rs:504`, `xtask/src/bench.rs:852` |
| `xtask/src/netsuite.rs` | 1 | `xtask/src/netsuite.rs:188` |

**F.** Also update separately supplied SQL names and values: the package migration table formatter and platform ledger constants; reserved relation-name arrays; the system grant test's seed and policy roster; the feed fixture model roster; stored event codes and their assertions; the NATS key-kind bind; constraint-name matching; and SQL row/JSON decoders that rely on expanded names or whole-row `to_jsonb`. `crates/environment/src/project_migration.rs:9`, `crates/environment/src/platform_migrations.rs:19`, `crates/postgres/src/lib.rs:62`, `crates/postgres/src/migrations.rs:112`, `crates/package/src/manifest.rs:20`, `test-support/grant-tests/tests/system.rs:253`, `test-support/grant-tests/tests/system.rs:354`, `crates/codegen/tests/feed_live.rs:115`, `crates/platform-install/src/platform_event.rs:28`, `services/control/src/platform_event/tests.rs:83`, `crates/identity/src/identity/nats_store.rs:167`, `crates/identity/src/identity/org.rs:37`, `crates/identity/src/principal.rs:175`, `crates/plugin/src/workflow.rs:278`, `crates/connection/src/project_connections/store.rs:47`

## 5. Installation and in-place rename

1. **F. System install and upgrade.** Platform install removes the previous `wamn-system-sql` Job and upgrades the Helm release with Job waiting. The chart recreates the Job, which runs `provision-system` from the selected control image. On a fresh database, `install_control_store` installs the embedded `record-history.sql`, `system-schema.sql`, definer privileges and portable-store bootstrap in one transaction. The registry SQLx ledger is the completion marker. A complete existing database skips that bootstrap. An existing substrate without the marker fails as incomplete. `crates/platform-install/src/platform_install.rs:27`, `crates/platform-install/src/platform_install.rs:123`, `deploy/charts/platform/templates/system-sql.yaml:24`, `deploy/charts/platform/templates/system-sql.yaml:37`, `services/control/src/main.rs:77`, `crates/platform-install/src/lib.rs:28`, `crates/platform-install/src/provision_system.rs:203`, `crates/platform-install/src/provision_system.rs:253`, `crates/platform-install/src/provision_system.rs:289`
2. **F. Existing system databases.** The same Job still runs pending embedded **system** SQLx migrations, then installs pooler auth and reapplies issuer/apply grants and platform limits. `apply_target` uses the restricted migration login, a session advisory lock and SQLx history. The two embedded platform migration directories contain only `.gitkeep` at this commit. The existing test supplies numbered sources directly and demonstrates that a second initialization applies the new source to the existing database. `crates/postgres/src/migration_scope.rs:13`, `crates/postgres/src/migrations.rs:89`, `crates/postgres/src/migrations.rs:110`, `crates/platform-install/src/provision_system.rs:56`, `crates/platform-install/src/provision_system.rs:87`, `crates/platform-install/src/provision_system.rs:226`, `crates/environment/tests/platform_migrations_live.rs:59`, `crates/environment/tests/platform_migrations_live.rs:77`
3. **F. Project databases.** Env provisioning installs the project catalog, app, run state and queue schema plus grants for an unconfigured project. The project bootstrap itself gates on `platform."user"`. Fresh provisioning then applies the embedded project migrations. For an existing project, `wamn platform schema apply --target project --env …` submits a worker Job, takes the env lifecycle lock, and applies pending **project** migrations from that worker image to each selected project. The diff command observes only. Package migrations and package reset use separate, per-package histories. Project reset preserves the platform schemas and truncates catalog/run state. `crates/environment/src/environment_sql_effects.rs:724`, `crates/environment/src/platform_migrations.rs:134`, `crates/environment/src/environment_schema.rs:7`, `services/cli/src/platform_verbs.rs:70`, `services/control/src/platform.rs:17`, `crates/environment/src/environment/worker.rs:260`, `crates/environment/src/platform_migrations.rs:60`, `crates/environment/src/environment_sql_effects.rs:485`, `crates/environment/src/environment_sql_effects.rs:515`, `crates/environment/src/project_migration.rs:4`
4. **I. In-place is technically possible through new forward migrations for both targets.** Editing the bootstrap files alone changes fresh installs. A live pass would need to converge the previously installed tables, columns, dependent objects, text values, policies and grants before matching readers and writers start. In particular, finalization applies the new system grants after the system migrator runs, and a change to the system completion marker has to preserve detection through that transition. SQLite has a separate issue: its startup executes `CREATE TABLE IF NOT EXISTS`, so replacing `claims` and `samples` in that DDL would create new tables beside the old data. The evidence here is source and the checked-in migration test. This read-only scope did not execute a migration or inspect a live catalog. `crates/platform-install/src/provision_system.rs:63`, `crates/platform-install/src/provision_system.rs:106`, `crates/platform-install/src/migration_history.rs:6`, `crates/environment/src/platform_migrations.rs:139`, `services/edge/src/store.rs:105`, `services/edge/src/store.rs:111`, `services/edge/src/samples.rs:10`
