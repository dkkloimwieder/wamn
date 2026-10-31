# Audit lane F: Developer experience

Three read-only Codex gpt-6.1-sol runs at reasoning effort high wrote this file, one per part, on 2026-10-31 against commit 8c831349d. Root copied each run's final message verbatim and checked that every finding cites a file and line. Beads: wamn-rk4wz.1, wamn-rk4wz.2, wamn-rk4wz.3. Shared tool output: [measurements.md](measurements.md).

<!-- lane F1, bead wamn-rk4wz.1 -->

## Part 1: inventory

The six applications contain 224 tracked files and 39,103 physical lines at `8c831349d3116aac27d9ca6f59a499da1c87efc7`.

Counts include comments and blank lines. Tests include `tests/**` and `web/test/**`. Inline Rust tests remain with their owning source file. SQL includes migrations, commands, and queries. Cargo files belong to their data or component category.

Each cell gives files / lines.

| Application | Manifest | SQL | Data crate | Component | Publication | Web | Tests | Other | Total |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `wamn_receiving` | 1 / 462 | 12 / 401 | 7 / 1,883 | 5 / 730 | 2 / 334 | 14 / 1,593 | 44 / 18,086 | 3 / 131 | 88 / 23,620 |
| `wamn_wms` | 1 / 356 | 14 / 386 | 10 / 1,135 | 4 / 302 | 3 / 422 | 0 / 0 | 25 / 6,615 | 3 / 114 | 60 / 9,330 |
| `edge_samples` | 1 / 60 | 2 / 7 | 6 / 210 | 4 / 147 | 2 / 69 | 0 / 0 | 0 / 0 | 1 / 13 | 16 / 506 |
| `rest_connector` | 1 / 44 | 0 / 0 | 0 / 0 | 3 / 351 | 4 / 166 | 0 / 0 | 3 / 1,055 | 1 / 33 | 12 / 1,649 |
| `platform_fixture` | 1 / 279 | 8 / 79 | 7 / 503 | 6 / 293 | 2 / 536 | 0 / 0 | 0 / 0 | 0 / 0 | 24 / 1,690 |
| `client_acme_receiving` | 1 / 199 | 7 / 151 | 5 / 536 | 5 / 477 | 3 / 243 | 0 / 0 | 1 / 601 | 2 / 101 | 24 / 2,308 |

“Other” contains READMEs, scenario documents, and package digest files. Counts come from `git ls-files` and newline counts over those files.

Tests account for 26,357 lines, or 67.4%. The remaining authoring surface contains 12,746 lines.

The clearest scaffolding family contains 22 files and 450 lines, or 3.5% of non-test lines. It comprises component/data Cargo files, authored WIT worlds, and statement include files. These files carry some necessary choices, but a convention can own their repeated structure.

A broader candidate family adds application error modules, scalar modules, Receiving’s cursor module, and publication files excluding wirings. It contains 43 files and 3,800 lines, or 29.8% of non-test lines. This is a review boundary, not a deletion estimate. It includes business rules and publication policy that must survive.

| Application | Candidate lines / non-test lines | Share |
| --- | ---: | ---: |
| `wamn_receiving` | 1,162 / 5,534 | 21.0% |
| `wamn_wms` | 802 / 2,715 | 29.5% |
| `edge_samples` | 254 / 506 | 50.2% |
| `rest_connector` | 97 / 594 | 16.3% |
| `platform_fixture` | 903 / 1,690 | 53.4% |
| `client_acme_receiving` | 582 / 1,707 | 34.1% |

The shared clone measurement cannot establish an authoring boilerplate ratio. Its formats exclude KCL, JSON, and TOML, which contain much of this repetition. It does identify 66 duplicated lines between Edge Samples and WMS data code, plus 201 between Receiving and WMS tests. Evidence: measurements.md section 7.

`apps/target/wamn` is absent. Generated file counts, generated line counts, and source-to-generated ratios are therefore unavailable for all six applications. The missing generated inputs also caused the 16 compilation errors recorded in measurements.md section 3.

The emitter establishes the output families without requiring a build. They include compiled manifests, SQL, contracts, statement accessors, data functions, component handlers, WIT/codecs, publication records, and optional TypeScript clients. Evidence: `crates/schema/generator/src/generate.rs:337`, `crates/schema/generator/src/generate/contracts.rs:209`, and `crates/schema/generator/src/materialize.rs:430`. Logical `generated/` paths map below `apps/target/wamn/<package>/` through `crates/schema/generator/src/output.rs:11`.

The target minimum for a CRUD application, which creates, reads, updates, and deletes records, is two production inputs: `wamn.k` and an initial SQL migration. The author also writes application acceptance cases and seed data. The generator owns ordinary SQL access, boundary contracts, component assembly, and default publication. An optional operator interface requires navigation choices and custom screens only. This target extends existing generation, which already emits complete CRUD data functions and handlers: `crates/schema/generator/src/generate/data.rs:3` and `crates/schema/generator/src/generate/component.rs:3`.

This lane changed no files and ran no builds or tests. The final working tree was clean.

## Finding F1-1: Custom publication copies contracts and already contains drift

Severity: high  
Effort: M  
Area: `apps/*/publication`, `crates/schema/generator/src/generate/publication.rs`  
Evidence: `apps/platform_fixture/wamn.k:177`, `apps/platform_fixture/publication/components/fixture.json.in:79`, `apps/edge_samples/wamn.k:29`, `apps/edge_samples/publication/attachments.json:23`, `crates/schema/generator/src/generate/publication.rs:42`.

Problem: Custom publication files repeat operation identities, input fields, types, limits, and definition hashes. The fixture manifest declares `expected_edit_version` as `int32`, but its component declaration describes a string. CRUD publication already derives schemas and hashes, while custom operations retain independent copies.

Proposal: Derive component port schemas and route input schemas for every manifest operation. Authors retain route selection, paths, authentication policy, and body limits. Generation resolves those choices into publication records and hashes.

