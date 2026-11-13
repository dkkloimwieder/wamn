# Crate review

One sub-tab per crate. Each crate is read in full and judged on two things: is its API one a caller would choose, and is its complexity needed. The question for every caller is whether it should exist, not only what it needs.

Order: crates with no workspace dependencies first, most-used first. Basis: main at `553511bbd`. Nothing is built or run. Findings are marked fact or inference. Changes are options for the owner to rule on.

## Rules the owner has set for this pass

- A crate has one purpose. (2026-11-10)
- What a crate defines is not defined again anywhere else. Single source of truth. (2026-11-10)
- The preference is always to delete. (2026-11-10)
- Platform pieces do not live with client pieces. (2026-11-10)
- SQL is not scattered across the code. (2026-11-10)
- Be generous in what is accepted and conservative in what is sent. (2026-11-10)
- Every caller is read before any option is written. (2026-11-10)

For each crate the tab therefore answers: what is its one purpose; what in it serves another purpose; and where else its definitions are repeated.

## Rulings that cross crates

- One login per database role. A changed Secret restarts the host. `CredentialGeneration` goes in 84 files. (2026-11-11, tab 09)
- Compatibility with rows of the retired flow language goes: the `flow_id` and `flow_version` columns and the code that reads them. (2026-11-11, tab 09)
- An application may run dynamic SQL. The host holds it to a read-only transaction. (2026-11-11, tab 07)
- `jsonwebtoken` 11.1.0 with `EdDSA` is the one JWT implementation. (2026-11-11, tab 05)
- Request repeats are compared by a hash the server takes of the parsed request. No per-field respelling. (2026-11-10, tab 01)

| # | Crate | Non-test lines | Used by | Verdict | Status |
| --- | --- | --- | --- | --- | --- |
| 01 | `wamn-execution-contract` | 825 | 26 crates | Shared types and helpers, six unrelated topics, about a third without a caller | Ruled. The crate stops existing. The port names go to `wamn-catalog`. |
| 02 | `wamn-control-registry` | 1,826 | 16 | Identity of an environment plus an unused in-memory copy of the registry | Ruled. Options 1 to 4 and 6 accepted; registry SQL is gathered, not spread. |
| 03 | `wamn-event-wire` | 536 | 13 | One purpose, right name | Ruled. All nine options accepted; two carry a check. |
| 04 | `wamn-postgres-statements` | 685 | 10 | One purpose, right name | Ruled. All six options accepted; the SQLx fixture is removed. |
| 05 | `wamn-session` | 680 | 8 | One purpose, right name. Cleanest so far. | Ruled. `jsonwebtoken` 11.1.0 with `EdDSA` replaces the hand-written JWT here and in the runtime. |

| # | Crate | Non-test lines | Used by | Verdict | Status |
| --- | --- | --- | --- | --- | --- |
| 06 | `wamn-pg-core` | about 190 | 10 | Quoting helpers. Its headline type `Sql` has no caller. | Ruled. The crate is deleted; callers use \`postgres-protocol\`. |
| 07 | `wamn-component-policy` | 608 | 5 |  | Ruled. One table of allowed imports; the build script and the dead path go; raw SQL is no longer refused to applications. |
| 08 | `wamn-number-pattern` | 477 | 2 |  |  |

Not in this order: test support crates, fixtures and the small guest components with no dependents. They come after the crates that others depend on.

| # | Crate | Non-test lines | Used by | Verdict | Status |
| --- | --- | --- | --- | --- | --- |
| 08 | `wamn-number-pattern` | 477 | 2 | One purpose, single source holds | No change proposed. |
| 09 | `wamn-run-state` | about 3,000 | 9 | The run queue, plus five other things. About 1,130 lines have no production caller. | Ruled. All seven options accepted; the crate becomes the run queue only. |

| # | Crate | Non-test lines | Used by | Verdict | Status |
| --- | --- | --- | --- | --- | --- |
| 10 | `wamn-runtime`, part 1: three open points | 38,374 with inline tests |  | No queued run has a waiting caller; raw SQL can be held read-only; the stream function has no caller | Ruled. Four options accepted. The crate itself is read in further parts. |

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 11 | `wamn-runtime` part 2: Postgres plugin entry, claims, pools | 5,494 | 20 | waiting for rulings |

**Tab 11 status (2026-11-11):** ruled, except findings 19 and 21.

**Rule added 2026-11-11:** each tab lists the tests that go, change or move with the code. Tab 11 has the list. Tabs 01 to 10 got their lists on 2026-11-12.

**Cross-crate ruling 2026-11-11:** application dynamic SQL runs as a login that holds SELECT only. Grants refuse writes; text checks do not carry that job. How the host picks the login is open (tab 11).

The login is picked by interface: `client` gets the SELECT-only login, `statements` gets the application login (ruled 2026-11-11).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 12 | `wamn-runtime` part 3: SQL calls, operations, write log | 3,765 | 15 | waiting for rulings |

Tests for tabs 01 to 10: on hold by the owner.

Tab 12: ruled in full (2026-11-11).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 13 | `wamn-runtime` part 4: admitted statements and numbering | 1,428 | 8 | waiting for rulings |

Tab 13: ruled in full (2026-11-11). Time zone rule from the owner: stored datetimes are UTC; time zone settings are needed for printed numbers.

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 14 | `wamn-runtime` part 5: run queue, run admission, resolution | 3,169 | 16 | waiting for rulings |

Tab 14: ruled in full (2026-11-11).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 15 | `wamn-runtime` part 6: HTTP connection plugin | 2,812 | 12 | waiting for rulings |

Tab 15: ruled, except finding 2.

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 16 | `wamn-runtime` part 7: connection and OAuth files | 3,296 | 16 | waiting for rulings |

**Cross-crate ruling 2026-11-11:** `oauth2` 5.0.0 is the one OAuth 2 client implementation, with the pinned transport as its HTTP client. `jsonwebtoken` makes the signed assertion.

Tabs 15 and 16: ruled (tab 15 finding 2 held).

**Owner direction 2026-11-11:** use `hyper`, drop `reqwest`. Open for `wamn-runtime`: three files use `reqwest` as a client (`oauth_encryption.rs`, `session_keys.rs`, `pod_identity.rs`). Each gets an option when read.

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 17 | `wamn-runtime` part 8: route authentication and host memory | 2,190 | 13 | waiting for rulings |

Tab 17: ruled, except finding 8 (A2 or B). Owner direction 2026-11-11: adopt `moka` for caching.

Tab 17: fully ruled (finding 8: option A2).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 18 | `wamn-runtime` part 9: events, guest logging, effect spans | 3,416 | 15 | waiting for rulings |

Held row "scope record shared by plugins" (tab 15 finding 2): now also covers the claim map of `wamn_logging.rs` (tab 18 finding 12) and host-attested log fields (tab 18 finding 10 option B).

Tab 18: fully ruled (finding 10: option A).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 19 | `wamn-runtime` part 10: blob store plugin | 4,768 | 12 | waiting for rulings |

Held row "scope record shared by plugins" (tab 15 finding 2): now also covers the invocation map of the blob store plugin (tab 19 finding 2).

Tab 19: fully ruled (finding 12: option A).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 20 | `wamn-runtime` part 11: root files (last part) | 3,605 | 10 | waiting for rulings |

`wamn-runtime` is read in full (tabs 10 to 20).

Tab 20: fully ruled (finding 5: option A; option B open for the `control/dev` tab).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 21 | `wamn-engine` part 1: route plugin | 3,047 | 8 | waiting for rulings |

Held row "scope record shared by plugins" (tab 15 finding 2): now also covers `InvocationTraces` in `wamn-engine` (tab 21 finding 7).

Tab 21: fully ruled (finding 3: option A).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 22 | `wamn-engine` part 2: delivery and the operation call | 3,364 | 10 | waiting for rulings |

Tab 22: ruled (finding 4: option A; finding 8 open until part 3).

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 23 | `wamn-engine` part 3: engine build and component admission | 3,214 | 8 | waiting for rulings |

**Correction, 2026-11-11, tab 16 finding 9.** The finding named a crate `wamn-digest`. No crate of that name exists. The ruling "one function" stands. Tab 23 finding 6 proposes its home: `wamn-execution-contract`.

**Correction to the correction, 2026-11-11.** The note above was wrong. Tab 01 rulings (2026-11-10) say: "New crate `wamn-digest`. One digest function." and "`wamn-execution-contract` stops existing." So `wamn-digest` is a ruled new crate, not a mistake. Tab 23 finding 6 (home `wamn-execution-contract`) conflicts with tab 01. The tab 01 ruling stands: the one `sha256_digest` goes to the new crate `wamn-digest`. Tab 16 finding 9 stands as first written.

Tab 23: fully ruled (finding 2: option A). Tab 22 finding 8 closed: delete the provider check.

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 24 | `wamn-engine` part 4: integrations, release, artifacts (last part) | 2,982 | 7 | waiting for rulings |

`wamn-engine` is read in full (tabs 21 to 24).

Tab 24: fully ruled. Finding 8 added from the owner's question.

**Owner direction 2026-11-11:** integrations run through the native wash-runtime dispatch and pool. "we want to stay as close as possible to wasmcloud best practices."

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 25 | `wamn-execution-host` part 1: the operation host | 3,604 | 11 | waiting for rulings |

Tab 25 brings the two held questions back with options: the scope record shared by plugins (finding 1) and signed-file routes on the entry path (finding 2).

**Correction, 2026-11-11, tab 24 finding 3.** A cloud caller of `validate_component_in_release` exists: `crates/execution/host/src/operation.rs::validate_released_components`.

Tab 25: fully ruled. Held row "scope record shared by plugins" closed: one scope registry (finding 1 A). Held question "signed-file routes on the entry path" closed: `sign-result` on `wamn:operation` (finding 2 B1). Owner direction: prefer the WIT interface.

| # | Part | Lines read | Findings | Status |
| --- | --- | --- | --- | --- |
| 26 | `wamn-execution-host` part 2: file routes and application host routes | 3,354 | 9 | waiting for rulings |

Tab 26: fully ruled (finding 4: option A; finding 1 removes the second HTTP server).

Tab 27 (control\_route, integration, connection\_test): fully ruled. Finding 3: A plus moka pool per environment, pending research. Findings 4 and 10: A (one option). All others A; 12 to SQL scatter.

Tab 28 (wamn-router): fully ruled. Finding 2 B (delete `invariants.rs` and the random walk test). Finding 8 A (ports from `wamn-catalog`). Findings 1, 3–7, 9: A.

Tab 29 (wamn-workflow part 1): fully ruled. Finding 2 A (router reads catalog wiring types; `wiring_lowering.rs` goes). All others A. Tab 17 run-caller item closed (finding 3). Tab 28 finding 6 becomes "delete `ActiveWiring`".

Tab 30 (wamn-workflow part 2: queue, schedule, contract): fully ruled. Finding 1 A (router `Respond` stays; claim flags and durable caller mapping go; replaces tab 29 finding 10). Finding 3 B (park/release/list become host control routes). Finding 6 A (cron in the environment time zone). Findings 2, 4, 5, 7: A; 8 to SQL scatter. Owner rule: earlier epics, issues and plan documents are not cited.

Tab 31 (http-route guest): fully ruled. Finding 1 B pending checks (host routes become native ingress service routes). Finding 3 revised to A: the application route table lives in the guest, generated by the composer; the host keeps capabilities only (identity, operation authority); six checks listed. Finding 6 B (`form_urlencoded`, raw path). Others A. Tab 21 findings 1–3 corrected accordingly. Corrects tab 25 finding 10: the guest tag code is the only one.

Tab 32 (wamn-schema-generator part 1 of about 8; 25,624 non-test lines): fully ruled. Findings 1, 2: A. 4 items carried.

Tab 33 (wamn-schema-generator part 2: the build pipeline): fully ruled. Findings 1–4: A.

Tab 34 (wamn-schema-generator part 3: SQL type checking): fully ruled. Finding 1 A (SQLx describe over every statement; the verifier goes). Owner ruling U2: SQLx at build for all SQL; the host runs all queries on SQLx (replaces `tokio_postgres` and `rusqlite`); guests use the host interface. Alternatives (U1, guest SQLx over sockets) and future work (benchmark, developer ergonomics) recorded in the tab.

Tab 35 (wamn-schema-generator part 4: route publication): fully ruled. Finding 3 A (per-route body limit in the guest table, default 1 MiB). Findings 1, 2, 4: A.

Tab 36 (wamn-schema-generator part 5: CRUD SQL writer): fully ruled. Findings 1–4: A.

Tab 37 (wamn-schema-generator part 6: contract files, generated Rust) 37 wamn-schema-generator, part 6: fully ruled. Findings 1–5: A. Owner direction: take needed utilities from existing crates where possible.

Tab 38 (wamn-schema-generator part 7: WIT writer and JSON codecs) 38 wamn-schema-generator, part 7: finding 2: C (host converts JSON; operations export only typed run), with one open check (wasmtime value API). Re-entry: 1 (route table is its own component; tab 31 cross-referenced). Findings 1, 3–6: A. Fully ruled

Open checks, 2026-11-11: resolved in their tabs (24 finding 8, 26 finding 1, 27 finding 3, 31 findings 1 and 3 checks 2–4 and 6, 38 finding 2). Ruled: tab 31 check 1 A (no per-route shedding); tab 27 pool size is a host configuration option. Benchmark of the `Val` path added to tab 34 future work. Still open: tab 26 new item (file path against a model named `files`). Waiting for their tabs: tab 31 check 5 (edge), the composer data shape, the catalog definition hash. Tab 22 finding 5 is reversed by tab 38 finding 2 C.

Owner rule (2026-11-11, recorded in tab 26): applications may not use platform model names; the reserved set is read from the catalog's platform model set. Open: file byte routes under `/wamn_control/file/` (A) or `/api/<application>/files` (B).

Ruled (2026-11-11, tab 26): file byte routes B, `/api/<application>/file[/<id>]`. Singular model names are a convention: required for platform models, recommended for applications, not checked.

Tab 39 (wamn-schema-generator part 8: data functions and handlers) 39 wamn-schema-generator, part 8: finding 1: B (compiled input checks in the generated operation function; changed from A the same day; tab 31 finding 3 stands). Findings 2–6: A. Fully ruled

Tab 40 (wamn-schema-generator part 9: guest build step) 40 wamn-schema-generator, part 9: fully ruled. Findings 1–4: A.

Tab 41 (wamn-schema-generator part 10: data access grants) 41 wamn-schema-generator, part 10: fully ruled. Findings 1–2: A.

Tab 42 (wamn-schema-generator part 11: build record) 42 wamn-schema-generator, part 11: fully ruled. Finding 1: no freshness checks; the artifact takes every file from one build output. Findings 2–3: A.

Tab 43 (wamn-schema-generator part 12: manifest authoring) 43 wamn-schema-generator, part 12: findings 1–3: A; finding 4: A (keep the kcl binary). Owner direction recorded there: cargo xtask and just over Python and shell for project tasks; a tooling review tab follows the crates.

Tab 44 (build tooling and embedded KCL, investigation) 44 Build tooling and embedded KCL: ruled. KCL: keep the binary, embedding noted as possible. Tooling: move to cargo xtask and just; scripts reviewed after the crates.

Tab 45 (wamn-schema-generator part 13: client contract IR) 45 wamn-schema-generator, part 13: fully ruled. Findings 1–2: A.

Tab 46 (wamn-schema-generator part 14: client screen plan) 46 wamn-schema-generator, part 14: fully ruled. Finding 2: B; findings 1, 3: A.

Tab 47 (wamn-schema-generator part 15: TypeScript client) 47 wamn-schema-generator, part 15: fully ruled. Finding 1: B, keep the camelCase field maps (hand-written rule, no library), plus a fix to fromWire's key lookup. Finding 2: A.

Tab 48 (wamn-schema-generator part 16: client fields, route, components) 48 wamn-schema-generator, part 16: fully ruled. Findings 1–2: A.

Tab 49 (wamn-schema-generator part 17: OpenTofu renderer, platform document schemas, fully ruled: findings 1–2 A) 49 wamn-schema-generator, part 17.

Tab 50 (wamn-schema-generator part 18: SQL readers) 50 wamn-schema-generator, part 18: fully ruled. Findings 1–2: A.

Tab 51 (wamn-schema-generator part 19: cursor, references, inheritance, manifest validity) 51 wamn-schema-generator, part 19: ruled 2026-11-12, all A. Owner note: the generator looks too complex; its value is unclear.

Tab 52 (wamn-schema-generator part 20: materialize and build) 52 wamn-schema-generator, part 20: ruled 2026-11-12, findings 1, 2, 3, 5 A; finding 4 waits for the dev tab. Next: generator value tab (owner chose B, 2026-11-12).

Tab 53 (wamn-schema-generator value per output) 53 wamn-schema-generator, value per output: ruled 2026-11-12. Owner direction: generated CRUD is an overridable starting point (dev tooling). Output 2 C (trait defaults plus eject); 4 B (contract files deleted, facts in `wamn.json`); 5 delete (source maps); 6 A plus an OpenAPI document; 7 delete (SQLx verifier). Generator review complete.

Tab 54 (identity part 1: session, credential store, project state, client) 54 identity, part 1: ruled 2026-11-12. Findings 1, 2, 3, 5, 6 A; finding 4 B (hand-written Kubernetes client on `hyper`; `kube` is a later option if `kubectl` goes).

Tab 55 (identity part 2: platform identity core) 55 identity, part 2: ruled 2026-11-12, all A. Cross-checked against tabs 01, 02, 03, 04 and 05; tab 54 corrected to follow tab 05 (`jsonwebtoken`, `wamn-scalar`).

Tab 56 (identity part 3: orgs, applications, connections, host memory) 56 identity, part 3: ruled 2026-11-12, all A (finding 4: one cursor envelope; finding 6: NATS connect home carried to host and control tabs). Tab 02 renamed `wamn-registry` (owner, 2026-11-12). Platform identity crate review complete; `services/identity` next.

Tab 57 (identity service) 57 identity service: ruled 2026-11-12, all A. Finding 8 settles R-02 (no user PATs; API users are purpose-made service principals, not supported today). Identity review complete; edge next.

Tab 58 (edge) 58 edge: ruled 2026-11-12, all A (finding 6: one `wamn-kubernetes` crate). Settles tab 31 check 5. Control crates next.

Tab 59 (wamn-control-provision) 59 wamn-control-provision: ruled 2026-11-12, findings 1–4 and 6 A; finding 5 (Kubernetes manifests in Rust) carried to the control environment tab.

Tab 60 (wamn-package-control) 60 wamn-package-control: ruled 2026-11-12. Findings 1, 2 A; finding 3 A for now (B noted); finding 4 carried to the control environment tab.

Tab 61 (wamn-release-control) 61 wamn-release-control: ruled 2026-11-12, all A.

**Base moved (2026-11-12).** Owner: deploy is complete on `epic/dep7`, head b94833fd. Tabs 01–61 were read at `553511bbd`. Tabs 59–61 are rechecked at b94833fd: tab 61 finding 4 is done by dep7; tab 60 adds findings 5 and 6 (migration history), waiting for a ruling. The review continues at b94833fd.

Tab 60 at dep7: finding 6 ruled A3 (2026-11-12): one migration role per project database, SQLx `Migrator`. Open: views are not in the ownership table (gap under A3); finding 5 (empty platform migration scopes) not ruled.

Tab 60 view gap accepted (2026-11-12). Owner: packages composed together are one ownership unit; overlay additivity is a dev check, not an apply-time restriction.

Tab 60 finding 3 changed to B (2026-11-12): the overlay additive check moves to build and dev; the apply-time ownership table and its code go. **Owner direction (standing):** in general wamn is far too restrictive at apply and deploy; checks that are developer verification belong in build or dev.

Carried to the schema-introspection tab (owner, 2026-11-12): the apply-time migration allowlist (`migration_policy`, 1,689 lines). Options raised: A roles are the only check at apply, allowlist moves to build/dev or goes; B keep only a `SECURITY DEFINER` refusal at apply; C keep the allowlist at apply on `pg_query`.

Tab 60 finding 5 B (2026-11-12): platform migration scopes move to the SQLx `Migrator` now. Tab 60 is fully ruled at dep7.

Tab 62 (wamn-environment-control part 1, at dep7) 62 wamn-environment-control, part 1: crate shape, verb inputs, environment document stores, `env apply` flow. Four findings; waiting for ruling. Planned parts: 2 apply steps, 3 SQL, 4 ensure and provisioning, 5 platform install, 6 platform build, 7 the rest and the crate split.

Tab 62 ruled 2026-11-12: 1 A (one Helm-rendered config file per component), 2 A (Helm history is the applied-release record; SQL Jobs and credential Secrets are explicit steps, not hooks), 3 B (registry holds environment configuration; ConfigMap goes), 4 to 8 A, 9 A (role component builds move to the one component build path), 10 A preferred, final after parts 2 to 7. **One owner per fact:** platform settings in Helm values, applied release in Helm history, environment configuration in the registry, `environment.k` as input.

Tab 63 (wamn-environment-control part 2, apply steps) 63 wamn-environment-control, part 2: ruled 2026-11-12, all A (application is its release; one checked plan; release-keyed rows kept, retention removes them; delete is Helm uninstall plus explicit steps; no automatic rollback, \`host\_report\` goes).

Tab 64 (wamn-environment-control part 3, application SQL and execution paths) 64 wamn-environment-control, part 3: ruled 2026-11-12, all A (control route is the one execution path; one Job runner; \`Migrator\` checksums replace fingerprints; reset by \`DROP OWNED BY\` and rerun). Tab 60 finding 4 settled.

Tab 65 (wamn-environment-control part 4, provisioning) 65 wamn-environment-control, part 4: ruled 2026-11-12: 1 A (environment and org Helm charts; settles tab 59 finding 5), 2 B (CNPG managed roles, A as fallback), 3 A (runbook verbs become internal steps). Finding 4 A: org admins are admins in every environment by rule (no copied rows); an environment admin is not an org admin. Admin rules confirmed: org admin everywhere; environment admin over its projects and applications; project admin over its applications; all derived. \*\*Carried to the registry crate:\*\* the code orders org, project, environment (\`Triple\`, \`registry.project\_envs\`, key in 34 files) against the ruled org, environment, project, application.

Tab 66 (wamn-environment-control part 5, platform install) 66 wamn-environment-control, part 5: five findings with recommendations; settles the tab 62 `envFrom` check (runtime-operator 2.11.0 supports `runtime.envFrom`, per-group `envFrom`, `volumes`, `volumeMounts`). Ruled 2026-11-12, all A.

Tab 67 (wamn-environment-control part 6, platform build) 67 wamn-environment-control, part 6: ruled 2026-11-12, both A (BuildKit cache replaces input hashing; Cloud Build offload, process scan and spend counter deleted).

Tab 68 (wamn-environment-control part 7, remaining modules and the crate split) 68 wamn-environment-control, part 7: ruled 2026-11-12, all A (identity crate writes project user rows; schemas owned by the platform owner role; \`invite.rs\` and \`create\_user.rs\` go; split into \`wamn-environment\` and \`wamn-platform-install\`). Environment crate review complete; next: \`wamn-control-dev\`.

Tab 69 (wamn-dev-control part 1, loop stages and target database): ruled 2026-11-12, all A (six stages; Introspect goes, settling tab 52 finding 4; one reset method for dev and environments).

Tab 70 (wamn-dev-control part 2, dev runtime and provisioning): ruled 2026-11-12: 1 A pending a kind loop measurement (local dev overrides or CLI tools may still be needed for ergonomics); 2 A. \`wamn-dev-control\` review complete; next: \`wamn-ctl\`.

Tab 71 (`wamn-ctl`): ruled 2026-11-12: 1–5 A (one user CLI `wamn`, hidden worker `wamn-ctl`, `wamn-ctl-ops` goes; every user verb through a control route; no nested SQL Jobs; one hyper route client); 6: no change, a Linux-only `wamn` is acceptable. Corrects tab 70: `dev/watch.rs` is hand-written inotify. Next: host service.

Tab 72 (`wamn-host` service): ruled 2026-11-12: 1 A (local modes go after the kind measurement; settles tab 20 finding 5 as B), 2 B (control becomes a native HTTP service), 3 A (comments without bead ids or plan labels). Next: composer, catalog, introspection.

Tab 73 (composer and virtualizer): finding 1 A (2026-11-12); finding 2 checked: a linked base cannot join the overlay's transaction, ruled 2 A (keep composition). Next: catalog. The virtualizer is already deleted by the boundary ruling of 2026-11-10.

Tab 74 (`wamn-catalog` part 1: lib, manifest, SQL lexer): ruled 2026-11-12, all A. Next: catalog part 2.

Tab 75 (`wamn-catalog` part 2: serving manifest, component facts): ruled 2026-11-12, both A. Next: catalog part 3.

Tab 76 (`wamn-catalog` part 3: host routes, wiring, connections): ruled 2026-11-12, finding 1 A. Catalog review complete; next: package artifact, schema control, introspection. Notes that host route authority has no environment admin (tab 65 4 A).

Tab 77 (schema control, introspection, package artifact; includes the migration allowlist carried from tab 60): ruled 2026-11-12: 1 A, 2 A, 3 B (event trigger in place of the allowlist), 4 A. Next: runtime Postgres plugin.

Tab 78 (Postgres client after U2): one finding (no PostgreSQL connection uses TLS) ruled 2026-11-12, A (TLS on every connection). Next: materializer.

Tab 79 (materializer): ruled 2026-11-12: 1 B (native event admission in the host; the materializer guest goes); 2 falls away. Next: apps.

Tab 80 (apps): ruled 2026-11-12: 1 A (wamn.k the one authored source), 2 B (palette built once with the platform). Next: scripts and tooling.

Tab 81 (scripts and tooling): ruled 2026-11-12: 1 A, 2 B, 3 A. Next: registry crate follow-up (hierarchy order).

Tab 82 (registry, hierarchy order; carried from tab 65): ruled 2026-11-12, finding 1 A (environment entity above projects; names ordered org, environment, project).

Tab 83 (cdc-reader): ruled 2026-11-12, all A. Remaining unreviewed: scenario-worker, authoring model, client core, run-state-sqlite, test trees.

Tab 84 (scenario-worker and authoring model): ruled 2026-11-12, both A. Next: client core and run-state-sqlite.

Tab 85 (client core and run-state-sqlite): ruled 2026-11-12, finding 1 A. Next: the test trees (`tests/`, `test-support/`).

Tab 86 (test trees: `tests/`, `test-support/`): ruled 2026-11-12, findings 1–9 A.

Tab 87 (guest data libraries and vendored `pg_walstream`): ruled 2026-11-12, findings 1–4 A (1: B if the fork stays private). Crate review complete: every Rust package is reviewed.

Tab 88 (open items after the crate review): R-01, R-02, R-03, R-05, both held rows, tab 26 and the SQL-text carry are settled. Ruled 2026-11-12: R-04 A, R-06 C, R-07 B, R-08 B, R-09 TLS a need with A (loopback plus a documented TLS front), R-10 A, R-11 A, R-12 A, environment spellings A. The `pg_walstream` fork can be public (owner), so tab 87 finding 1 is A. Local development moves to `kind` (owner): tabs 70 1 A and 72 1 A no longer wait; the `kind` loop measurement is a step of the implementation plan. R-01 settles only the cache; the revocation rule is open (Summary). Test lists for tabs 01–10 written 2026-11-12.

Summary and Architecture tabs (2026-11-12): Summary holds the decisions by area and every open point; Architecture holds the current and the proposed system, with the rules for applications (row numbers kept). The earlier area tabs and notes are deleted; a Markdown export of 2026-11-12 keeps them outside the doc.

## Held, to come back to

| What | Where | Comes back when |
| --- | --- | --- |
| One scope record shared by all plugins, in place of one map per plugin | tab 15 finding 2, widens tab 11 finding 2 | `execution/host/src/operation/native_policy.rs` is read in full |
| Test lists for tabs 01 to 10 (written 2026-11-12) | this tab | the owner says so |
| Can routes that sign file results use the entry path | tab 12, option C | the `execution/host` tab |

Carried to every crate that follows: SQL text is in 130 non-test Rust files in 23 crates (tab 02); an environment is spelled five ways (tab 03).

Line counts are approximate: non-test lines, counted by search.