Deletes: Inline custom input schemas, repeated operation declarations, and authored definition hashes across the six publication trees. Preserve wiring graphs and deliberate policy differences.

Risk: Definition hashes change, and stricter schemas can reject previously accepted inputs. Add a fixture test that admits a numeric revision and refuses a string through the published boundary. Retain publication coverage in `apps/rest_connector/tests/publication.rs:12`.

## Finding F1-2: Custom handlers repeat manifest request, result, and error definitions

Severity: medium  
Effort: M  
Area: `apps/wamn_receiving/data`, `apps/wamn_wms/data`, `apps/*/component`  
Evidence: `apps/wamn_wms/wamn.k:143`, `apps/wamn_wms/data/src/inventory_adjust.rs:19`, `apps/wamn_wms/component/src/inventory.rs:29`, `apps/wamn_receiving/wamn.k:189`, `apps/wamn_receiving/data/src/record_receipt.rs:80`.

Problem: WMS declares command fields in the manifest, repeats them in Rust structures, and copies them again in component adapters. Receiving similarly maintains a handwritten error vocabulary that repeats its manifest. Authors must synchronize these copies whenever a contract changes.

Proposal: Emit reusable Rust boundary types and their WIT conversions from the manifest. Business functions consume those types. Internal failures retain contextual error structures and translate once into the manifest-owned boundary contract.

Deletes: Duplicate command/result structures, literal switches, and field-copy adapters. Keep parsed business state, database sequencing, and business refusals.

Risk: Generated conversions can alter optional fields, numeric spelling, or error details. Exercise the resulting components through `released_wms_routes` at `apps/wamn_wms/tests/cluster.rs:46` and Receiving command histories at `apps/wamn_receiving/tests/receiving_command_histories_live.rs:1`.

## Finding F1-3: CRUD generation stops before component assembly

Severity: medium  
Effort: M  
Area: `apps/*/component`, `apps/*/data/src/statements.rs`  
Evidence: `apps/wamn_wms/component/src/lib.rs:10`, `apps/edge_samples/component/src/lib.rs:10`, `apps/edge_samples/component/wit/authored.wit:5`, `apps/wamn_receiving/data/src/statements.rs:6`, `crates/schema/generator/src/components.rs:119`.

Problem: The generator emits complete CRUD implementations, but authors still assemble their modules, bindings, worlds, and exports. WMS and Edge Samples each spend 33 lines on nearly identical component entry files. Statement include files repeat paths that generation already determines.

Proposal: Build pure CRUD packages from generated component crates. For custom behavior, use one conventional handler entry per operation and generate the surrounding assembly. Cargo continues to own custom dependencies and features.

Deletes: The five statement include files total 178 lines. Include-only WIT worlds and generated-module bootstrap blocks also disappear from application source. Pure CRUD applications need no authored data or component crate.

Risk: Assembly changes can omit exports or combine Cargo features differently. Extend `typed_crud_contracts_follow_a_second_model_declaration` at `crates/schema/generator/tests/generation.rs:1732` to build without authored scaffolding. Preserve separate guest invocations required by `crates/schema/generator/src/components.rs:5`.

## Finding F1-4: Acme reauthors the contract of its pinned base operation

Severity: medium  
Effort: M  
Area: `apps/client_acme_receiving`  
Evidence: `apps/client_acme_receiving/wamn.k:15`, `apps/client_acme_receiving/wamn.k:57`, `apps/client_acme_receiving/component/wit/authored.wit:8`, `apps/client_acme_receiving/component/src/lib.rs:224`, `apps/client_acme_receiving/component/src/lib.rs:243`.

Problem: Acme pins Receiving’s operation, then repeats its request, result, error details, and canonicalization rules. Its Rust component manually translates matching request and error structures. The base version also appears in authored WIT despite the exact dependency declaration.

Proposal: Let an overlay operation explicitly consume the wire shape of an operation from its pinned base artifact. Generate conversions and imports from that relationship. Keep Acme’s identity, claim scope, inspection behavior, and intentional contract overrides explicit.

Deletes: The copied contract portions, the handwritten base WIT import, `base_request`, and matching branches of `base_error`.

Risk: Incorrect inheritance can merge operation identities or change rollback behavior. Prove that a refused inspection rolls back the base receipt and that retries retain separate claim scopes. Retain overlay schema evidence from `apps/client_acme_receiving/tests/overlay_compatibility.rs:24`.

## Finding F1-5: Single-statement projections require handwritten implementations

Severity: medium  
Effort: M  
Area: `apps/platform_fixture`, `apps/wamn_receiving`, `apps/wamn_wms`  
Evidence: `apps/platform_fixture/data/src/widget_maker.rs:16`, `apps/platform_fixture/component/src/widget_maker.rs:17`, `apps/platform_fixture/wamn.k:259`, `apps/wamn_wms/data/src/inventory_aggregate.rs:17`, `apps/wamn_receiving/component/src/reads.rs:35`.

Problem: Several custom projections only call one generated statement, translate errors, and copy result fields. The fixture’s maker list uses 65 Rust lines across its data and component files. Its manifest already declares the input, statement, result, and errors.

Proposal: Support a direct statement implementation for projections whose parameters and results map without business decisions. Generate the full handler from that declaration. Keep aggregate null handling and other deliberate transformations explicit.

Deletes: The fixture maker-list data and component files, plus equivalent passthrough portions of Receiving and WMS reads. Authored query SQL remains.

Risk: Automatic mapping can confuse optional rows, nullable columns, and empty lists. Add generated execution cases for each distinction. Retain Receiving’s column-grant coverage at `apps/wamn_receiving/tests/receiving_data_access.rs:342`.

## Finding F1-6: Applications maintain scalar and cursor machinery that a shared crate already owns

Severity: medium  
Effort: S  
Area: `apps/wamn_receiving/data/src/cursor.rs`, `apps/*/data/src/scalar.rs`  
Evidence: `apps/wamn_receiving/data/src/cursor.rs:113`, `apps/platform/data/access/src/cursor/id.rs:61`, `apps/wamn_wms/data/src/scalar.rs:17`, `apps/edge_samples/data/src/scalar.rs:19`, `apps/platform/data/access/src/scalar.rs:19`.

Problem: Receiving retains a 309-line implementation of the canonical cursor format. WMS and Edge Samples also wrap UUID or timestamp parsing independently. Shared data access already owns these mechanisms, while application modules mix them with business rules such as positive quantities.

Proposal: Use the existing shared cursor and scalar functions. Keep application-specific bounds and value restrictions beside the business operation. Translate shared parsing failures at the owning operation boundary.

Deletes: Receiving’s cursor implementation after compatibility proof, plus duplicate generic parsing functions. Preserve business predicates and their tests.

Risk: Cursor bytes and scalar spelling affect continuation and retry behavior. Compare old and shared cursor bytes and rejection cases. Preserve Receiving’s cursor cases at `apps/wamn_receiving/data/src/cursor.rs:219` and WMS scalar cases at `apps/wamn_wms/data/src/scalar.rs:76`.

## Finding F1-7: Application tests duplicate platform build and deployment machinery

Severity: medium  
Effort: M  
Area: `apps/wamn_receiving/tests`, `apps/wamn_wms/tests`  
Evidence: `apps/wamn_wms/tests/cluster/build.rs:12`, `apps/wamn_receiving/tests/route_authentication_live/cluster/build.rs:23`, `apps/wamn_wms/tests/Cargo.toml:17`, `apps/wamn_receiving/tests/Cargo.toml:17`; measurements.md section 7.

Problem: Tests dominate the inventory, but some of that surface builds guests, starts infrastructure, captures artifacts, and deploys releases. Receiving and WMS repeat these platform responsibilities. The shared measurement identifies 201 duplicated lines between their test trees, independently of duplication within each tree.

Proposal: Let one platform test runner own package builds, environment lifecycle, and retained evidence. Applications supply package selection, seed SQL, business actions, and assertions. Share actual repeated behavior without generating copies into each app.

Deletes: Duplicate process setup, build execution, artifact collection, and deployment helpers. Keep business histories, concurrency cases, and failure assertions.

Risk: Consolidation can hide application-specific cleanup or lose failure evidence. Preserve WMS route and partial-completion cases at `apps/wamn_wms/tests/cluster.rs:46` and `apps/wamn_wms/tests/cluster.rs:53`. Preserve Receiving’s evidence-retention case at `apps/wamn_receiving/tests/route_authentication_live/cluster.rs:765`.

## Finding F1-8: Receiving manually reconnects generated CRUD descriptions to their behavior

Severity: medium  
Effort: M  
Area: `apps/wamn_receiving/web`  
Evidence: `apps/wamn_receiving/web/src/routes.tsx:70`, `apps/wamn_receiving/wamn.k:58`, `apps/wamn_receiving/web/src/forms.tsx:24`, `apps/wamn_receiving/web/src/forms.tsx:82`, `crates/schema/generator/tests/client_component.rs:554`.

Problem: Receiving repeats filter and sort fields that the manifest already declares. Its supplier and purchase-order forms manually connect generated descriptions to requests, revisions, retry keys, and reference selectors. Generated metadata therefore leaves substantial ordinary CRUD behavior for each author to rebuild.

Proposal: Put standard list, detail, create, and update controllers in the shared operator shell. Drive them from generated descriptions and routes. Keep Receiving’s receipt workflow and navigation choices authored.

Deletes: Repeated filter/sort lists, ordinary form submission adapters, generic detail loading, and equivalent reference-resolution glue.

Risk: Shared controllers can change retry identity, revision handling, or omitted-versus-null behavior. Preserve browser cases for supplier retry keys at `apps/wamn_receiving/web/test/screens.test.tsx:528` and reference selection at `apps/wamn_receiving/web/test/screens.test.tsx:352`.

## Open questions for the owner

1. Can pure CRUD applications use generated component crates with no authored Rust files?
2. Must HTTP exposure remain explicit for each custom public operation?
3. Can Acme consume its pinned base operation’s wire shape while retaining separate identity and inspection behavior?

## Proposed fix epics

### Derive operation boundaries once

1. Generate custom publication schemas and hashes
2. Generate custom Rust boundary types and adapters
3. Derive overlay contract shapes from pinned base artifacts

### Remove application execution scaffolding

1. Build CRUD components without authored wrapper crates
2. Generate direct statement projection handlers
3. Reuse shared scalar and cursor implementations

### Reduce application interface and test setup

1. Share CRUD controllers in the operator shell
2. Share application build and deployment test fixtures
3. Prove the two-input CRUD authoring path

<!-- lane F2, bead wamn-rk4wz.2 -->

## Part 2: dev loop

## Finding F2-1: Saved edits fail the freshness rule before the loop can rebuild

Severity: high  
Effort: M  
Area: crates/control/lib/src/dev/config.rs  
Evidence: `crates/control/lib/src/dev/coordinator.rs:1375`, `crates/control/lib/src/dev/config.rs:1291`, `crates/schema/generator/src/receipt.rs:178`, `apps/wamn_receiving/tests/route_authentication_live/dev/local_delivery.rs:165`

Problem: Each run resolves packages before it reaches Generate. Package resolution requires a build receipt whose input digest matches the current source. A saved SQL, migration, or manifest edit changes that digest, so the loop refuses the edit before rebuilding it. The existing acceptance test expects SQL edits to reach generation without a separate manual build.

Proposal: Give source preparation and artifact consumption separate boundaries. Prepare the candidate from authored inputs, then require a current receipt when admission or publication consumes its output.

Deletes: Remove the receipt freshness prerequisite from development source resolution and its call to `write_compiled_manifest`.

Risk: Accepting stale output at publication remains unacceptable. The saved-edit acceptance test must pass while publication still refuses an outdated receipt.

## Finding F2-2: Watch mode ignores application Rust and web edits

Severity: high  
Effort: M  
Area: crates/control/lib/src/dev/watch.rs  
Evidence: `crates/control/lib/src/dev/watch.rs:265`, `crates/control/lib/src/dev/watch.rs:399`, `crates/control/lib/src/dev/session.rs:390`, `apps/wamn_receiving/tests/route_authentication_live/dev/local_delivery.rs:159`

Problem: The package filter accepts manifests, migrations, declared SQL, and publication inputs. It does not accept files under the application's `component/`, `data/`, or `web/` directories. The additional build roots cover platform sources and shared WIT, so an application Rust edit produces no rebuild. The existing acceptance test explicitly expects a change to `component/src/reads.rs` to activate.

Proposal: Derive watched source files from the build inputs. Use the same source ownership for detection and rebuilding, with generated output excluded.

Deletes: Remove the separate, incomplete package input whitelist.

Risk: Watching generated output can cause endless rebuilds. The existing Rust edit case and `first_build_outputs_in_overlapping_component_roots_do_not_rerun` must both pass.

## Finding F2-3: Candidate preparation stops the previous application too early

Severity: high  
Effort: M  
Area: crates/control/lib/src/dev/coordinator.rs  
Evidence: `crates/control/lib/src/dev/coordinator.rs:1416`, `crates/control/lib/src/dev/coordinator.rs:1455`, `docs/operations/development-loop.md:266`, `apps/wamn_receiving/tests/route_authentication_live/dev/local_delivery.rs:172`

Problem: A changed package input digest calls `shutdown()` during preparation, before Generate, Build, or Gate. A later failure therefore leaves no previous application serving requests. This contradicts the documented promise that build and gate failures preserve the running application. The invalid-SQL acceptance case requires the previous application to remain reachable.

Proposal: Prepare artifacts before stopping the active host. Keep the stop boundary at activation for compatible changes, and stop earlier only when database changes require exclusive access.

Deletes: Remove unconditional shutdown on a changed package digest.

Risk: Running an old component against an incompatible schema can break requests. Extend the existing invalid-SQL case to cover compiler failure, gate refusal, and an incompatible migration.

## Finding F2-4: The generic developer command provisions a fixed Receiving environment

Severity: high  
Effort: M  
Area: crates/control/lib/src/dev/environment.rs  
Evidence: `crates/control/lib/src/dev/environment.rs:163`, `crates/control/lib/src/dev/environment.rs:247`, `crates/control/lib/src/dev/environment.rs:1036`, `crates/control/lib/src/dev/coordinator.rs:802`, `docs/operations/development-loop.md:159`

Problem: Standup fixes the coordinate to `acme/receiving/dev` and requires exactly one model schema across package sources. Release creation takes its coordinate from that generated configuration. Editing the coordinate in `environment.k`, as the documentation instructs, does not change the provisioned coordinate. Applications with multiple schemas also fail before startup.

Proposal: Read `environment.k` before provisioning and use its coordinate throughout. Derive package schemas from `wamn.k` without collapsing them into one application schema.

Deletes: Remove Receiving constants and `JourneyScope::receiving()` from the product startup path. Remove the single-schema restriction.

Risk: Identity, database markers, and event subjects must retain the same coordinate. Add a startup case with a different coordinate and two schemas, then make authenticated requests through both.

## Finding F2-5: Infrastructure ownership remains outside the developer command

Severity: medium  
Effort: L  
Area: crates/control/lib/src/dev/up.rs  
Evidence: `crates/control/lib/src/dev/up.rs:108`, `services/ctl/src/dev/up.rs:188`, `apps/wamn_receiving/tests/examples/delivery_timings.rs:299`, `crates/control/lib/src/dev/environment.rs:379`

Problem: `dev up` connects to supplied services rather than starting PostgreSQL, either NATS broker, or the collector. It starts identity, control, and web processes, while application activation starts the host and CDC reader. The test fixture already starts both brokers and the collector through Compose. Meanwhile, standup resets shared control schemas and drops matching Acme databases on the supplied PostgreSQL server.

Proposal: Make one private local stack the default developer environment. Start PostgreSQL 18 with logical WAL, both NATS brokers, and the collector under one session owner. Keep external endpoints as explicit overrides. Docker and Compose remain host prerequisites.

Deletes: Remove separate service startup instructions and fixture-only orchestration after moving their shared behavior into the local stack owner.

Risk: Cleanup must never remove another session's services. Prove concurrent startup, failed startup cleanup, and preservation of external services.

## Finding F2-6: Startup requires 22 input kinds that largely describe local machinery

Severity: medium  
Effort: M  
Area: services/ctl/src/dev/up.rs  
Evidence: `services/ctl/src/dev/up.rs:29`, `services/ctl/src/dev/up.rs:109`, `services/ctl/src/dev/up.rs:153`, `services/ctl/src/dev/command.rs:21`, `apps/wamn_receiving/tests/examples/delivery_timings.rs:338`

Problem: `dev up` requires 22 input kinds through flags or environment variables, including four event credential pairs. Only the platform UI path has a value default. The optional `--overlay-root` controls the printed next command, but that next command requires it. Startup therefore exposes provisioning details before the developer can run an application.

Proposal: Take package identities, dependencies, components, schemas, and connection requirements from `wamn.k`. Take the coordinate, route host, and application map from `environment.k`. Default binary paths to siblings of `wamn`, resolve component paths from build output, and allocate a private session root and service ports. Generate local broker credentials and use the fixture's local values of one replica and a 120-second duplicate window. Keep connection instance selections and external credentials explicit.

Deletes: Remove routine local startup flags and the optional-input path that prints an invalid next command. Do not add infrastructure fields to `wamn.k`.

Risk: Defaults must not reach deployed services. Test startup with no infrastructure flags and explicit external overrides.

## Finding F2-7: Checkout preparation builds unrelated applications and misses runtime inputs

Severity: medium  
Effort: M  
Area: tools/lane-start  
Evidence: `tools/lane-start:43`, `tools/lane-start:46`, `tools/lane-start:53`, `tools/lane-start:87`, `docs/operations/building.md:14`, `docs/operations/development-loop.md:11`

Problem: The documented first command builds every discovered application before any selected development task. It builds the CLI and scenario worker, but it omits the host, identity service, CDC reader, workload guests, and platform UI required by `dev up`. An unrelated application failure blocks checkout preparation. Also, `eval "$(tools/lane-start "$PWD")"` can hide failure because evaluating empty output succeeds.

Proposal: Limit checkout preparation to dependency installation and environment selection. Let the selected application command build its dependency closure and required runtime artifacts. Return failure directly from preparation.

Deletes: Remove the all-application build loop, its temporary database lifecycle, and the combined build-and-`eval` entry point.

Risk: Tests that include generated application files still need preparation. The quality runner must prepare its selected inputs, and a failed preparation command must return a nonzero status.

## Finding F2-8: Required host tools appear across several late failure points

Severity: medium  
Effort: S  
Area: docs/operations/building.md  
Evidence: `.cargo/config.toml:1`, `rust-toolchain.toml:3`, `crates/control/lib/src/dev/coordinator.rs:522`, `crates/control/lib/src/delivery/sqlx.rs:33`, `tools/install-kcl:13`, `docs/operations/building.md:5`

Problem: The build documentation names generic compiler tools, but Cargo specifically selects `clang` and `mold`. SQL packages also require SQLx CLI 0.9.0, which the loop checks during Generate. The pinned installers require Linux x86_64, while cold setup and cached `--offline` commands appear in different places. A developer can discover these requirements after expensive preparation.

Proposal: State one supported local host contract and test its executable versions before preparation. Separate initial dependency fetching from later offline operation. Keep KCL automatic, and leave wash, Helm, and OpenTofu outside the minimum app loop unless a selected command needs them.

Deletes: Remove duplicated prerequisite lists and personal cache configuration from the required checkout path.

Risk: An incomplete prerequisite list still causes late failures. Prove the documented setup in an empty supported host with no personal Cargo configuration.

## Finding F2-9: The collector recipe assumes a cached image and a free fixed port

Severity: medium  
Effort: S  
Area: tools/dev-collector  
Evidence: `tools/dev-collector:25`, `tools/dev-collector:30`, `tools/dev-collector:35`, `docs/operations/development-loop.md:35`, `deploy/platform/otel-collector.yaml:43`

Problem: The renderer sets `pull_policy` to `never`, and the documented Compose command also forbids pulling. Neither step fetches the pinned collector image for an empty Docker cache. The default host port is fixed at 4319, which conflicts across concurrent sessions. Small telemetry batches also wait up to 60 seconds before file output, as the documentation correctly states.

Proposal: Let the local stack owner fetch the pinned image when absent, allocate a port, and supply the discovered endpoint. Preserve deployed sampling and batching where parity matters.

Deletes: Remove manual collector rendering and endpoint copying from the normal startup procedure.

Risk: Changing batching changes observation behavior. Prove startup with an empty image cache, two concurrent sessions, and eventual receipt of a sampled trace.

## Finding F2-10: Test execution needs a separate artifact and service recipe

Severity: medium  
Effort: M  
Area: docs/operations/running-tests.md  
Evidence: `docs/operations/running-tests.md:163`, `docs/operations/running-tests.md:177`, `apps/wamn_wms/tests/local_business.rs:14`, `tests/integration/src/local_application/auth_nats.rs:31`, `test-support/postgres/lib.rs:23`, measurements.md section 3

Problem: Local business tests require application components, a separately built HTTP guest, and a native NATS executable through three environment variables. PostgreSQL binaries also come from the fixed path `/usr/lib/postgresql/18/bin`. Ordinary test runs omit these ignored cases. Shared measurements record 16 compilation errors from absent generated application files, so even compiling all targets requires earlier application builds.

Proposal: Give selected tests one preparation path that resolves artifacts and starts owned services. Preserve explicit path overrides for artifact comparisons. Make the selected runner execute required ignored cases and reject zero executed cases.

Deletes: Remove repeated component path exports and manual preparation recipes from individual test instructions.

Risk: A runner can select stale bytes or silently skip coverage. Prove execution of the Receiving and WMS business cases, plus all-target compilation from an empty artifact directory.

## Finding F2-11: No measured checkout-to-app timeline exists in the shared evidence

Severity: low  
Effort: S  
Area: docs/operations/development-loop.md  
Evidence: `tools/lane-start:43`, `services/ctl/src/build_verbs.rs:35`, `services/ctl/src/dev/command.rs:197`, `crates/control/lib/src/dev.rs:44`, `docs/operations/running-tests.md:632`, measurements.md section 11

Problem: The shared measurements contain no startup or edit latency measurements. The CLI prints development stage milliseconds, but checkout preparation, package builds, and `dev up` lack a combined timing record. The saved-edit correctness test explicitly reports no performance measurements. This lane changed no files and ran no builds or tests, so elapsed times remain unmeasured.

Proposal: Record the following current path once from a cold checkout and once with warm caches. Every duration below is unmeasured. Limits are failure budgets, not observed durations.

1. Install Linux x86_64 host tools: Rust 1.99.0, its configured components, `clang`, `mold`, `protoc`, Node 24.21.0, pnpm 12.9.1, Python, Docker, and Compose. Install SQLx CLI 0.9.0 for SQL applications. Tests additionally require PostgreSQL 18 binaries and a native `nats-server`. Time: unmeasured.
2. Initialize submodules and select the checkout's target directory. Install web dependencies with `pnpm install --frozen-lockfile`. KCL 0.13.0 installs automatically on first use. Time: unmeasured.
3. Build `wamn-ctl`, `wamn-host`, `wamn-identity`, and `wamn-cdc-reader` in debug mode. Time: unmeasured.
4. Start disposable PostgreSQL 18 with `wal_level=logical` and create `wamn_system`. Start separate scheduler and event NATS services. Prepare provisioning, runtime, publisher, and materializer credentials. Render the collector, fetch its printed image, and start its Compose service. Time: unmeasured. The lane-start database readiness limit is 300 seconds (`tools/lane-start:77`).
5. Run `wamn build` with package roots ordered before their overlays and `DATABASE_URL` or `--database-url`. Its other flags are `--output-root` and `--record-digest`, neither required for ordinary startup. Build HTTP route and materializer guests separately with `tools/guest-rustflags`, then build `@wamn/platform-ui`. Time: unmeasured.
6. Run `dev up` with all required inputs. For Receiving alone, the current command shape is:

   ```bash
   "$CARGO_TARGET_DIR/debug/wamn" dev up \
     --system-database-url "$SYSTEM_DATABASE_URL" \
     --root "$DEV_ROOT" \
     --nats-url "$SCHEDULER_NATS_URL" \
     --event-nats-url "$EVENT_NATS_URL" \
     --event-nats-username "$EVENT_RUNTIME_USER" \
     --event-nats-password-file "$EVENT_RUNTIME_PASSWORD_FILE" \
     --event-provisioning-username "$EVENT_PROVISIONING_USER" \
     --event-provisioning-password-file "$EVENT_PROVISIONING_PASSWORD_FILE" \
     --stream-replicas 1 \
     --dup-window-secs 120 \
     --otel-exporter-otlp-endpoint "$OTLP_ENDPOINT" \
     --route-host receiving.localhost \
     --platform-domain example.invalid \
     --flow-http-component "$CARGO_TARGET_DIR/wasm32-wasip2/release/http_route.wasm" \
     --cdc-reader-binary "$CARGO_TARGET_DIR/debug/wamn-cdc-reader" \
     --event-publisher-username "$EVENT_PUBLISHER_USER" \
     --event-publisher-password-file "$EVENT_PUBLISHER_PASSWORD_FILE" \
     --materializer-component "$CARGO_TARGET_DIR/wasm32-wasip2/release/materializer.wasm" \
     --event-materializer-username "$EVENT_MATERIALIZER_USER" \
     --event-materializer-password-file "$EVENT_MATERIALIZER_PASSWORD_FILE" \
     --host-binary "$CARGO_TARGET_DIR/debug/wamn-host" \
     --package "$PWD/apps/wamn_receiving" \
     --overlay-root "$PWD/apps/wamn_receiving"
   ```

   `--platform-ui` defaults to `apps/platform_ui/dist`. `--local-bindings` supplies explicit connection selections when needed. Time: unmeasured. Control and web startup has a 120-second readiness limit (`crates/control/lib/src/dev/control.rs:217`).

7. Write `environment.k` beside `dev.json`, with its application map and coordinate. For declared connections, set `WAMN_CONTROL_URL` and `WAMN_TOKEN`, then write the connection and its initial credential. Time: unmeasured.
8. Run `wamn dev --config "$DEV_ROOT/dev.json" --overlay-root "$PWD/apps/wamn_receiving" --watch`. Use `--hold` for a persistent one-shot run. Without either mode, the application stops after activation. Time: unmeasured. The loop reports Prepare, Migrate, Introspect, Generate, Build, Virtualize, ACL, Admit, Gate, Release, and Activate. Endpoint reachability has a shared five-second budget. HTTP workload readiness allows 120 seconds, and CDC streaming allows 60 seconds (`crates/control/lib/src/dev/config.rs:26`, `crates/control/lib/src/dev/activation.rs:35`, `crates/control/lib/src/dev/cdc_reader.rs:21`).
9. Open the emitted application URL and complete local invitation sign-in when needed. API requests return 503 before application activation. Stop the loop, then run `wamn dev down --root "$DEV_ROOT"` and stop external services separately. Time: unmeasured (`docs/operations/development-loop.md:109`, `docs/operations/development-loop.md:120`, `docs/operations/development-loop.md:125`).

Deletes: Replace scattered startup recipes with the selected application path. Retain detailed provisioning instructions for external-service overrides.

Risk: Warm caches can hide first-run costs. Record cache state and require a successful authenticated application request before stopping the startup timer.

## Open questions for the owner

1. Will the normal developer command own the full disposable stack, with external services supported through explicit overrides?
2. Will `environment.k` become the sole coordinate authority for local and deployed environments?
3. What cold-start, warm-start, and saved-edit latency limits must the developer loop meet?
4. Which existing saved-edit acceptance run passed against the current freshness and watcher rules?

## Proposed fix epics

### Restore the saved-edit loop

1. Separate source preparation from artifact freshness enforcement
2. Derive watched files from build inputs
3. Preserve the active application through candidate failures

### Own local startup

1. Start and stop one private development stack
2. Derive environment identity and local startup defaults
3. Prepare only the selected application and runtime artifacts

### Make startup reproducible

1. Establish the supported host prerequisite contract
2. Prepare and execute selected tests through one runner
3. Record cold and warm startup and edit timings

<!-- lane F3, bead wamn-rk4wz.3 -->

## Part 3: target path

The target is an authored manifest and SQL, a managed development session, and one deployment command. The commands below describe the proposed product. They are not the current CLI.

Assumptions: the author installs the SDK and Docker first. A platform administrator provides an organization, deployment target, and access. The ten-minute clock includes application generation, local startup, and deployment. This audit used source reads only. No files changed, and no builds or tests ran.

Findings follow the proposed implementation order.

## Finding F3-1: The starter must generate an executable application outside the platform checkout

Severity: high  
Effort: L  
Area: `services/ctl/src/bin/wamn.rs`, `crates/schema/generator/src/components.rs`  
Evidence: `services/ctl/src/bin/wamn.rs:18` defines no `new` or `deploy` command. `crates/schema/generator/src/components.rs:94` skips component compilation without the parent workspace manifest. `crates/schema/generator/src/components.rs:110` resolves platform build tools relative to that workspace. `apps/edge_samples/component/src/lib.rs:11` contains authored wrappers around generated operations.

Problem: Authors currently assemble an application inside the platform repository and maintain the Rust packaging around generated behavior. A manifest and SQL outside that layout do not produce the same executable package. Copying an existing application also copies its platform dependencies and maintenance duties.

Proposal: Make this the complete author path:

```bash
wamn new hello_app
cd hello_app
$EDITOR wamn.k migrations/0001_initial.sql
wamn dev
wamn deploy acme/hello/dev
```

`wamn new hello_app` generates `wamn.k`, an initial SQL migration, `.gitignore`, and a short README linking the platform quickstart. The starter declares a greeting model and its create, read, update, and delete operations.

The editor step changes the schema, declared operations, labels, and visible fields. SQL owns database types. Generation derives operation contracts from the migrated database.

`wamn dev` generates `target/wamn/hello_app/`. Its output contains the compiled manifest, SQL, contracts, WIT interfaces, Rust implementation, component bytes, browser client, web bundle, and `build.json`. WIT defines component interfaces. The build receipt records inputs and output digests. Generated Cargo manifests and wrappers remain build output.

`wamn deploy acme/hello/dev` generates package, release, and qualification artifacts. Qualification is evidence that required platform tests pass. Deployment selects their exact digests, applies the environment, publishes browser files, and prints the authenticated application URL.

Custom Rust remains an explicit extension for business behavior that declarations cannot express.

Deletes: Remove mandatory authored Cargo crates, WIT path lists, generated-code includes, and publication boilerplate from the default application shape. Replace repository-relative tool discovery for this path.

Risk: Standalone builds can change component interfaces or artifact identity. A proposed `hello_app_from_empty_directory` test must build and invoke every starter operation without the platform checkout.

## Finding F3-2: `wamn dev` must own its local services and first-run setup

Severity: high  
Effort: L  
Area: `services/ctl/src/dev/`, `crates/control/lib/src/dev/`  
Evidence: `services/ctl/src/dev/up.rs:28` requires database, broker, credential, telemetry, and binary inputs. `services/ctl/src/dev/command.rs:16` requires configuration and an overlay root. `crates/control/lib/src/dev/session.rs:225` reads a separate `environment.k`. `crates/control/lib/src/dev/watch.rs:1078` tests refusal of a repository without committed HEAD.

Problem: Starting an application requires platform setup before the development loop starts. Authors must also write an environment document and understand the two-step `dev up` and `dev` lifecycle. The Git requirement adds another prerequisite to a newly generated directory.

Proposal: Let bare `wamn dev` discover the current package, start isolated services, generate credentials and environment mapping, build, activate, and watch. Reuse the existing provisioning and supervision owners. Keep their strict internal documents generated in a private cache directory.

Ship matching platform binaries, guests, and shell assets with the SDK or its pinned development image. Preserve separate credential roles and deployed runtime behavior. Local startup must work before the first Git commit.

Deletes: Remove mandatory infrastructure flags, binary paths, handwritten local environment documents, and the separate standup step from the default author path. Remove committed HEAD as a local execution prerequisite.

Risk: Automatic setup can reset another session or leave services running. Proposed tests must prove two-session isolation and cleanup. Retain the existing compatible-edit and invalid-SQL behavior described at `docs/operations/running-tests.md:628`. Explicitly run `local_host_matches_the_deployed_host_contract`, currently ignored at `crates/control/lib/src/dev/activation.rs:2051`.

## Finding F3-3: Generated descriptions need a default application renderer

Severity: high  
Effort: L  
Area: `apps/platform_ui`, `apps/wamn_receiving/web`, `web/shell`  
Evidence: `apps/wamn_receiving/web/README.md:8` assigns requests, selectors, validation, and navigation to application bindings. `apps/wamn_receiving/web/src/routes.tsx:60` authors the screen map. `apps/wamn_receiving/web/vite.config.ts:58` imports the shell through a source alias. `apps/platform_ui/src/main.tsx:17` supplies administration and control screens. See also measurements.md section 8 for the tooling limitation when loading the Receiving Vite configuration.

Problem: Generated descriptions provide presentation data, but authors still connect them to requests and screens. The shared shell reduces duplication without removing the per-application web project. The existing platform UI entry does not automatically render arbitrary application models.

Proposal: Keep `apps/platform_ui` as the control application and shared shell owner. Publish the shell as an SDK dependency instead of importing its repository source.

For a default application, generate the route map and standard table, detail, and form bindings. Build those modules with the platform-owned web toolchain. Store the resulting application bundle in its package artifact. The author maintains no `web/package.json`, Vite configuration, or generic TSX screens.

An optional authored `web/` package supplies custom workflows through the same shell and generated clients. Receiving retains its custom receipt and offline behavior. Both modes use the same session, transport, permission filtering, and release selection.

Deletes: Remove mandatory per-application web manifests and Vite files. Replace mechanical screen bindings with generated bindings. Delete the repository source alias once the shell package supplies the interface.

Risk: Generic rendering can lose reference labels, action parameters, validation, or permission filtering. A proposed browser test must exercise generated CRUD screens and a custom screen together. Preserve the Receiving behavior documented at `docs/operations/running-tests.md:1214`.

## Finding F3-4: Platform tests and operational instructions remain application responsibilities

Severity: high  
Effort: M  
Area: `apps/*/tests`, `test-support/`, `docs/operations/`  
Evidence: `apps/wamn_receiving/tests/Cargo.toml:42` links application tests to control, provisioning, runtime, and infrastructure libraries. `apps/wamn_receiving/tests/route_authentication_live/environment.rs:48` installs packages, and line 95 tests platform grant defaults. `apps/wamn_wms/tests/environment.rs:13` imports similar setup owners. `apps/wamn_receiving/web/README.md:31` sends authors to issue notes for startup commands. See measurements.md section 7 for 201 duplicated lines between Receiving and WMS tests.

Problem: Application tests mix business assertions with installation, credentials, grants, and runtime setup. Application documentation repeats those platform procedures or depends on issue history. New authors inherit both responsibilities when they copy an example.

Proposal: Move reusable platform assertions and lifecycle setup into platform-owned suites using `platform_fixture`. Those suites own generation, wire contracts, standard CRUD, authorization isolation, migrations, restart behavior, browser sessions, and cleanup.

Authors retain tests for business rules, custom SQL results, integrations, and custom screens. Keep Receiving and WMS end-to-end tests for those domain behaviors.

Platform documentation owns installation, the quickstart, generation, local services, authentication, deployment, and recovery. Application documentation owns its scenario, schema meaning, business rules, custom behavior, and seed data.

Deletes: Remove duplicated application setup helpers and generic platform assertions after equivalent platform coverage exists. Delete copied operational procedures and replace issue-note instructions with links to canonical documentation.

Risk: Moving assertions can remove domain coverage accidentally. Run the platform fixture suite and retained Receiving/WMS business histories before deleting each duplicate. Measurements.md section 3 also records 16 compile errors from missing generated inputs. The platform test entry must prepare those inputs itself.

## Finding F3-5: Release qualification rejects every new application identity

Severity: high  
Effort: L  
Area: `crates/control/lib/src/delivery/qualification.rs`  
Evidence: `crates/control/lib/src/delivery/qualification.rs:119` selects cases through an exact package-name match. Line 137 rejects every unsupported package set. `docs/architecture/overview.md:109` states that environment application refuses an unqualified release.

Problem: Qualification supports WMS, Receiving, and Receiving with Acme. A correctly built hello application cannot pass that selection. Adding one more hello-specific branch preserves the same blocker for the next author.

Proposal: Replace package-name selection with a platform qualification suite driven by declared capabilities and frozen release contracts. Run installation, component admission, authorization, route invocation, readiness, and applicable event checks against the exact candidate artifacts.

Keep application business suites separate from platform qualification. Preserve qualification identity through the release digest, platform image set, and suite identity.

Deletes: Remove the production `application_for` package allowlist and mandatory selection of Receiving/WMS test executables. Retain their business cases under their application owners.

Risk: Generic qualification can accept an application while missing domain failures. Keep domain tests required by the application delivery policy. Proposed qualification tests must accept an unfamiliar package ID and reject malformed contracts, missing artifacts, and invalid grants. Preserve refusal of absent or mismatched qualification evidence.

## Finding F3-6: Deployment needs one author command over the existing control owners

Severity: high  
Effort: M  
Area: `services/ctl`, `crates/control/lib`, `docs/operations/deployment.md`  
Evidence: `docs/operations/deployment.md:119` requires an initial environment application without a release. Line 158 requires package publication before release publication. Line 185 qualifies the release, and line 212 applies its digest. `docs/operations/web-upload.md:94` requires a separate served-release selection before browser upload.

Problem: Deployment exposes the author to environment bootstrapping, publication, qualification, digest transfer, and static-file delivery. Successful runtime deployment also leaves a separate browser delivery step. Authors must coordinate these stages correctly.

Proposal: Implement `wamn deploy <org/project/env>` as orchestration over the existing control libraries. Build the current package, establish the target environment, publish exact package artifacts, publish and qualify the release, upload its web files, and apply its digest.

Generate the application mapping for the single-application default. Keep administrator-owned infrastructure and policy outside the application source. Print the release digest, qualification evidence, and application URL. Report the failed stage and retained artifacts when deployment stops.

Deletes: Remove author-maintained deployment scripts, manual digest transfers, and a separate web-upload step from the default path. Keep lower-level operator verbs for administration and recovery.

Risk: Retries can select different bytes or expose an API before its browser files exist. A proposed `deploy_hello_exact_release` test must cover first deployment, unchanged retry, qualification failure, upload failure, and rollback. Preserve immutable package tags and the environment lifecycle lock.

## Finding F3-7: The ten-minute target lacks a matching acceptance test

Severity: medium  
Effort: M  
Area: `services/ctl/tests`, `docs/operations/running-tests.md`  
Evidence: `docs/operations/building.md:17` prepares all packages before lane tests. `docs/operations/running-tests.md:632` explicitly excludes performance measurements from the local correctness case. `services/ctl/tests/platform_ui_live.rs:115` requires prebuilt binaries, components, and UI assets.

Problem: Existing tests start after substantial preparation and use established applications. They do not establish elapsed time from a new directory to an authenticated deployed application. This audit therefore supplies no evidence that ten minutes is achievable today.

Proposal: Add one platform-owned acceptance journey that runs the proposed commands verbatim. Start with the installed SDK, Docker, and provisioned account. Count downloads and preparation after `wamn new` in the result.

Create a greeting, change its field in SQL and `wamn.k`, observe the local change, deploy, sign in, and read the deployed value. Record elapsed time, cache state, stage results, and cleanup. Require completion within 600 seconds on a named runner. Publish installation time separately.

Deletes: Replace the hello-app procedure with this executable quickstart. Remove author-maintained smoke-test setup for generated behavior.

Risk: Hidden preparation can produce a misleading passing result. The proposed journey must run outside the platform checkout with empty application caches. It must fail when required generated outputs or browser files are absent.

## Open questions for the owner

1. Does the ten-minute clock use the proposed installed-SDK baseline, or include SDK installation and account provisioning?
2. Can the default author experience require Docker on Linux, or must the first release support another host platform?
3. Which application-specific business tests must deployment require in addition to generic platform qualification?

## Proposed fix epics

### Standalone application authoring

1. Generate a standalone manifest-and-SQL starter
2. Make development startup own local services and artifacts
3. Generate standard screens through the shared shell

### Platform-owned contracts and delivery

1. Move generic tests and operational documentation to platform owners
2. Qualify releases by capabilities instead of application names
3. Deliver runtime and browser artifacts through one deploy command

### Ten-minute acceptance

1. Run the executable hello-app quickstart within a measured 600-second budget
