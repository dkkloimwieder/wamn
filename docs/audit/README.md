# WAMN codebase audit, 2026-10-31

This audit covers WAMN applications, runtime services, build and deployment tools, tests, security boundaries, and documentation at commit `8c831349d`. The reports are recorded at `45a5ecd31`. On 2026-10-31 `8c831349d` is 13 commits behind `origin/main`. Every file:line pin in this audit refers to `8c831349d`, so open that commit before using a pin. Twenty-four read-only Codex gpt-6.1-sol runs used high reasoning effort, with one shared measurement pass at `788cdadb4`.

Inputs: [measurements](measurements.md), [code quality](code-quality.md), [coupling](coupling.md), [upgrade](upgrade.md), [deployment](deployment.md), [security](security.md), [developer experience](developer-experience.md), [tests](tests.md), and [hygiene](hygiene.md). The synthesis also reads [the architecture overview](../architecture/overview.md) and [CLAUDE.md](../../CLAUDE.md). No build, test, or deployment ran during this synthesis.

## Findings ranked across reports

The table covers all 222 numbered findings in 184 merged rows. Severity follows the reports. Merged rows use the highest severity and largest effort estimate. S, M, and L mean small, medium, and large effort.

Within each severity, risk reduction takes priority, followed by estimated deletion. Relocation does not count as deletion. Savings overlap and must not be added together. Each linked report file contains the source locations for its finding IDs.

| id | report file | severity | effort | area | one-line summary |
| --- | --- | --- | --- | --- | --- |
| E1-1, E2-1 | [E](security.md) | high | M | Org credentials | Valid coordinates collide in Secret, ServiceAccount, Role, and RoleBinding names. |
| E2-2 | [E](security.md) | high | L | Organization authority | An org control credential can change authority and connection records across organizations. |
| E1-2 | [E](security.md) | high | L | Authorization cache | Shared NATS credentials permit forged grant answers for another environment. Closed by alignment R4, R5 and R7. |
| E3-1 | [E](security.md) | medium | M | Component admission | Publication trusts registry descriptors without deriving admission facts from the component bytes. Waits for alignment W1 to W3. |
| E1-3 | [E](security.md) | high | L | Grant revocation | A delayed grant projection can restore application administration after revocation. |
| E1-4 | [E](security.md) | high | M | Membership | Concurrent grants can outlive deactivation, and session admission omits active org membership. |
| E1-5, E2-3 | [E](security.md) | medium | L | Database transport | Native database and replication connections permit plaintext traffic, including signing material. |
| E2-4 | [E](security.md) | medium | M | Event transport | Event NATS exposes credentials and event contents through plaintext connections. Closed by alignment R1, R3 and R4. |
| E3-2 | [E](security.md) | high | M | Dependencies | The recorded native dependency scan reports 17 vulnerabilities and 12 warnings. |
| E3-3 | [E](security.md) | high | M | Artifact limits | Package download and extraction lack aggregate byte, layer, and file budgets. |
| E3-4 | [E](security.md) | high | S | Guest memory | Serving defaults observe aggregate guest allocations instead of refusing excess allocation. |
| A3-2 | [A](code-quality.md) | high | L | Authority lifetime | Claim cleanup can panic during unwinding before it finishes revoking authority. |
| B2-1, C2-2, C3-3 | [B](coupling.md), [C](upgrade.md) | high | L | Lifecycle ownership | Parallel upgrade drivers mutate production packages outside one encompassing environment lock. C2-2: package application is an internal phase of `env apply`, the public `apply-package` verb is deleted. B2-1 closes under C3-3. |
| C1-2, C2-1 | [C](upgrade.md) | high | M | Installed environments | Installer completion rules can skip schema preparation or replay an application with `release = none`. C1-2 fix: the baseline SQL carries a content hash, install records it, apply refuses on a mismatch. C1-3 closes into C1-2. |
| C2-3 | [C](upgrade.md) | high | L | Package upgrades | Environment apply requires successor facts that its own upgrade phase cannot establish. Closed by the package-baseline deletion. |
| C1-4 | [C](upgrade.md) | high | M | Bootstrap recovery | Partial control bootstrap can leave a database that refuses every retry. Narrowed: bootstrap runs in one transaction and a retry recreates the system database. |
| C1-1, C1-5, C3-2 | [C](upgrade.md) | high | L | Schema ownership | Full-schema installation, empty migration streams, and partial repair provide different schema paths. C1-1 is rejected and its Deletes inverted: the migration machinery is deleted now, the full-schema path stays. C1-6 closes into C1-1. |
| C1-3 | [C](upgrade.md) | high | M | Upgrade ordering | Environment analysis reads schema-dependent facts before project migrations run. Closed into C1-2. |
| C3-4, D3-2 | [C](upgrade.md), [D](deployment.md) | high | L | Qualification identity | Qualification omits parts of the deployed platform and schema transition. D3-2 is medium: the key stays release plus host image, and chart, identity, edge and runner changes are proved by `platform_apply_kind` on their branch. C3-4 is closed: nothing to qualify under the package-baseline ruling. |
| A2-2, B2-4, D2-4 | [A](code-quality.md), [B](coupling.md), [D](deployment.md) | high | M | Process lifetime | Command adapters bypass bounded execution and can retain lifecycle locks indefinitely. D2-4 itself is medium after Group 1 deletes most listed owners. B2-4 is merged into D2-4: survivors go through `owned_command.rs` with a caller deadline, signal listeners move to the CLI binary. |
| C3-1, D1-1, D3-4, H3-2 | [C](upgrade.md), [D](deployment.md), [H](hygiene.md) | high | L | Desired state | KCL, JSON sidecars, host overlays, and local charts select competing deployment inputs. |
| D2-2, H1-1 | [D](deployment.md), [H](hygiene.md) | high | M | Image production | The older image pipeline builds unselected artifacts and omits real inputs from its cache identity. |
| B2-7, D2-6, D3-1 | [B](coupling.md), [D](deployment.md) | high | L | Installation | Installation builds and publishes candidates, while a platform stamp can skip application freshness. `build-platform` stays a `wamn-ctl` verb and `platform apply` consumes its outputs. |
| D2-1 | [D](deployment.md) | high | M | Clean upgrades | Closed by C3-3. The upgrade path is deleted, so no clean-worktree upgrade test is written. |
| B2-3 | [B](coupling.md) | high | L | Package execution | Package application calls back into the qualification runner that also calls package application. Closed by the package-baseline deletion. |
| B2-2 | [B](coupling.md) | high | L | Grant declarations | Provisioning, comparison, and schema repair independently assemble parts of grant policy. Re-scoped to the role to operation ACL. Small, Group 3: provisioning declares, control/lib compares against that declaration, no copy. |
| B1-1, B3-1 | [B](coupling.md) | high | L | Serving dependencies | Shared package types and release-profile logic pull the compiler into deployed services. B1-1: move the consumed types down to `catalog/model` and drop the three generator edges. No new crate. B3-1 merges into B1-1. The `wamn-contract` extraction is the Group 4 step. |
| B3-2 | [B](coupling.md) | high | L | Crate map | The proposed consolidation removes manifests while preserving engine, local runtime, and workflow restrictions. Package count is not a goal. The map is adopted with the README names, T11 deferred and T13 rejected. |
| B3-3 | [B](coupling.md) | high | L | Crate migration | Consolidation needs dependency ordering, live graph enforcement, and explicit guest repinning. Step 5 waits for A5. One qualification cycle covers the guest move. |
| A2-1 | [A](code-quality.md) | high | M | Scalar contracts | Numeric and timestamp implementations accept and normalize different spellings. |
| A3-1 | [A](code-quality.md) | high | M | PostgreSQL errors | Operation binding maps infrastructure failures to permission denials. |
| F2-1 | [F](developer-experience.md) | high | M | Saved edits | Development resolution requires fresh output before the loop can rebuild changed inputs. |
| F2-2 | [F](developer-experience.md) | high | M | Watch inputs | The package watch filter omits application Rust and web source directories. |
| F2-3 | [F](developer-experience.md) | high | M | Candidate activation | Preparation stops the active application before generation, compilation, and admission succeed. |
| F2-4 | [F](developer-experience.md) | high | M | Local identity | Generic development startup fixes a Receiving coordinate and requires one application schema. |
| F1-1 | [F](developer-experience.md) | high | M | Publication contracts | Custom publication repeats manifest contracts and already disagrees about a revision field. |
| F1-3, F3-1 | [F](developer-experience.md) | high | L | Application assembly | Generated CRUD still requires authored wrappers and repository-relative component build machinery. |
| F1-8, F3-3 | [F](developer-experience.md) | high | L | Application screens | Authors reconnect generated descriptions to ordinary CRUD behavior and maintain a web project. |
| F2-5, F2-6, F3-2 | [F](developer-experience.md) | high | L | Local startup | Development requires an external service recipe, many infrastructure inputs, and a separate standup step. |
| B2-6, F3-5 | [B](coupling.md), [F](developer-experience.md) | high | L | Qualification policy | Production qualification hard-codes Receiving and WMS package sets and rejects unfamiliar applications. The application declares its own case set beside its tests. No approver. |
| F3-6 | [F](developer-experience.md) | high | M | Author deployment | Authors coordinate bootstrap, publication, qualification, digest transfer, and browser upload manually. |
| D3-3, G3-3 | [D](deployment.md), [G](tests.md) | high | L | Merge evidence | A passing merge queue does not establish all affected live behavior or deployment qualification. D3-3 is closed into D2-8: keep successful logs, no CI service, no promotion job. |
| G3-1 | [G](tests.md) | high | L | Test tiers | Default and ignored tests do not identify their infrastructure or execution boundary. |
| G3-2 | [G](tests.md) | high | M | Test selection | Selection policy spans JSON, shell branches, preparation rules, and exceptional path mappings. |
| H1-2 | [H](hygiene.md) | high | S | Selection failures | Failed Git reads can become an empty successful test selection. |
| H3-1 | [H](hygiene.md) | high | S | Web dependencies | The parent pnpm installation omits dependency patches declared by the pinned wfe submodule. |
| D1-3 | [D](deployment.md) | high | S | RC telemetry | Closed by D3-6. The RC path and its Dockerfile are deleted, so no separate collector fix exists. |
| H2-1 | [H](hygiene.md) | high | M | Contract documentation | Active plans contradict current integration and write-log contracts. |
| D2-7, D3-6, F1-7, F3-4, H1-6 | [D](deployment.md), [F](developer-experience.md), [H](hygiene.md) | high | L | Deployment fixtures | Application and check routes repeat builds, installation, credentials, collection, and cleanup. |
| E3-5 | [E](security.md) | medium | L | Compiler containment | Import discovery compiles components, and cancelled callers cannot terminate detached compilation. |
| C2-8 | [C](upgrade.md) | medium | L | Contract version minimums | The declared contract phase has no admitted executor and separates contraction from version minimum recording. |
| C1-6 | [C](upgrade.md) | medium | M | Migration history | Retained retired records conflict with the promised restart at migration `0001`. Closed into C1-1. |
| C2-4 | [C](upgrade.md) | medium | M | Recovery | Completion markers skip mutable work without observing its current postconditions. |
| C2-5 | [C](upgrade.md) | medium | M | Helm recovery | Historical values equality bypasses pending-operation recovery and repair of live deployment drift. |
| C2-6 | [C](upgrade.md) | medium | M | Executor identity | Commit selection builds a target binary but still applies schema through the launching binary. Closed by C3-3. |
| C2-7 | [C](upgrade.md) | medium | M | Credential recovery | Upgrade cleanup deletes private outputs before durable stage completion, breaking interruption recovery. Closed with C3-3. The plan's admin flow survives. |
| C2-9 | [C](upgrade.md) | medium | M | Package reconciliation | Current package versions skip repair of derived history functions and triggers. |
| E1-6 | [E](security.md) | medium | M | Credential expiry | Cached PAT approval can remain valid after the credential expires. |
| E1-7 | [E](security.md) | medium | M | Cache freshness | Replication gives old authorization facts a new cache lifetime. |
| E1-8 | [E](security.md) | medium | M | Offline trust | File-backed signing keys remain trusted indefinitely. |
| E1-9 | [E](security.md) | medium | M | Grant admission | CLI and HTTP mutations enforce different covering-grant and grantability rules. |
| E2-5 | [E](security.md) | medium | M | Development credentials | Standing fixtures and backup declarations share known administrator credentials. Re-scoped after integration-schema drops the credential import paths. |
| E2-6 | [E](security.md) | medium | S | Secret diagnostics | Command help can print password-bearing environment values. |
| E2-7 | [E](security.md) | medium | M | Container identity | Standalone service images and deployments default to root execution. |
| E2-8 | [E](security.md) | medium | M | Image provenance | Mutable build and infrastructure tags permit different executable inputs for the same source. |
| E3-6 | [E](security.md) | medium | M | Connector retention | Retired bindings leave compiled connector templates and component bytes retained without bounds. Reshaped by integration-schema Runtime refresh. Nothing separate is filed. |
| E3-7 | [E](security.md) | medium | S | Process initialization | Engine construction mutates the process environment after threads and connection work start. |
| E3-8 | [E](security.md) | medium | M | Dependency refresh | Identity checks provide no fresh advisory requirement across maintained dependency domains. |
| A3-3 | [A](code-quality.md) | medium | M | Blobstore errors | Wire errors expose backend prose and classify timeouts through text matching. |
| A3-4 | [A](code-quality.md) | medium | M | Workflow settlement | Durable retry and terminal decisions depend on hidden types in error chains. |
| A3-5 | [A](code-quality.md) | medium | L | Library errors | Public APIs expose `anyhow`, forcing callers to inspect chains or display text. |
| A3-6 | [A](code-quality.md) | medium | S | PostgreSQL contract | SQLx duplicates the generated PostgreSQL wire error taxonomy. |
| A3-7 | [A](code-quality.md) | medium | M | Diagnostics | Internal wrappers discard causes before the public redaction boundary. |
| A3-8 | [A](code-quality.md) | medium | M | Credential parsing | Five provisioning validators repeat parser scaffolding and error vocabularies. Re-scoped to the five database-credential validators. |
| A3-9 | [A](code-quality.md) | medium | S | Client decoding | Malformed error envelopes acquire fabricated codes, operation names, and revisions. |
| H1-4 | [H](hygiene.md) | medium | S | Runner arguments | A missing value can leave the Kubernetes shell runner in an endless parser loop. |
| H1-8 | [H](hygiene.md) | medium | M | Benchmark cleanup | Failed cloud version minimum runs can leave sampling active or triggers altered. |
| H1-9 | [H](hygiene.md) | medium | S | Submodule governance | The merge queue omits its documented submodule guard. |
| G2-1 | [G](tests.md) | medium | M | History coverage | Quarantine disables owned-database history reconstruction and retention assertions. |
| G2-2 | [G](tests.md) | medium | M | Grant coverage | Shared fixture counts suppress the assertion that private operations receive no public grant. |
| G2-3 | [G](tests.md) | medium | S | Introspection coverage | A stale four-table assertion disables the full introspection refusal scenario. |
| G2-4 | [G](tests.md) | medium | M | Host parity | Incidental argument counts and quarantine suppress registry rendering and local host parity. |
| G2-5 | [G](tests.md) | medium | S | Credential tests | PostgreSQL-only ignores exclude ordinary credential retry tests. |
| G2-8, G3-4 | [G](tests.md) | medium | M | Ignored tests | Ignore reasons mix missing prerequisites, disabled assertions, and a subprocess helper. |
| G3-5 | [G](tests.md) | medium | M | Contract compilation | Compile-time generated-file reads prevent isolated client and generator test compilation. |
| G3-6 | [G](tests.md) | medium | L | Test placement | Mixed targets carry cheap assertions into expensive fixtures and repeat business histories at deployment boundaries. |
| A2-3 | [A](code-quality.md) | medium | M | Host contracts | Seventy-one redundant JSON files contain 3,020 repeated contract lines. |
| H2-2 | [H](hygiene.md) | medium | M | Active plans | Completed programs remain in the active plan collection. |
| B1-4, B2-5 | [B](coupling.md) | medium | L | Control coupling | The control compilation unit includes the developer application and unrelated tool orchestration. B1-4 is deferred until Group 1 has deleted about a fifth of control. B2-5 is deferred with it. |
| B1-2 | [B](coupling.md) | medium | M | Test dependencies | Six development dependency links pull higher-level owners into lower-level test builds. Fix each edge by replacing the import, not by relocating tests. Group 3. |
| B1-3 | [B](coupling.md) | medium | L | Artifact ownership | Narrow artifact consumers depend on the runtime hub and its cloud execution dependencies. Group 4, after the A5 inversion lands. |
| B1-5 | [B](coupling.md) | medium | M | Catalog ownership | Catalog combines canonical records with installation SQL and built-in application construction. In passing. |
| B1-6 | [B](coupling.md) | medium | M | WIT ownership | Guest builds read canonical interfaces from native implementation directories. One canonical WIT directory, versions unchanged, one repin. |
| B2-8 | [B](coupling.md) | medium | L | Release construction | Publication mixes immutable release assembly with local runtime representation and unused database inputs. Split: the unused inputs and duplicate path now, In passing. The release-control crate in Group 4. |
| B2-9 | [B](coupling.md) | medium | M | Provisioning results | Internal callers exchange credentials through CLI output paths, temporary files, and sentinels. In passing, when the integration-schema work touches `provision_project_env`. |
| B3-4 | [B](coupling.md) | medium | M | Guest packages | Application data crates and single-consumer SQLx and label helpers can become private modules. |
| B3-5 | [B](coupling.md) | medium | M | Workspace roots | Modbus and two fixture roots can move while native, standard guest, and `no_std` isolation remains. |
| G1-1, B3-6 | [G](tests.md), [B](coupling.md) | medium | L | Test packages | Support and suite packages can consolidate while PostgreSQL lifetime and test authority stay separate. |
| A1-12, A2-6 | [A](code-quality.md) | medium | M | Test ownership | Portable evaluators and database fixtures live in production libraries with repeated schema composition. |
| A1-1 | [A](code-quality.md) | medium | S | Dormant measurements | The startup-burst runner has no repository execution entry point. |
| A1-2 | [A](code-quality.md) | medium | S | Dormant cache | Router wiring-cache implementations and tests have no production caller. |
| F1-6 | [F](developer-experience.md) | medium | S | Application parsing | Applications duplicate shared cursor and scalar mechanisms. |
| A2-4 | [A](code-quality.md) | medium | M | PostgreSQL adapters | Named and default bindings repeat transaction, tracing, and resource handling. |
| A2-5 | [A](code-quality.md) | medium | M | Authority SQL | Four authored installation artifacts repeat the same authority bootstrap. |
| G1-2 | [G](tests.md) | medium | M | Database fixtures | Fifty helpers repeat connection and driver setup with different failure handling. |
| G1-3 | [G](tests.md) | medium | M | SQL commands | Fourteen local PostgreSQL runners repeat transport setup with inconsistent execution rules. |
| G1-5 | [G](tests.md) | medium | M | Cluster commands | Capture helpers and kubectl constructors repeat cancellation, context, and diagnostic policy. |
| G1-7 | [G](tests.md) | medium | M | Package fixtures | Three manifests and several environment documents repeat one fixture shape. |
| A2-7 | [A](code-quality.md) | medium | S | Workflow fixtures | Manual-run and schedule tests repeat component and scoped credential setup. |
| A2-8 | [A](code-quality.md) | medium | M | HTTP guests | Request and pagination guests repeat transport policy helpers. |
| A2-9, H3-4, H3-8 | [A](code-quality.md), [H](hygiene.md) | medium | M | Web tooling | Applications repeat Vite policy, worker naming, Solid assertions, and bundle parsing tests. |
| A2-10, H1-10 | [A](code-quality.md), [H](hygiene.md) | medium | S | Tool installation | Four pinned installers repeat one download and installation lifecycle. |
| A1-6 | [A](code-quality.md) | medium | S | Native dependencies | Forty-eight native dependency declarations have no confirmed use. |
| A1-11 | [A](code-quality.md) | medium | M | Generated support | Codec templates emit helpers and constants that particular operations never use. |
| A3-10 | [A](code-quality.md) | medium | M | Native services | Host and CDC root files combine several lifecycle owners. |
| A3-11 | [A](code-quality.md) | medium | L | Release control | Large publication, upgrade, and activation files mix sequencing with infrastructure implementation. |
| A3-12 | [A](code-quality.md) | medium | M | Generator structure | Manifest and codec files mix semantic stages and recover prior results through generated JSON. |
| A3-13 | [A](code-quality.md) | medium | M | Runtime contracts | Routing and serving-manifest files mix policy, representation, and transport mechanics. |
| C3-5 | [C](upgrade.md) | medium | L | Upgrade model | Maintenance upgrades can remove online stages, version minimums, progress storage, and inferred rollback. |
| D1-2 | [D](deployment.md) | medium | M | Cloud destinations | Telemetry and certificate rendering retain installation-specific values instead of declared destinations. Lowered from high by the owner review. |
| D1-4 | [D](deployment.md) | medium | M | Workload variants | Historical HTTP and materializer workloads preserve a second deployment model. |
| D1-5 | [D](deployment.md) | medium | M | Host policy | Local development embeds Receiving cloud limits for every application. |
| D1-6 | [D](deployment.md) | medium | M | CDC artifact | Production CDC uses the checks image. The reader executable moves into the host image and the separate stage is deleted. |
| D1-7 | [D](deployment.md) | medium | S | Operator pin | Host and operator installation select the upstream chart independently. |
| D1-8 | [D](deployment.md) | medium | M | Administrative Jobs | Manual invitation and PAT Jobs freeze an older runner. |
| D2-3 | [D](deployment.md) | medium | M | Platform publication | Failed publication can leave version tags for an incomplete platform artifact set. Lowered from high by the owner review. Push by digest, tag the set after the last push. |
| D2-5, D3-5 | [D](deployment.md) | medium | L | Resource rendering | Helm, edited YAML, and Rust object builders compete to define deployed resources. D2-5 is superseded by the D3-5 ruling. Only the stderr-prefix classification in `kubectl.rs` survives, fixed inline. |
| D2-8, G3-7 | [D](deployment.md), [G](tests.md) | medium | S | Retained results | Successful logs disappear, and no result joins preparation, testing, publication, qualification, and readiness timing. |
| F1-2 | [F](developer-experience.md) | medium | M | Handler contracts | Custom Rust types and adapters repeat manifest request, result, and error definitions. |
| F1-4 | [F](developer-experience.md) | medium | M | Overlay contracts | Acme reauthors its pinned base operation contract and conversion code. |
| F1-5 | [F](developer-experience.md) | medium | M | Projections | Single-statement projections require handwritten field-copy handlers. |
| F2-7 | [F](developer-experience.md) | medium | M | Checkout preparation | Preparation builds unrelated applications, omits required runtime inputs, and can hide failure through `eval`. |
| F2-8 | [F](developer-experience.md) | medium | S | Prerequisites | Required host tools appear across several late failure points. |
| F2-9 | [F](developer-experience.md) | medium | S | Local telemetry | Collector startup assumes a cached image and an available fixed port. |
| F2-10 | [F](developer-experience.md) | medium | M | Test preparation | Local business tests need a separate manual artifact and service recipe. |
| F3-7 | [F](developer-experience.md) | medium | M | Author acceptance | No test establishes the proposed ten-minute path from a new application to authenticated deployment. |
| G1-4 | [G](tests.md) | medium | S | Database URLs | Credential and database replacement use several parsers and string-splitting implementations. |
| G1-6 | [G](tests.md) | medium | S | Private files | Nine helpers share a name but disagree about exclusive creation and replacement. |
| G1-9 | [G](tests.md) | medium | S | Admission fixtures | Three artifact tests repeat one admitted-component builder. |
| H1-3 | [H](hygiene.md) | medium | S | Descriptor diagnostic | A two-case diagnostic requires four passes and therefore cannot succeed. |
| H1-5 | [H](hygiene.md) | medium | M | Identity tests | Separate shell runners duplicate identity test orchestration and retain the old evidence-directory rule. |
| H1-7 | [H](hygiene.md) | medium | M | Benchmark results | Compact summaries discard source identity, workload details, conflicts, and errors. |
| H2-3 | [H](hygiene.md) | medium | S | Plan retirement | The previous audit recommends blanket archival of plans that retain unresolved work. |
| H2-4 | [H](hygiene.md) | medium | S | Agent instructions | Standing repository rules remain inside a completed deployment plan. |
| H2-5 | [H](hygiene.md) | medium | S | Documentation links | Active pages contain missing source paths and retired anchors. |
| H2-6 | [H](hygiene.md) | medium | M | Historical evidence | Twelve historical links present unavailable artifacts as inspectable evidence. |
| H3-3 | [H](hygiene.md) | medium | M | Web boundaries | Source aliases conceal undeclared dependencies and private package imports. |
| H3-5 | [H](hygiene.md) | medium | M | Shared presentation | The tooling package owns application sync UI despite its documented role. |
| H3-6 | [H](hygiene.md) | medium | S | Web lint | Equivalent Solid application code receives different rules, and generated exclusions are incomplete. |
| H3-7 | [H](hygiene.md) | medium | M | Browser fixtures | Scenario drivers repeat authentication, interaction, forwarding, and capture support. |
| H3-10 | [H](hygiene.md) | low | S | Tracker files | An obsolete export and an empty temporary file retain about 7.6 MB beside the canonical export. |
| H2-7 | [H](hygiene.md) | low | M | Historical snapshots | Three overlapping history snapshots contain 3,891 lines beside current reference owners. |
| H1-12 | [H](hygiene.md) | low | S | Retired tools | An applied rename tool and unused check build/removal pair retain 653 lines. |
| A1-3 | [A](code-quality.md) | low | S | Recovery helpers | Most operator-recovery helpers have no caller beyond their own tests. |
| A2-13, G1-8 | [A](code-quality.md), [G](tests.md) | low | M | Test filesystem | Tests repeat repository discovery, temporary-directory ownership, and executable creation. |
| A1-4 | [A](code-quality.md) | low | S | Pool observability | Public pool aggregation and metric-registration APIs have no repository caller. |
| A1-5 | [A](code-quality.md) | low | S | Engine conversion | The reverse native context decoder has no caller. |
| A1-7 | [A](code-quality.md) | low | S | Generator APIs | Five convenience methods have no repository caller. |
| A1-8 | [A](code-quality.md) | low | S | Runtime APIs | Five authority and wiring convenience APIs have no repository caller. |
| A1-9 | [A](code-quality.md) | low | S | Credential placeholder | An unused Kubernetes credential provider always returns an unimplemented error. |
| A2-12 | [A](code-quality.md) | low | S | Digest spelling | Raw-byte digest helpers independently implement one wire spelling. |
| A2-14, H1-11 | [A](code-quality.md), [H](hygiene.md) | low | S | Shell plumbing | Repository launchers repeat failure handling, discovery, lookup, and command printing. |
| E1-10 | [E](security.md) | low | S | Unused authority | Application API-key schema, history, and tests preserve a credential path that runtime does not read. |
| A1-10 | [A](code-quality.md) | low | S | Guest dependencies | Nine guest dependency declarations have no confirmed use. |
| A2-11 | [A](code-quality.md) | low | S | SQL quoting | Callers duplicate identifier quoting despite an existing owner. |
| A2-15 | [A](code-quality.md) | low | S | Empty declarations | Four empty attachment documents remain because readers disagree about absence. |
| A1-13 | [A](code-quality.md) | low | M | Public surface | Router, SQLite, and materializer implementation details remain public without external consumers. |
| A3-14 | [A](code-quality.md) | low | S | Lint overrides | Handwritten and generated code do not consistently follow the stated `expect` policy. |
| B1-7 | [B](coupling.md) | low | M | Architecture check | Dependency enforcement omits workspaces, development dependency links, and relevant feature distinctions. Rejected as written. One check rule per Group 4 crate move, nothing before. |
| C2-10 | [C](upgrade.md) | low | M | Schema prerequisites | Several lifecycle phases repeat project migration and platform-extension responsibilities. In passing: move platform-extension installation into substrate setup. |
| D3-7 | [D](deployment.md) | low | M | Cluster locking | A local installation lock cannot serialize different deployment machines. Re-scoped to a Kubernetes Lease held by the installer. |
| D1-9 | [D](deployment.md) | low | S | Orphan examples | Unused deployment examples and stale executor prose remain beside an active NATS diagnostic. |
| D1-10 | [D](deployment.md) | low | M | Prototype resources | Generic tests and examples preserve obsolete database, registry, credential, and retention deployments. |
| F2-11 | [F](developer-experience.md) | low | S | Development timing | The supplied evidence contains no complete checkout-to-application or saved-edit timeline. |
| G1-10 | [G](tests.md) | low | S | Router fixtures | Three targets repeat graph constructors. |
| G1-11 | [G](tests.md) | low | S | Role fixtures | Tests repeat generation-state observations and disposable-role cleanup SQL. |
| G1-12 | [G](tests.md) | low | S | Credential doubles | Three local recording stores implement the same behavior. |
| G1-13 | [G](tests.md) | low | S | Offline fixtures | Plugin tests repeat offline PostgreSQL and empty connection configuration. |
| G1-14 | [G](tests.md) | low | S | Package copies | Three test helpers independently implement recursive package copying. |
| G1-15 | [G](tests.md) | low | S | Small test helpers | Ready-only polling and Unix-second conversion repeat across tests. |
| G2-6 | [G](tests.md) | low | S | Incidental counts | Table totals and a publication count freeze implementation choices without a named contract. |
| G2-7 | [G](tests.md) | low | S | SQL snapshots | A complete formatted DDL snapshot repeats behavior already covered by semantic tests. |
| H2-8 | [H](hygiene.md) | low | S | Documentation navigation | The plan index omits alignment work, and history lacks a successor map. |
| H3-9 | [H](hygiene.md) | low | S | Web exports | Three unnecessary exports and one root dependency can leave without deleting their active implementations. |

### Rejected findings and proposals

The owner review of 2026-10-31 rejects the following. The report evidence stays in the report files. A rejected row becomes no issue. The narrowed row keeps only the part its ruling names.

| Finding or proposal | Ruling |
| --- | --- |
| C3-5: Maintenance upgrades remove the need for online stages and inferred rollback | Maintenance-window lifecycle. Deployment state stays in Helm and Kubernetes, rollback is `helm rollback`, and an upgrade never stops user workflows. Keep the environment lock and the rolling drain of the old release. Drop the writer pause, maintenance windows and database-recorded rollout state. |
| C2-8: The declared contract-version minimum path has no executor | Online stages and version minimums are not replaced by a maintenance window. Same ruling as C3-5. `environment/contract.rs` and the contract admission in `qualify_upgrade/stage.rs` go with the package-baseline deletion. |
| Package upgrades (owner ruling 2026-10-31) | Owner ruling 2026-10-31: one baseline covers application packages as well as the platform schema. Migrations are a future problem, because current greenfield work is high churn and replacement. A package's SQL is one baseline. A package schema change during development is a new release and a reset of that application's tables. There is no in-place package upgrade. The machinery is deleted now as a Group 1 row (about 9,000 lines). |
| C2-3: Env apply cannot initiate an installed-package upgrade | Closed by the package-baseline deletion. The staged-successor branch and the successor evidence path go with it. No additive-upgrade test is written. |
| C2-6: Commit selection does not select the schema executor | Closed by C3-3. The commit-build coordinator is deleted. |
| C3-4: Qualification covers separate slices instead of the complete transition | Closed. Nothing to qualify under the package-baseline ruling, and R12 keeps release plus host image. |
| C1-1: Schema truth describes fresh installations but cannot advance installed databases | Migration machinery. The schema is squashed to one baseline during development and a reset is free. No migration streams, no executor. Deletes inverted: the full-schema installation path stays, and the machinery that serves only the deferred path is deleted now (`crates/control/provision/src/schema_migrations.rs` (both empty lists), `crates/control/lib/src/upgrade_schema.rs` and the `upgrade-schema` verb in `services/ctl/src/provisioning_verbs.rs`, `record_fresh_install` and its three callers, the migration record table `platform.schema_migrations` in `deploy/sql/app-schema-migrations.sql`, and their tests). Real migrations return when development is stable. |
| C3-2: Fresh installation and reconciliation bypass versioned migrations | Migration machinery. Same ruling as C1-1. |
| C1-4: A failed control bootstrap can leave an installation that refuses every retry (narrowed, stays in Group 1) | Baseline histories and refusal of altered or newer records are rejected with C1-1. The retry part stays. Simplest fix: bootstrap runs its batches in one transaction, and a retry that finds `registry` without a completed bootstrap drops and recreates the system database. No lock, no recorded stages. |
| E3-1: Registry descriptors can bypass byte admission | Downgraded from high to medium. Publication trusts the registry and WIT. Registry write access is the trusted boundary. |
| The 51-epic chain and its effort table | Replaced by the four ordered groups below. Every effort row was M or L, so the table carried no information. |
| D1-3: The RC path installs an incomplete collector variant | Closed by D3-6. The RC Dockerfile and the RC path are deleted in Group 1, so no separate collector fix is filed. |
| D1-2: Cloud rendering ignores declared destination values | Downgraded from high to medium and moved to Group 1 In passing. `wamn-dev` is the only cluster. Fix when the collector and certificate renderers are next open. |
| D2-1: A clean upgrade lacks the application builds that it consumes | Closed by C3-3. `upgrade_environment.rs` is deleted in Group 1, so no clean-worktree upgrade test is written for a deleted path. |
| D2-5: Deployment ownership is spread across CLI tools and Helm history | Superseded by D3-5. No Rust reconciler, no Kubernetes client port, no desired/observed store. The stderr-prefix transport classification at `kubectl.rs:26` is fixed inline when the file is next open. |
| D2-3: Cloud failures leave published version tags without a complete platform set | Downgraded from high to medium and moved to Group 1 In passing. Builds are local by rule and Cloud Build is overflow only. Push every image by digest and write the version tags after the last push. One test, no fault injection per stage. |
| Part 2 preamble: kubectl to Kubernetes client, charts to typed Rust resources, gcloud to provider API calls, operator-owned rollout | Deleted from the report. This is the operator model that D3-5 rejects. |
| D3-2: the wider qualification key (chart, identity, edge, runner, qualification executable) | Rejected. Every chart edit would re-qualify every release, which is the stated cost problem. The key is release digest plus host image digest. Chart, identity, edge and runner changes are proved by `platform_apply_kind` on the branch that touched them. D3-2 is medium and deletes only the `ImageSet` versus `QualificationImages` split. |
| D3-3: CI service, protected reference, promotion job, "merge only the tested commit or restart" | Rejected. Merges are fast-forward after reviewer acceptance. A branch that touches install, chart or env apply files merges after `platform_apply_kind` passes on that branch. Builds are local. D3-3 closes into D2-8 (keep successful logs). The train-tools-test cases are dropped. |
| D3-7: deployment queue, promotion job, protected selection | Re-scoped to a Kubernetes Lease held by the installer, released on exit, refused while held. Severity low. Tests: two installers with one refused, and lock loss stops the installer. |
| Question 8 (retained database data) | Answered: no installed database retains data. |
| Question 10 (maintenance windows, version-minimum executor) | Answered by the C3-5 ruling. |
| Question on external consumers of prototype manifests (H3-2, D1-9, D1-10) | Answered: nothing is applied outside repository automation. Delete without confirming external use. |
| Question on CDC image identity (D1-6) | Moot: the reader executable moves into the host image, so no sixth image exists. |
| Question on `upgrade-environment` support (B2-1, C2-2, C2-7, D2-1) | Answered: not supported, deleted. |
| Question on kind and GCP builds (D2-2) | Answered: one build graph, local by default, Cloud Build as overflow under the daily cap. Kind and GCP consume the same published artifacts. |
| Question on build budgets (D2-8) | Answered: no budgets until the build record has produced numbers. |
| Question on the CI service and protected reference (D3-3) | Answered: none. The owner's machine and the agents run builds and installs, and merges are fast-forward. |
| Question on a disposable GKE project (D3-6, D3-3, G3-3) | Answered: none. Kind is the qualification provider. GKE-only behavior is proved on wamn-dev under manual approval. |
| Question on widening R12 (C3-4, D3-2) | Answered: no. R12 keeps the narrow key, release plus host image. |
| Question on `wamn-m6or2` (C1-2) | Answered: it closes on the next green `environment_apply_kind` run on main. No dedicated run. |
| Question on `apply-package` as an internal phase (C2-2) | Answered: yes, internal phase of `env apply`, no public command. |
| Question on contract version minimums (C2-8) | Answered: unsupported. The contract-phase code is deleted. |
| Question on maintenance windows (C3-5) | Answered: no. Old and new images serve concurrently. |
| B1-7: The graph check does not enforce the complete architecture | Rejected as written. No extension to three workspaces, dependency kinds and feature selections, no deliberate-violation fixtures. `tools/repo-lint-dependencies` gains one rule per crate move that lands in Group 4, nothing before. |
| B1-4: The control hub combines independent tools in one compilation unit | Deferred. Group 1 deletes about a fifth of control (`upgrade_environment.rs`, `package_upgrade`, `qualify_upgrade`, the platform build stages). Re-read the crate after that. The split may not be needed. |
| Question on timings for the six development edges (B1-2) | Answered: none are supplied. The next test build is the measurement. |
| Question on a shared contract layer below generation, provisioning and execution-host (B1-1) | Answered: yes, and it is `crates/catalog/model` today. |
| A delivery tool or `tools/delivery` (coupling Part 2) | There is none. `docs/plan/delivery.md` is deferred and there is no PR-gated CI. The owner of build, publish and qualify is the existing `wamn-ctl` verb (`build-platform`, `publish-release`, `qualify-release`) run locally or by an agent. |
| B2-3: Package application depends on the qualification runner it must serve | Closed by the package-baseline deletion. The package-control owner is the Group 4 crate map. |
| B2-5: Developer orchestration is reusable, but belongs to the developer application | Deferred with B1-4. Owner question: may `wamn dev` own the development engine? |
| Question on who approves qualification cases (B2-6, F3-5) | Answered: no approver. The application declares its own cases in its package. |
| Question on external consumers of the `wamn-control` API (B2-5) | Answered: none. The umbrella may go without a facade, in Group 4. |
| B3-2 T13: one package for `wamn-host` and `wamn-scenario-worker` | Rejected. They stay separate packages. One manifest saves nothing and widens feature selection. |
| B3-2 T11: `wamn-control` absorbs `wamn-ctl` | Deferred with B1-4. The README split (package, release and environment control plus `wamn-ctl` and `wamn-dev`) and this merge both wait for the re-read after the Group 1 deletions. |
| "Halve the package count" and the 88 to 44 arithmetic | Dropped. Package count is not a goal. Delete a package when it has one consumer or no enforced boundary. Keep one when `tools/repo-lint-dependencies` enforces something through it. |
| Question on Cargo package names (B3-2) | Answered: no external automation depends on them. |
| Question on one qualification cycle for guest moves (B3-3 through B3-5) | Answered: yes. A guest repin is a new release digest, and one `qualify-release` run covers it. |
| C1-2's pending-work record and C1-3's schema advancement before analysis | Rejected. One baseline, so nothing advances. Apply compares the installed baseline hash with the candidate's and refuses with "schema changed, reset the environment database" on a mismatch. C1-3 closes into C1-2 with its Deletes widened to every project migration call. |

### Plans in flight

Two plans change the platform the audit describes: [alignment](../plan/alignment.md) (One NATS R1 to R10, Edge and CDN C1 to C5, WASI match W1 to W3, deploy failures) and [integration-schema](../plan/integration-schema.md). A finding a plan already settles is closed by the plan, not re-proposed. Each row cites the ruling that closes or reshapes it. The plan's epic owns the change. Group 1 is control-crate and YAML work, so it rides the One NATS platform build. No Group 1 row forces its own build (alignment, Build cost and tests).

| Finding | Ruling | Effect |
| --- | --- | --- |
| D1-4 | alignment, One NATS Deleted | `flow-http.yaml`, `materializer.yaml` and `wms-materializer.yaml` are the plan's deletions. The audit adds only `wms-flow-http.yaml` and the snapshot generator. |
| D1-2 | alignment C1 | The `letsencrypt.yaml` and DNS-project part is moot under the Google-managed certificate. Only the collector destination part survives, In passing. |
| D1-9 | alignment R8 | `evt-nats-check.yaml` goes with `evt-nats`. No NATS diagnostic is retained. |
| Variant inventory | alignment, One NATS Deleted | `auth-nats-clients.yaml`, `host-environment-certs.example.yaml`, `wasmcloud-ca-issuer.yaml`, the `evt-nats-authorization` Secret and its generator are plan deletions. |
| E1-2 | alignment R4, R5, R7 | Closed. Per-environment client certificates and key prefixes `$KV.wamn-auth.<env_key>.>`, proved by R5's two-environment test. The audit proposal (drop the bucket, read grants from Postgres) is rejected. The bucket stays. |
| E2-4 | alignment R1, R3, R4 | Closed. TLS on, cert-manager CA, certificate-mapped users. The 365-day CA question is answered by R3. |
| E1-8 | none | Unchanged. Not covered by either plan. |
| E3-6 | integration-schema, Runtime refresh | Reshaped. Compile once per digest, link per generation, calls in flight finish. Retention bounds are set there. Nothing separate is filed. |
| E2-5, A3-8 | integration-schema, Dropped or merged from main | Re-scoped. `registry.credential_import`, `import_connection_credentials` and `identity.credential.secret` are dropped, and credentials enter by UI or API only. E2-5 keeps the MinIO root and backup root identity (the Postgres fixture goes with D1-10). A3-8 keeps the five database-credential validators. |
| E3-1 | alignment W1 to W3 | Admission matches family and ceiling. Any E3-1 work waits for W1 to W3 and must not re-pin exact versions. |
| B2-2 | integration-schema, binding is the permission | Re-scoped to the role to operation ACL. `integration_allow` and `project_connection` are dropped. |
| C2-7 | C3-3 and integration-schema, Admin flow | Closed. The upgrade path is deleted. What survives is the plan's admin flow, which deploys nothing. |
| A3-11 and any row naming `connection_instances`, `connection_bindings`, `org_connections`, `integration_allow` or `wamn:connector` | integration-schema, Dropped or merged from main | Rename per that table before filing. |
| Question on certificate renewal and the 365-day CA (E2-3, E2-4) | alignment R3 | Answered: cert-manager owns the CA and renews it. |
| Question on org isolation under host compromise (E1-2) | alignment R5 | Answered: per-environment certificates, refused across environments. |
| B1-3 and every crate move inside runtime or execution-host | [perf-arch](../plan/perf-arch.md) A5 | The application as the HTTP workload is the ruled direction and lands before any re-measurement. Moves inside runtime and execution-host before A5 are churn and wait. |

## The six largest areas for improvement

### 1. Authority, credential isolation, and resource limits

Supporting findings: E1-1 through E1-9, E2-1 through E2-8, E3-1 through E3-8, A3-1, and A3-2.

The refreshed dependency measurement records 17 vulnerabilities and 12 warnings (`docs/audit/measurements.md:391`). Secret names concatenate coordinate parts ambiguously (`crates/identity/credential-store/src/kubernetes.rs:21`). Identifier rules already reserve double hyphens as separators (`crates/control/registry/src/identifiers.rs:64`).

Grant fan-out uses process-local coordination (`crates/execution/host/src/control_route.rs:323`, `:685`). Shared cache entries reach authorization through `crates/platform/runtime/src/auth_cache.rs:292`. Production database clients use plaintext transport (`services/identity/src/lib.rs:165`, `services/host/src/host.rs:782`). Publication converts carried descriptors into admitted facts (`crates/control/lib/src/publish_release.rs:1621`).

Delete ambiguous naming formulas, shared grant-value replication, broad cross-org writes, plaintext defaults, unchecked admission assumptions, and unbounded artifact buffers. The expected effect is narrower org authority and bounded failure, with explicit tests for cross-org access, revocation, transport refusal, and resource exhaustion.

### 2. One lifecycle and one schema path

Supporting findings: B2-1 through B2-4, C1-1 through C1-6, C2-1 through C2-10, and C3-2 through C3-5.

The control crate contains 80,688 source lines, 32,437 test lines, and 71 `Command::new` expressions (`docs/audit/measurements.md:57`). The upgrade driver alone spans 2,879 lines under A3-11. It applies packages at `crates/control/lib/src/upgrade_environment.rs:784`, then enters locked environment application at `:984`.

Platform installation forces `release = none` during its initial application (`crates/control/lib/src/platform/environments.rs:46`). Environment analysis precedes schema preparation (`crates/control/lib/src/environment/mod.rs:276`). Both platform migration streams are empty (`crates/control/provision/src/schema_migrations.rs:93`, `:96`). This last behavior is deliberate under `docs/architecture/overview.md:67`, rather than an accidental missing implementation.

Delete the independent upgrade driver, mutable completion shortcuts, synthetic uninstall pass, and competing structural repair path. The owner rejected the maintenance model (C3-5, C2-8): deployment state stays in Helm and Kubernetes, rollback is `helm rollback`, and an upgrade never stops user workflows.

The owner rejected the migration machinery (C1-1, C3-2) and the baseline-history part of C1-4. The schema is squashed to one baseline during development and a reset is free, because no installed database retains data. The migration machinery is deleted now under the inverted C1-1 Deletes, C1-6 closes into C1-1, and C1-5 stays as a deletion only. C1-2 becomes a baseline content hash that install records and apply compares. The owner ruled the same for application packages, so the package-upgrade machinery (lineage, evidence, stages, qualification, contract) is deleted now.

### 3. One artifact build and deployment path

Supporting findings: C3-1, C3-4, D1-1 through D1-8, D2-1 through D2-8, D3-1 through D3-7, H1-1, and H3-2.

The baseline contains 83 YAML files and 46,235 YAML lines (`docs/audit/measurements.md:13`). Those totals include pinned upstream bundles and are not a deletion budget. The release chart changed in 111 commits, while its Rust owner changed in 110 (`docs/audit/measurements.md:821`, `:822`). Receiving and WMS lifecycle scripts share 78 detected lines (`:711`).

WMS KCL declares four application packages, while its JSON sidecar supplies only WMS (`deploy/gcp/environments/dkk--wms--dev.k:16`, `deploy/gcp/environments/dkk--wms--dev.json:11`). Production CDC selects the checks image (`crates/control/lib/src/platform/cdc.rs:21`). Qualification excludes runner, checks, and identity images from its key (`crates/control/lib/src/release_chart.rs:54`).

Delete environment JSON sidecars, the older image builder, deployment-time compilation, historical workload snapshots, and independent fixture installers. The expected effect is one qualified artifact selection that both kind and GKE consume. This is Group 1.

D2 recommends a Rust operator, while D3 recommends Helm plus KCL. Source confirms an existing bounded lifecycle command (`crates/control/lib/src/environment/mod.rs:271`) and mixed resource construction (`crates/control/lib/src/platform/cdc.rs:78`). It supports the shared defect, but establishes no requirement for continuous reconciliation. This synthesis selects D3’s smaller Helm and KCL proposal.

### 4. Shared contracts below tools, with fewer competing implementations

Supporting findings: A1-1, A1-2, A1-6, A1-10 through A1-13, A2-1 through A2-8, A3-1 through A3-13, B1-1 through B1-7, and B3-1 through B3-6.

The baseline records 80,688 control source lines, 45,267 runtime source lines, and 34,991 generator source lines (`docs/audit/measurements.md:57`, `:58`, `:59`). Copy detection reports 592 matches and 8,548 duplicated lines, including tests and overlapping matches (`:680`).

The execution host returns a generator-owned release-profile type during requests (`crates/execution/host/src/control_route.rs:1126`). Provisioning also consumes generator declarations (`crates/control/provision/src/operation_grants.rs:14`). Host contracts contain 71 redundant files and 3,020 repeated lines, with an estimated 2,800-line net saving under A2-3. Dormant startup and wiring-cache code contribute approximately 827 and 574 deletion lines under A1-1 and A1-2.

Delete compiler dependencies from serving owners, redundant contract files, unused APIs, and 48 native plus nine guest dependency declarations. Consolidate shared semantics and boundary errors so each retained contract has one implementation, without claiming unmeasured build-time gains.

B1 recommends a narrow artifact owner, while B3 folds artifact transport into runtime. The source explicitly keeps package-artifact independent of engine and runtime (`crates/catalog/package-artifact/src/lib.rs:15`). It supports B1’s boundary, which the target retains.

B1 also argues for retaining label-template because of native test conflicts. Current label-render disables its guest runtime during native tests (`apps/platform/no-std/label-render/src/lib.rs:44`, `:51`). That source supports B3-4’s module consolidation, with the golden vectors retained.

### 5. Manifest-and-SQL authoring with a working saved-edit loop

Supporting findings: F1-1 through F1-8, F2-1 through F2-11, F3-1 through F3-7, H3-1, and H3-3 through H3-8.

The six-application inventory contains 224 files and 39,103 physical lines. Tests account for 26,357 lines, or 67.4% (`docs/audit/developer-experience.md:9`, `:26`). The narrow scaffolding family contains 22 files and 450 lines. The broader 3,800-line candidate family includes business behavior and is not a deletion estimate (`:28`, `:30`).

The fixture manifest declares an `int32` revision, while its authored component schema requires a string (`apps/platform_fixture/wamn.k:177`, `apps/platform_fixture/publication/components/fixture.json.in:79`). Generation distinguishes numeric `int32` from string `int64` revisions (`crates/schema/generator/src/generate/wit.rs:450`). The watch filter omits ordinary application source directories (`crates/control/lib/src/dev/watch.rs:265`).

Delete mandatory component wrappers, statement includes, repeated custom contracts, generic screen adapters, and routine infrastructure arguments. The expected effect is an author who writes application declarations and business behavior while the platform prepares, watches, builds, and activates their dependency closure.

### 6. Tests, tools, and documentation that establish current behavior

Supporting findings: A2-6, A2-7, A2-13, B1-2, B3-6, G1-1 through G1-15, G2-1 through G2-8, G3-1 through G3-7, and H1-2 through H3-10.

Direct all-target linting produced 16 compilation errors from absent generated application files (`docs/audit/measurements.md:128`). Copy detection records 201 duplicated lines between Receiving and WMS tests (`:697`). G2 counts 136 first-party ignore attributes, including seven quarantines and one subprocess helper (`docs/audit/tests.md:292`).

The merge queue ends at source-test execution and deletes successful logs (`tools/train-run:60`, `:80`). A standalone descriptor diagnostic creates two cases but requires four passes (`tools/native-descriptor-limit-check:129`, `:158`, `:167`). H2-7 identifies 3,891 lines in three overlapping historical snapshots. H3-10 identifies about 7.6 MB in obsolete tracker files.

Delete repeated fixture mechanics, incidental snapshots, dead command surfaces, obsolete exports, and completed plan copies after preserving their unique requirements. The expected effect is explicit test selection, independently compilable cheap targets, retained execution results, and one current documentation owner for each contract.

## Proposed target architecture

### Crate map

This map combines B3’s complete inventory with B1-3’s artifact boundary and B2’s package, release, environment, developer, and delivery ownership. Package count is not a goal: a package goes when it has one consumer or no enforced boundary, and stays when `tools/repo-lint-dependencies` enforces something through it. These are proposed ownership changes, not claims that the current arrangement violates its documented design.

| Target crates | Source consolidation |
| --- | --- |
| `wamn-contract` | Pure catalog, authoring, registry, component policy, numbering, project identity, SQL vocabulary, execution, event, history, manifest, and schema contracts. Canonical WIT lives with these contracts. |
| `wamn-guest-sdk` | Shared data-access helpers and typed PostgreSQL statements. |
| `wamn-session`, `wamn-run-state`, `wamn-engine` | Retain separate verification, storage-decision, and execution boundaries. |
| `wamn-provision` | Combine control provisioning and schema-control declarations and plans. |
| `wamn-identity` | Combine identity persistence, credential storage, identity client, and identity service targets. |
| `wamn-package-artifact` | Retain verified artifact transport, registry credentials, and trust roots. Move duplicate runtime transfer code here. |
| `wamn-runtime`, `wamn-workflow` | Merge execution-host into runtime. Merge router algorithms into workflow as private pure modules. The runtime merge waits for the A5 inversion, whose import boundary decides the merged crate. |
| `wamn-build` | Combine generation, introspection, composition, and virtualization tools. |
| `wamn-package-control`, `wamn-release-control`, `wamn-environment-control` | Replace the control umbrella with the sole package executor, release construction, and locked lifecycle owner. Deferred with B1-4 until the Group 1 deletions land. |
| `wamn-ctl`, `wamn-dev` | Keep administrative and delivery commands in the CLI application. Move the developer engine and `wamn` binary to `services/dev`. |
| `wamn-local`, `wamn-host`, `wamn-scenario-worker` | Fold SQLite storage into the local runtime crate (today `wamn-edge`; the name follows the repository rule against `edge` and `cloud` in names). `wamn-host` and `wamn-scenario-worker` stay separate packages (T13 rejected). |
| `wamn-cdc-reader`, `wamn-client`, `wamn-test-postgres` | Retain separate replication, native client, and private PostgreSQL lifetime owners. |
| `wamn-test-support`, `wamn-test` | Combine test support, infrastructure, and fixture-package. Combine test/tool packages into explicit targets, with application sources kept beside their apps. |
| `receiving`, `wms`, `client-acme-receiving`, `device`, `samples`, `modbus-reader`, `netsuite`, `rest-connector`, `rest-consumer-fixture`, `wamn-integration`, `fixture`, `fixture-overlay` | Retain application component identities. Fold the five application data crates into their owning components. |
| `http-route`, `materializer`, `blob-put`, `jsonata-expression`, `wamn-command-fixtures`, `connection-http-standard`, `sqlx-command`, `std-virtualization-probe` | Retain capability-specific artifacts. Combine busyloop and sockprobe package ownership. Fold the SQLx driver into its fixture. |
| `http-page`, `http-request`, `label-render`, `transform`, `wamn-modbus` | Retain the isolated `no_std` workspace. Fold label-template into label-render and move Modbus into this workspace. |
| `pg_walstream` | Retain the vendor dependency until its required upstream API exists. |

Migration order is contracts, serving consumers, provisioning, build tools, identity, artifact/runtime/workflow owners, control applications, guests, then test packages. Update graph checks with each move. Rebuild and repin changed guests. Retain the three first-party workspaces and remove empty compatibility packages. Sources: B1-1 through B1-7, B2-3 through B2-9, B3-1 through B3-6, A1-2, and A1-12.

### Lifecycle model

Use `apply`, `apply --dry-run`, `show`, `status`, and `delete` as the common lifecycle surface. Platform and environment commands select scope. Package and schema changes run as internal phases of that reconciler, with no independent production mutation route.

`cluster.k` selects a platform bundle by digest. `environment.k` selects the application release. The release fixes package and overlay artifacts. The bundle carries one schema baseline. Helm and Kubernetes provide observations, not independent desired versions.

One cluster-visible lock coordinates installation and environment locks cover every target mutation. Desired state is the selected bundle and release. Observed state is Helm and Kubernetes. Rollback is `helm rollback`. An upgrade never stops user workflows. The rolling drain of the old release stays: releases coexist by digest, expand runs before the Helm write and contract runs after the drain ([integration-schema](../plan/integration-schema.md), Deployment). There is no writer pause, no maintenance window and no database-recorded rollout state. The schema is one baseline during development, and a reset is free. The same holds for application packages: a package's SQL is one baseline, a package schema change during development is a new release and a reset of that application's tables, and there is no in-place package upgrade. Sources: B2-1 through B2-4, C1-2, C1-3, C2-1 through C2-7, C2-9, C3-1, C3-3, C3-4, and D3-7. The owner rejected C3-5, C2-8, C1-1 and C3-2 and narrowed C1-4 (see Rejected findings and proposals).

### Deployment path

The path is commit and submodule pins, selected tests, one local artifact build, complete bundle publication, disposable qualification on kind, then `platform apply`. Merges are fast-forward after reviewer acceptance, and a branch that touches install, chart or env apply files merges after `platform_apply_kind` passes on it. Application authors reach the same lifecycle through `wamn deploy`.

Keep the root Dockerfile, Helm, KCL, pinned upstream operators, and separate platform/environment Helm instances. Use one WAMN chart package with kind and GKE values, folding only the WAMN-owned templates (alignment R9 keeps the operator, CRDs, `WorkloadDeployment`s and the subchart host Deployment). The CDC reader lives in the host image. The qualification key is release digest plus host image digest.

Delete the older upgrade builder, RC Dockerfile, environment sidecars, deployment-time builds, manual installation implementation, and independent journey installers. Retain their behavioral assertions in one disposable fixture. Native development remains an editing path. Sources: C3-1, C3-4, D1-1 through D1-8, D2-1 through D2-8, D3-1 through D3-7, and G3-3.

### Authoring path

For ordinary CRUD, the author writes `wamn.k`, an initial SQL migration, acceptance cases, and seed data. Custom Rust implements business decisions. Custom screens implement application workflows.

The platform generates boundary types, SQL access, WIT adapters, component assembly, publication schemas, clients, and standard screens. An overlay consumes its pinned base contract without copying its shape.

Bare `wamn dev` owns a private service stack, credentials, ports, selected builds, and watching. Source changes reach generation before freshness admission. Compatible candidate failures leave the active application available. `wamn deploy` publishes, qualifies, uploads browser assets, and applies exact digests.

The proposed 600-second journey begins with the installed SDK, Docker, and a provisioned account. Its duration remains unmeasured. Sources: F1-1 through F1-8, F2-1 through F2-11, F3-1 through F3-7, and H3-1 through H3-8.

## Open beads the proposals make moot

These rows use exported `open` or `in_progress` status from `.beads/issues.jsonl`. They propose replacement or disposition, not closure. Requirements that survive deletion remain acceptance criteria for the replacement. CLAUDE.md requires an implementation commit or cited evidence before closure.

| Open bead | Exact exported title | Finding or proposal | Why its current implementation premise disappears |
| --- | --- | --- | --- |
| wamn-j7qz7 | Delete values-host-{receiving,wms}-pat.yaml and values-host-default.yaml when the kind cluster tests run on an env apply environment | D1-4, D1-10, D3-6 | Shared lifecycle fixtures replace the PAT overlays and their separate installation path. Export: `.beads/issues.jsonl:2075`. |
| wamn-qpgq | reconcile-run-plane makes schema changes in code; they move under upgrade-schema (docs/plan/schema-upgrade.md) | C1-5, C1-2 | The repair planner is deleted and historical cutovers are replaced by a reset. Acceptance is the baseline hash check of C1-2, because C3-2 is rejected. Export: `.beads/issues.jsonl:3469`. |
| wamn-eycl | Package upgrade Epic 4: Package upgrade in the platform UI | C3-3, C3-5 | A separate `environment.upgrade` saga disappears. There is no package upgrade in development. A release change is an `environment.k` apply, and the UI submits that document. Export: `.beads/issues.jsonl:3458`. |
| wamn-ptxoz | gcp.md §4.19.1: the platform UI version in values-edge.yaml and url-map.yaml is a hand-edited literal that drifts with every platform bump | D3-1, D3-4, D3-6 | Artifact-selected deployment removes the manually edited version carriers and alternative installation instructions. Export: `.beads/issues.jsonl:1954`. |
| wamn-litd5 | tests/system/tests/deployment_declarations.rs:1059 pins platform-ui 0.1.31, but deploy/gcp/values-edge.yaml, url-map.yaml and the release chart are 0.1.36 | D1-10, D3-4 | Tests move from historical deployment literals to current rendered resources and selected artifacts. Export: `.beads/issues.jsonl:1938`. |
| wamn-u1qym | gcp.md §4.13: names packages wamn_receiving-2.1.0 and wamn_wms-2.1.0, not the current package versions | D3-1, D3-4, D3-6 | Deployment consumes the release’s package closure, so manual package-version instructions disappear. Export: `.beads/issues.jsonl:1958`. |
| wamn-klisb | gcp.md §4.8/§4.17: the scheduler NATS port-forward ended during the install and env apply failed with connection refused on 127.0.0.1:14223 | D3-6 | The common in-cluster installation path replaces the manual port-forward-dependent route. Broker recovery assertions remain. Export: `.beads/issues.jsonl:1953`. |
| wamn-ev5g6 | platform apply 4.17: the Job deadline (600 s) equals the host readiness budget, so a failed host start shows no error | C3-5 disposition evidence | This premise already differs from audited source: the outer Job budget is 900 seconds around 600-second readiness. Evidence: `crates/control/lib/src/platform/job.rs:15`. Export: `.beads/issues.jsonl:1919`. |

## Ordered work groups

The owner reviews of 2026-10-31 replace the epic chain with four ordered groups. Each group has two parts. Do holds the rows the review named and every row that deletes a build, image, route, file or test target. In passing holds helper dedup, naming, count pins and doc links, closed when the owning file is already open and never filed alone. Crate consolidation moves last. Each row copies the Deletes line of its finding from the report file. Shared finding ids close only after all listed work lands.

### Group 1: Deployment consumes, never builds

Target: `platform apply` consumes an immutable bundle by digest. No Cargo, pnpm or registry write runs inside apply. Delete `upgrade_environment.rs` and its images, the older image builder and the RC Dockerfile, the environment JSON sidecars, the Rust object builders in favor of one Helm package, and eight of the nine deployment routes in favor of one deployment fixture.

Do (27 findings). The rows the review named, then every row that deletes a build, image, route, file or test target, or carries severity high. The Image column names the images a row rebuilds: ctl/runner for the control library and Dockerfile, host as well for provisioning and schema-control (the host links both), none for YAML, tools and docs. Group 1 rides the One NATS platform build and no row forces its own.

| Finding | Title | Severity | Image | Deletes |
| --- | --- | --- | --- | --- |
| D3-1 | Installation still builds and publishes its own candidate | high | ctl/runner | Package and UI compilation from `platform/build.rs`, publication from `platform/releases.rs`, and the installer’s request to commit newly discovered release digests. From D2-6: installer PostgreSQL container management, pnpm invocation, debug executable lookup, and recursive ctl execution. |
| B2-7 | Platform installation also acts as a repository build application | medium | ctl/runner | Same Deletes as D3-1. `build-platform` stays a `wamn-ctl` verb and `platform apply` consumes its outputs. |
| D2-6 | Installation couples package freshness to an unrelated platform stamp | medium | ctl/runner | Merged with D3-1. The installer deletions are stated once under D3-1. |
| C2-2 | Production package changes escape the lifecycle lock | high | ctl/runner | Independent production package coordination in `platform/packages.rs`, the public `apply-package` verb, and every mutation path that bypasses the environment owner. Package application is an internal phase of `env apply` under the environment lock. `wamn dev` calls the library. |
| A3-11 | Release control files mix orchestration with implementation owners | medium | ctl/runner | Monolithic parent bodies, the activation error wrapper, and duplicate infrastructure helpers where ownership already exists. `upgrade_environment.rs` is deleted under C3-3, so the scope is `publish_release.rs` and `dev/activation.rs`. |
| D2-2 | The older image pipeline builds artifacts that deployment does not select | high | ctl/runner | `tools/journey-image-cache`, the upgrade image and guest stages, their leases, relabeling, and duplicate registry authentication. Listed in Group 1 for the deletion and in Group 2 for the cache identity. |
| H1-1 | The image cache omits actual build inputs | high | none | The parallel `COPIED` inventory, custom source-tag selection, and the production dependency on `tools/journey-image-cache`. |
| D1-1 | KCL and JSON declare different application sets | high | ctl/runner | Both environment JSON files, their required-sibling convention, and their package lists. Remove the old delivery configuration reader after its remaining consumers migrate. The JSON also carries `context` and `gates_image`. Both are cluster facts and belong in `deploy/gcp/cluster.k`, not in any environment file. The empty `floors = {}` block in both environment `.k` files is deleted. |
| H3-2 | Environment JSON creates a second deployment authority | high | ctl/runner | Both environment JSON files, the obsolete `EnvironmentFile` reader, and companion-JSON discovery in package preparation. |
| D3-5 | Helm plus KCL removes more machinery than a Rust operator | medium | ctl/runner | `platform/manifests.rs` substitutions, Kubernetes object builders in `platform/cdc.rs` and `platform/edge.rs`, and independent WAMN identity and edge chart packages after their templates move. Alignment R9: the operator, CRDs, `WorkloadDeployment`s and the subchart host Deployment stay as they are. Only WAMN-owned templates and the plugins and NATS values move. |
| D3-6 | Nine deployment routes must collapse into one fixture | medium | none | `tools/receiving-cluster-journey-run`, `tools/wms-cluster-journey-run`, `tools/delivery-owned`, `tools/rc-gate-run`, `tools/kind-gate-build`, `tests/integration/src/rc/Dockerfile`, and `deploy/mvp/`. Retain generic verdict and cleanup tools where callers still need them. Clusters are disposable and one agent holds a cluster at a time. Receiving and WMS assertions move into the fixture before their scripts go. |
| D2-7 | Cluster checks retain separate artifact recipes and duplicated lifecycle code | medium | none | The RC Dockerfile, redundant journey image builds, and duplicated lifecycle branches. Retire per-node check-image cleanup when disposable clusters replace retained check clusters. Merged with D3-6. |
| C1-1 | Schema truth describes fresh installations but cannot advance installed databases | high | host, ctl/runner | Inverted by the owner review. The full-schema installation path stays. Delete `crates/control/provision/src/schema_migrations.rs` (both empty lists), `crates/control/lib/src/upgrade_schema.rs` and the `upgrade-schema` verb in `services/ctl/src/provisioning_verbs.rs`, `record_fresh_install` and its three callers, the migration record table `platform.schema_migrations` in `deploy/sql/app-schema-migrations.sql`, and their tests. C1-6 closes into this row. |
| C1-2 | Installer completion records substitute for database upgrade state | high | ctl/runner | The installed-environment replay of first apply with `release = none` and completion-marker decisions about schema readiness (shared with C2-1). From C1-3: every project migration call. Fix: the baseline SQL carries a content hash, install records it, apply refuses on a mismatch with "schema changed, reset the environment database". |
| C1-4 | A failed control bootstrap can leave an installation that refuses every retry | high | ctl/runner | The `registry` existence test as the installation-completion authority and the separate unrecorded bootstrap stages. Fix: one transaction, and a retry that finds `registry` without a completed bootstrap recreates the system database. No lock, no recorded stages. |
| C2-1 | Platform setup can uninstall a serving environment | high | ctl/runner | The separate initial-apply Job, forced `DeclaredRelease::None`, and first-versus-final environment application split. |
| Package upgrades (owner ruling) | Package-upgrade machinery, about 9,000 lines in `crates/control/lib` | high | ctl/runner | `crates/control/lib/src/package_upgrade.rs` and `package_upgrade/` (lineage, accepted evidence, progress), the expand, backfill and contract stages of `apply_package/` (`stage.rs`, `backfill.rs`, `migration_policy.rs`, `abandon.rs`), `qualify_upgrade/` and the `qualify-upgrade` verb, `environment/contract.rs`, the tables `catalog.package_upgrade_qualifications`, `catalog.package_upgrade_stages` and `catalog.package_floors` in `deploy/sql/catalog-schema.sql`, their tests, and `migrations/0002+` under each application (nine files), collapsed to one `0001` per package or the generator's full SQL. Closes C2-3, C2-8, C3-4 and the C3-5 Deletes. |
| C2-9 | Env apply skips reconciliation owned by the package applier | medium | ctl/runner | The version-only skip around all package work. Derived-object reconciliation runs on every apply. |
| C3-1 | Desired state still depends on checkout and local build inputs | high | ctl/runner | Delete `deploy/gcp/environments/dkk--receiving--dev.json`, `deploy/gcp/environments/dkk--wms--dev.json`, and `crates/control/lib/src/delivery/environment.rs`. Delete deployment-time package discovery and builds from `platform/build.rs`, `platform/packages.rs`, and `platform/releases.rs`. Remove the local chart directory as an independent deployment selector. |
| C3-3 | Multiple upgrade drivers own ordering, locking, and recovery | high | ctl/runner | Delete `upgrade_environment.rs` and its sole-purpose `delivery/run_record.rs`. Delete the public `upgrade-environment` verb. Replace the numbered installer driver with shared reconciliation. Remove nested lifecycle wrappers from `platform/environments.rs` and direct installation from `platform/packages.rs`. Make `upgrade-schema`, `apply-package`, and `reconcile-run-plane` internal operations rather than alternative deployment entry points. |
| D1-4 | Historical workloads retain a second deployment model | medium | none | `deploy/gcp/wms-flow-http.yaml` and the snapshot generator in `test-support/infrastructure/examples/workload_files.rs`. The other three files are alignment deletions. Do not file them twice. |
| D1-5 | Local development embeds Receiving’s cloud host policy | medium | ctl/runner | The Receiving manifest embedding, repeated policy values across host overlays, and tests that compare two consumers of the same org-specific input. |
| D1-6 | Production CDC uses the check image | medium | host, ctl/runner | Both historical CDC YAML files, the redundant `event-reader-rbac.yaml`, the `cdc-reader` Dockerfile stage, and the production dependency on the checks image. The reader executable moves into the host image. Retain it in checks where tests require it. |
| D1-9 | The named orphan candidates have different consumer states | low | none | `identity-db.example.yaml`, `prometheus-local.yaml`, the CDC files covered by D1-6, `evt-nats-check.yaml` (alignment R8), and stale executor references. Do not recreate `executor.yaml`. |
| D2-4 | Several command owners bypass bounded process supervision | medium | ctl/runner | Duplicate `output`, `run`, and command-timeout implementations. Scope is what survives Group 1, routed through `owned_command.rs` with a deadline. No second executor. B2-4 is merged here: signal listeners move from the library to the CLI binary. |
| D3-2 | Qualification does not identify all deployed behavior | medium | ctl/runner | The qualification identity split between `ImageSet` and `QualificationImages` only. The key stays release digest plus host image digest. |
| D3-4 | Desired deployment inputs still have several owners | medium | none | `EnvironmentFile` and the authored `host-group-receiving.yaml` and `host-group-wms.yaml` overlays. The JSON sidecars are deleted once, under D1-1. `environment.k` owns bindings and versions, not integration selections or floors. |

In passing (12 findings). Helper dedup, naming, count pins and doc links. Closed when the owning file is already open, never filed alone.

| Finding | Title | Severity | Image | Deletes |
| --- | --- | --- | --- | --- |
| B2-8 | Publication mixes release construction with local runtime adaptation | medium | ctl/runner | The unused database inputs from the local assembler and the duplicated local-versus-registry assembly path. The release-control crate is Group 4. |
| B2-9 | Provisioning APIs expose CLI output paths to internal callers | medium | ctl/runner | Temporary credential file round trips in environment ensure, internal `emit_*` fields and `-` sentinels. Return credential objects from the library. Files are a services/ctl concern. When the integration-schema work touches `provision_project_env`. |
| C1-5 | Run-plane repair is a partial second schema-upgrade engine | medium | host, ctl/runner | Structural SQL slicing, missing-column mutation, and separate table-bootstrap actions. Deletion only. Reconciliation keeps grants, roles and policies. |
| C2-4 | Completion markers replace current-state observation | medium | ctl/runner | Completion-marker gating around environment reconciliation and finished-stage gating around mutable upgrade phases. Markers stay for immutable artifacts and qualification results only. |
| C2-5 | Helm history equality bypasses recovery and drift repair | medium | ctl/runner | The historical-values-only early return and separate interpretations of successful history versus current deployment state. Resolve the pending Helm operation first. |
| C2-10 | Platform-schema ownership is scattered across lifecycle phases | low | ctl/runner | Platform-extension installation inside the application-package applier, moved into substrate setup. The duplicate migration calls vanish under C1-1. |
| D2-3 | Cloud failures leave published version tags without a complete platform set | medium | ctl/runner | Independent resolution of seven version tags as the platform publication boundary. Push by digest, tag the set after the last push, one test. |
| D1-2 | Cloud rendering ignores declared destination values | medium | ctl/runner | `deploy/gcp/otel-collector-values.yaml` as an independent authority and the test assertion that preserves `wamn-dev-otel`. The `letsencrypt.yaml` part is moot under alignment C1. |
| D1-7 | Operator and host releases select their dependency separately | medium | ctl/runner | The independent installer version constant, independent artifact selection, and the test’s separate expected-version literal. |
| D1-8 | Manual operator Jobs freeze an older runner | medium | none | Both operator YAML templates and the separate text-substitution renderer. The Job owner at `crates/control/lib/src/platform/install.rs:448` already renders with the stamped runner. Rename every surviving `mint-pat` reference to `issue-pat`. |
| D1-10 | Prototype deployment families survive through examples and test expectations | low | none | The unused Postgres, system-cluster, registry, credential-example, retention-example, and bootstrap families in the variant inventory, deleted in one commit. Remove associated test prerequisites after equivalent behavior is covered. |
| D3-7 | The installation lock cannot serialize different deployment machines | low | ctl/runner | The local file lock as the authority for cluster installation, replaced by a Kubernetes Lease held by the installer, and duplicated manual deployment writers in the runbook. No deployment queue, no promotion job. |

### Group 2: Rebuild only what changed

Target: cache identity comes from real inputs, so unchanged images are retagged by input hash (D2-2). The generator dependency is cut from the execution host, provisioning and schema control, so a generator change does not rebuild the host image (B1-1). The iteration-time bugs of the developer loop land here: watch misses application source, a candidate stops the active application, the generic command hard-codes Receiving, and checkout preparation builds unrelated applications (F2-1, F2-2, F2-3, F2-4, F2-7, F3-2). Stage timers go into the existing build and test runner, so the next real build reports cold, cached and commit-to-running durations (D2-8, G3-7). No separate measurement run.

Do (10 findings). The rows the review named, then every row that deletes a build, image, route, file or test target, or carries severity high.

| Finding | Title | Severity | Deletes |
| --- | --- | --- | --- |
| D2-2 | The older image pipeline builds artifacts that deployment does not select | high | `tools/journey-image-cache`, the upgrade image and guest stages, their leases, relabeling, and duplicate registry authentication. Listed in Group 1 for the deletion and in Group 2 for the cache identity. |
| B1-1 | Shared contracts pull the schema generator into serving code | high | The generator dependencies from provisioning, schema control, and execution host, after the consumed manifest and operation types move down to `catalog/model`. No new crate. Remove generator-specific error sources from provisioning errors. |
| D2-8 | The merge queue cannot establish release build cost or commit-to-running time | medium | Success-path evidence deletion and duplicate textual timing summaries. Keep successful logs and one build record with stage durations and artifact digests. No budgets yet. D3-3 closes into this row. |
| G3-7 | Tier durations lack a retained baseline | low | Successful-result deletion as the only retention policy. Replace scattered timing expectations with the selection file’s budgets. |
| F2-1 | Saved edits fail the freshness rule before the loop can rebuild | high | Remove the receipt freshness prerequisite from development source resolution and its call to `write_compiled_manifest`. |
| F2-2 | Watch mode ignores application Rust and web edits | high | Remove the separate, incomplete package input whitelist. |
| F2-3 | Candidate preparation stops the previous application too early | high | Remove unconditional shutdown on a changed package digest. |
| F2-4 | The generic developer command provisions a fixed Receiving environment | high | Remove Receiving constants and `JourneyScope::receiving()` from the product startup path. Remove the single-schema restriction. |
| F2-7 | Checkout preparation builds unrelated applications and misses runtime inputs | medium | Remove the all-application build loop, its temporary database lifecycle, and the combined build-and-`eval` entry point. |
| F3-2 | `wamn dev` must own its local services and first-run setup | high | Remove mandatory infrastructure flags, binary paths, handwritten local environment documents, and the separate standup step from the default author path. Remove committed HEAD as a local execution prerequisite. |

### Group 3: Tests and security

Target: one tier per target and one selection owner, mixed targets deleted (G3-1, G3-2, G3-6). Every ignore is fixed or deleted, and no ignore stays without a stated missing prerequisite (G2-8, G3-4). The security highs land here with no dependency on any crate move: E2-2 (an org control credential reaches its own org only), E1-2 (per-environment broker identity, grant answers local), E1-3, E1-4, A3-1, A3-2, A2-1, B2-6 and F3-5. Plaintext in-cluster transport (E1-5, E2-3, E2-4) is medium until a non-dev cluster exists.

Do (28 findings). The rows the review named, then every row that deletes a build, image, route, file or test target, or carries severity high.

| Finding | Title | Severity | Deletes |
| --- | --- | --- | --- |
| B2-2 | Grant policy spans provisioning, verification, and schema repair | high | The independently assembled expected grants and duplicate family dispatch in `provision_project_env/grants.rs`. Provisioning declares, control/lib compares, no copy. The three denial tests stay. |
| B1-2 | Six development back-links enlarge lower-layer test builds | medium | The six development dependencies, each fixed by replacing the import with a narrower one. Move a test only when no narrower import exists. |
| G3-1 | Default and ignored tests do not define useful execution tiers | high | The default-versus-ignored tier model and the `--cluster` alias for every ignored test. |
| G3-2 | Selection policy is scattered across the runner | high | The old JSON file and suite-specific shell branches. Retain existing build commands and fixture owners, but remove their duplicated selection conditions. |
| G3-6 | Tests need target moves and narrower deployment assertions | medium | Mixed render-and-cluster compilation targets, duplicate delivery setup, and repeated business assertions that prove no additional deployed boundary. Measurements.md section 7 reports 201 duplicated lines between Receiving and WMS tests, which supports inspecting this merge. |
| G2-8 | Empty prerequisite reasons conceal a built-service dependency | low | The empty prerequisite reasons and empty prerequisite calls where direct executable discovery supplies the required failure. |
| G3-4 | Ignore reasons mix prerequisites, disabled tests, and child helpers | medium | Empty reasons, prose-only prerequisite lists, permanent bead-only ignores, and the ignored SQLite helper test. |
| E2-2 | Org-scoped control credentials retain authority across organizations | high | Broad control table grants and caller-selected organization authority in privileged functions. |
| E1-3 | A delayed grant can restore revoked administration | high | Independent route-owned grant and revoke fan-out. |
| E1-4 | Inactive organization membership is not an admission boundary | high | Role-only and environment-membership-only admission predicates for user principals. |
| A3-1 | Operation binding turns infrastructure failures into permission denials | high | The unconditional permission-denied mapping and conversion through `error.to_string()`. |
| A3-2 | Authority cleanup can panic during unwinding | high | Parallel claim maps, repeated setters and removals, and lock-poison `expect` calls in cleanup. |
| A2-1 | Scalar spelling rules already diverge | high | Approximately 140–180 net lines, including the history date arithmetic and duplicate numeric parsing. |
| B2-6 | Release qualification embeds application test policy in the control library | medium | Application case lists (the closed Receiving/WMS match), Docker runner construction, and temporary test-result management from control/lib. The application declares its case set in its package beside its tests. |
| F3-5 | Release qualification rejects every new application identity | high | Remove the production `application_for` package allowlist and mandatory selection of Receiving/WMS test executables. Retain their business cases under their application owners. |
| E1-5 | Identity database traffic carries signing material without TLS | medium | Production `NoTls` connections in the identity service. |
| E2-3 | Postgres connections expose sensitive traffic without transport encryption | medium | Production `NoTls` connections, plaintext admission rules, and the CDC plaintext default. |
| E1-1 | Credential resource names collide across environments | high | The two independent single-hyphen naming formulas. |
| E2-1 | Valid environment names share credential Secrets and ServiceAccounts | high | The two independent single-hyphen name formats in `kubernetes.rs`. |
| E2-5 | Shared development infrastructure publishes administrator credentials | medium | `deploy/platform/postgres.yaml`, committed MinIO credential values, and shared root credential wiring in backup declarations. Re-scoped: the import paths are dropped by the integration-schema plan, so only the MinIO root and backup root identity remain here. |
| E2-7 | Standalone service images default to root execution | medium | Implicit root execution and unrestricted writable container filesystems. |
| E2-8 | Mutable base and infrastructure images weaken release provenance | medium | Tag-only shipping image references and duplicate image declarations. |
| E3-2 | The native lockfile contains reported security vulnerabilities | high | Vulnerable dependency resolutions and obsolete version assertions. |
| E3-3 | Package pulls and extraction have no resource budget | high | Unbounded blob buffers and archive `read_to_end` calls. |
| E3-4 | Aggregate guest memory remains observational | high | Serving defaults that select `GuestMemoryMode::Count`. |
| G2-1 | Quarantine removes history reconstruction and retention proofs | medium | The two quarantine attributes, obsolete fixture setup, and exact CLI prose assertions at `services/ctl/tests/prune_record_history_live.rs:621`. |
| G3-3 | The merge queue omits required live behavior | high | Manual live-stage instructions as the only execution path for required cases. Remove the rule that cluster coverage depends only on edits to cluster tests. |
| G3-5 | Generated output prevents isolated contract compilation | medium | Compile-time reads of generated application output from ordinary test targets. Remove application generation from documentation-only quality execution. |

In passing (16 findings). Helper dedup, naming, count pins and doc links. Closed when the owning file is already open, never filed alone.

| Finding | Title | Severity | Deletes |
| --- | --- | --- | --- |
| E1-6 | Cached PAT approval survives credential expiry | medium | Deadline-free `Token` approval entries. |
| E1-7 | Cache replication restarts the age of old authorization facts | medium | Receipt-time renewal of replicated authorization values. |
| E1-8 | File-backed signing keys remain trusted indefinitely | medium | The unconditional `true` freshness result. |
| E1-9 | CLI and HTTP routes enforce different grant rules | medium | Transport-specific semantic guards and direct bypass paths. |
| E1-10 | An unused API-key authority remains in the application schema | low | `API_KEY`, `platform.api_key`, its history scaffolding, and tests that only preserve the unused structure. |
| E2-6 | Command help can disclose database passwords | medium | Secret values from generated help and unnecessary password-bearing command arguments. |
| E3-1 | Registry descriptors can bypass byte admission | medium | The assumption that registry existence proves admission and the conversion of unchecked descriptors into admitted facts. Waits for alignment W1 to W3. Do not re-pin exact versions. |
| E3-5 | Execution deadlines do not contain compilation | medium | Full compilation solely for import discovery and unrestricted detached compilation work. |
| E3-7 | Engine construction mutates the environment after threads start | medium | Environment mutation from the shared engine constructor and its incomplete safety argument. |
| E3-8 | Dependency identity checks do not provide a refresh policy | medium | Unused dependency declarations and stale advisory results as release evidence. Retire `vendor/pg-walstream` when upstream supplies the required typed keepalive API. |
| G2-2 | Fixture grant inventories suppress the private-operation test | medium | The nineteen-entry shared-fixture snapshot, the absolute eighteen-grant assertion, and the `wamn-e1zd9` and `wamn-slf04` ignores. |
| G2-3 | Four-table pin disables the introspection refusal suite | medium | The four-table assertion and quarantine attribute. |
| G2-4 | Chart argument counts freeze incidental structure | medium | The five-argument assertion and both bead quarantine reasons after their causes are resolved. |
| G2-5 | PostgreSQL-only ignores exclude ordinary credential tests | medium | Both PostgreSQL-only ignore attributes. |
| G2-6 | Several count pins have no named contract | low | Redundant absolute inventory counts and `in_flight_bound_is_pinned_under_the_client_semaphore`. |
| G2-7 | SQL formatting receives the same freeze treatment as persisted identity | low | The complete DDL string snapshot. |

### Group 4: Code cleanup and consolidation

Target: dormant surfaces and unused dependencies first (A1-x, A1-6, A1-10), then the contract JSON (A2-3), then the crate map. Crate consolidation is last because it forces full rebuilds and guest repins and must not precede the build and deploy fixes. Helper and fixture dedup (G1-x, A2-x) lands only when the file is already being changed, never as its own issue. Authoring rows F1-x and F3-x stay here.

Do (33 findings). The rows the review named, then every row that deletes a build, image, route, file or test target, or carries severity high.

| Finding | Title | Severity | Deletes |
| --- | --- | --- | --- |
| A1-6 | Forty-eight native dependency declarations are unused | medium | 48 dependency lines, plus stale comments. Lockfile reductions require a new resolution and are not counted. |
| A1-10 | Nine guest dependency declarations are unused | low | Nine dependency lines, plus obsolete comments. |
| A2-3 | Host contracts repeat thousands of authored JSON lines | medium | Measured gross deletion of 71 files and 3,020 repeated lines. Estimated net saving: approximately 2,800 lines after shape assignments. |
| B1-6 | Guest interfaces depend on native implementation directories | medium | Guest paths into native runtime and router directories, replaced by one canonical WIT directory read by path. WIT package versions stay. Guest digests change once, repin once. |
| B3-2 | Forty-four packages can retain the enforced execution boundaries | high | Every package manifest with one consumer or no enforced boundary, its redundant dependency declarations, and obsolete compatibility re-exports. The map uses the README names. T01 is the `wamn-contract` extraction (from B3-1). T11 is deferred and T13 is rejected. Runtime merges wait for A5. |
| B3-3 | Migration must follow dependency order and distinguish artifact changes | high | Obsolete package-name assertions, retired dependency links, and duplicated runner selections. Do not retain empty compatibility packages. Step 5 waits for A5. One qualification cycle covers the guest move. |
| B3-4 | Guest data crates and single-consumer helpers can become modules | medium | Five application data manifests, the PostgreSQL SQLx library manifest, the label-template manifest, and the separate data-access manifest. Remove emitter references to the retired Rust crate names. |
| B3-5 | Three first-party workspaces are sufficient, but feature isolation must remain | medium | The Modbus standalone workspace declaration, both checked-in fixture manifests and lockfiles, and their standalone build contexts. Delete duplicated dependency and lint declarations where workspace inheritance replaces them. |
| B3-6 | Test package boundaries can collapse without exporting test authority | medium | Seven redundant manifests from the eight test and tool packages combined into T18. Delete two redundant support manifests. Remove facade imports between the merged owners. |
| F1-1 | Custom publication copies contracts and already contains drift | high | Inline custom input schemas, repeated operation declarations, and authored definition hashes across the six publication trees. Preserve wiring graphs and deliberate policy differences. |
| F1-3 | CRUD generation stops before component assembly | medium | The five statement include files total 178 lines. Include-only WIT worlds and generated-module bootstrap blocks also disappear from application source. Pure CRUD applications need no authored data or component crate. |
| F1-5 | Single-statement projections require handwritten implementations | medium | The fixture maker-list data and component files, plus equivalent passthrough portions of Receiving and WMS reads. Authored query SQL remains. |
| F1-7 | Application tests duplicate platform build and deployment machinery | medium | Duplicate process setup, build execution, artifact collection, and deployment helpers. Keep business histories, concurrency cases, and failure assertions. |
| F2-6 | Startup requires 22 input kinds that largely describe local machinery | medium | Remove routine local startup flags and the optional-input path that prints an invalid next command. Do not add infrastructure fields to `wamn.k`. |
| F3-1 | The starter must generate an executable application outside the platform checkout | high | Remove mandatory authored Cargo crates, WIT path lists, generated-code includes, and publication boilerplate from the default application shape. Replace repository-relative tool discovery for this path. |
| F3-3 | Generated descriptions need a default application renderer | high | Remove mandatory per-application web manifests and Vite files. Replace mechanical screen bindings with generated bindings. Delete the repository source alias once the shell package supplies the interface. |
| F3-4 | Platform tests and operational instructions remain application responsibilities | high | Remove duplicated application setup helpers and generic platform assertions after equivalent platform coverage exists. Delete copied operational procedures and replace issue-note instructions with links to canonical documentation. |
| F3-6 | Deployment needs one author command over the existing control owners | high | Remove author-maintained deployment scripts, manual digest transfers, and a separate web-upload step from the default path. Keep lower-level operator verbs for administration and recovery. |
| F3-7 | The ten-minute target lacks a matching acceptance test | medium | Replace the hello-app procedure with this executable quickstart. Remove author-maintained smoke-test setup for generated behavior. |
| H1-2 | Failed Git reads can produce an empty successful selection | high | The unchecked process substitution around change discovery. |
| H1-6 | Application lifecycle adapters duplicate resource management | medium | The three lifecycle scripts and duplicated image-loading loops. Reduce `delivery-owned` to retained repository launch commands. |
| H1-7 | Compact benchmark results lose their identity and failure detail | medium | The unused `metadata.json` handoff and independently maintained compact field selection. |
| H1-12 | Retired and unreferenced tools obscure the supported surface | low | All three scripts and tests whose sole subject is the removed command surface. |
| H2-1 | Plans compete with reference pages for current contracts | high | Repeated implemented contracts from active plans, including the SQL schema copies in `docs/plan/integration-schema.md:24`. |
| H2-2 | Closed work remains in the active plan collection | medium | Completed plan files and their active index entries after their unique contracts and standing rules reach current owners. |
| H2-5 | Local links contain five live missing paths and three missing anchors | medium | Obsolete source claims and links to retired deployment files. Do not recreate removed implementation files to satisfy documentation links. |
| H2-6 | Historical proof claims point to absent evidence | medium | Unsupported proof wording and dead artifact links when no retained source exists. Do not add placeholder evidence files. |
| H3-1 | The parent install omits wfe dependency repairs | high | No source deletion initially. Remove the separate, inconsistent installation assumptions. |
| H3-3 | Source aliases hide dependencies between workspace packages | medium | Cross-package source aliases, matching TypeScript path overrides, and Receiving’s direct import of the private wfe transformation script. |
| H3-4 | Shared Vite configuration owns too little | medium | Repeated Solid-version plugins, resolution blocks, test defaults, and worker filename callbacks. Replace manually restated Vite interface shapes with Vite’s own types. |
| H3-5 | The tooling package still owns application UI | medium | `web/shell/src/sync.tsx`, `web/shell/src/status.ts`, and their old test location after relocation. Remove Solid and sync runtime dependencies from the tooling package when no remaining code needs them. |
| H3-8 | Bundle parsing tests are copies with different parameters | low | The duplicated build, parse, diagnostic, and cleanup bodies. |
| H3-9 | Three exports and one root dependency are removable | low | Three unnecessary exports and one dependency declaration initially. Later, delete consumer chart-type dependencies and path mappings after their owner supplies the types. |

In passing (75 findings). Helper dedup, naming, count pins and doc links. Closed when the owning file is already open, never filed alone.

| Finding | Title | Severity | Deletes |
| --- | --- | --- | --- |
| A1-1 | The startup-burst module has no execution entry point | medium | Approximately 827 lines, including the module declaration. |
| A1-2 | The router wiring cache is unused | medium | Approximately 574 source and test lines, plus cache dependency comments and declarations. |
| A1-3 | Most operator-recovery helpers outlive their callers | low | Approximately 180 net lines from the 211-line module, after retaining the useful helper. |
| A1-4 | Pool observability exposes functions that nothing calls | low | Approximately 91 lines. |
| A1-5 | The engine carries an unused reverse context decoder | low | 49 lines. |
| A1-7 | Generator convenience methods have no callers | low | Approximately 43 lines, excluding surrounding blank lines. |
| A1-8 | Runtime convenience APIs have no callers | low | Approximately 39 lines. |
| A1-9 | The Kubernetes credential provider is an unimplemented placeholder | low | Approximately 24 lines, plus the re-export entry. |
| A1-11 | Generated templates emit support that particular operations never use | medium | Unused emitted helpers and constants. Generated outputs were unavailable, so no aggregate line count is claimed. |
| A1-12 | Portable test infrastructure remains inside production libraries | low | At least 1,331 lines leave production source directories through relocation: 538 invariant lines, 639 case/drift lines, and 154 database-fixture lines. Net repository deletion is approximately zero. |
| A1-13 | Internal implementation items are exposed as public APIs | low | Unused public re-export entries. Most implementations remain, so no substantial line saving is claimed. |
| A2-2 | Command runners bypass an existing lifecycle owner | high | Approximately 60–100 net lines from repeated execution and success handling. |
| A2-4 | PostgreSQL adapters repeat transaction and tracing machinery | medium | Approximately 150–220 net lines from the inspected production blocks. |
| A2-5 | Authority SQL remains embedded in four installation artifacts | medium | Approximately 120–150 net lines from the four embedded copies. |
| A2-6 | Database setup has three homes and repeated schema composition | medium | Move the three modules, totaling 154 lines, out of production libraries. Estimated net saving from shared setup and migrated callers: 60–120 lines. |
| A2-7 | Workflow tests duplicate component and credential fixtures | medium | Approximately 90–110 net lines from the inspected pair. |
| A2-8 | HTTP guests duplicate transport policy helpers | medium | Approximately 75–95 net lines from one copy of the helpers. |
| A2-9 | Web duplication is concentrated in build policy and bundle tests | medium | Approximately 55–75 net lines. Count zero savings from merging `browser.mjs` or the small `sw.ts` entries. |
| A2-10 | Four installers repeat the same installation state machine | medium | Approximately 60–90 net lines. |
| A2-11 | Identifier quoting has an existing owner that two callers bypass | low | Six implementation lines, or nine including the forwarding wrapper, before import changes. |
| A2-12 | Raw digest helpers repeat one wire spelling | low | Approximately 20–30 net lines across the five implementations. |
| A2-13 | Tests rediscover the repository root despite an existing helper | low | Approximately 120–150 net lines from test helpers. |
| A2-14 | Four shell tools repeat failure and usage handling | low | Approximately 20–35 net lines from the four tools. |
| A2-15 | Four empty attachment documents represent the same absence | low | Four files and four authored lines. Net line saving depends on replacing the inconsistent reader behavior. |
| A3-3 | Blobstore exposes backend prose and classifies timeouts by text | medium | Display-text timeout matching and backend `to_string()` calls in wire payloads. |
| A3-4 | Durable workflow decisions depend on hidden error-chain types | medium | `DeadlineAdjustments` as an error-context carrier and settlement downcasts. |
| A3-5 | Anyhow crosses public library boundaries across ten owners | medium | Unnecessary public helpers, public `anyhow` signatures, and caller-side error inspection. |
| A3-6 | SQLx maintains a second copy of the PostgreSQL wire taxonomy | medium | `WamnPgError` and its conversion match. |
| A3-7 | Diagnostic wrappers discard their original causes | medium | Cause-discarding conversions and the opaque activation wrapper. |
| A3-8 | Credential validators duplicate error vocabularies and parser scaffolding | medium | Repeated error enums, formatting implementations, and common parser branches. Re-scoped to the five database-credential validators. |
| A3-9 | Client error decoding invents missing contract data | medium | Defaults that fabricate error codes, operation names, and revisions. |
| A3-10 | Native service files combine several independent lifecycles | medium | The large root implementations and duplicated setup exposed during extraction. Moving code alone does not reduce total code. |
| A3-12 | Generator files need semantic splits, not blanket panic removal | medium | Generation-order JSON lookups, repeated codec fragments, and the large parent implementations. Keep justified formatting assertions. |
| A3-13 | Routing and serving manifests combine policy, representation, and mechanics | medium | Large root bodies and repeated projection or validation code exposed by the split. |
| A3-14 | The expect-over-allow policy does not hold across source and generated output | low | Unnecessary suppressions and blanket module exemptions. |
| B1-3 | The runtime hub makes artifact consumers depend on cloud execution | medium | Runtime-owned artifact transport implementations after consolidation, duplicate transport helpers, and control’s runtime dependency once its remaining imports move. After A5. |
| B1-5 | Catalog is a shared model hub with installation and product behavior | medium | The installation SQL constant and the built-in control-route construction, moved when catalog is next changed. |
| F1-2 | Custom handlers repeat manifest request, result, and error definitions | medium | Duplicate command/result structures, literal switches, and field-copy adapters. Keep parsed business state, database sequencing, and business refusals. |
| F1-4 | Acme reauthors the contract of its pinned base operation | medium | The copied contract portions, the handwritten base WIT import, `base_request`, and matching branches of `base_error`. |
| F1-6 | Applications maintain scalar and cursor machinery that a shared crate already owns | medium | Receiving’s cursor implementation after compatibility proof, plus duplicate generic parsing functions. Preserve business predicates and their tests. |
| F1-8 | Receiving manually reconnects generated CRUD descriptions to their behavior | medium | Repeated filter/sort lists, ordinary form submission adapters, generic detail loading, and equivalent reference-resolution glue. |
| F2-5 | Infrastructure ownership remains outside the developer command | medium | Remove separate service startup instructions and fixture-only orchestration after moving their shared behavior into the local stack owner. |
| F2-8 | Required host tools appear across several late failure points | medium | Remove duplicated prerequisite lists and personal cache configuration from the required checkout path. |
| F2-9 | The collector recipe assumes a cached image and a free fixed port | medium | Remove manual collector rendering and endpoint copying from the normal startup procedure. |
| F2-10 | Test execution needs a separate artifact and service recipe | medium | Remove repeated component path exports and manual preparation recipes from individual test instructions. |
| F2-11 | No measured checkout-to-app timeline exists in the shared evidence | low | Replace scattered startup recipes with the selected application path. Retain detailed provisioning instructions for external-service overrides. |
| G1-1 | Shared support needs one owner and a smaller dependency boundary | medium | Two support manifests, duplicate primitive implementations listed below, and cross-suite source imports such as `tests/integration/src/control_client_live.rs:28`. These imports already share source and are not additional helper definitions. |
| G1-2 | PostgreSQL connection setup repeats across fifty helpers | medium | Repeated `tokio_postgres::connect` and driver-spawn bodies. Retain the distinct session preparation. |
| G1-3 | PostgreSQL command runners already disagree on execution rules | medium | Fourteen local runners and their repeated pipe setup. Small success and refusal wrappers can disappear where direct assertions remain clear. |
| G1-4 | Database URL rewriting has several independent implementations | medium | Three `swap_db` bodies and the repeated parsing or string-splitting bodies in the credential helpers. |
| G1-5 | Cluster command helpers repeat cancellation and diagnostic policy | medium | Nine command-capture bodies, eight cluster command constructors, and three JSON capture wrappers. |
| G1-6 | Private file creation repeats nine times with different overwrite rules | medium | Nine local implementations and their repeated permission imports. |
| G1-7 | Three package manifest fixtures differ only in names | medium | Two complete manifest copies and repeated environment boilerplate. Preserve assertions for component loading, connection binding, route authentication, and overlay composition. |
| G1-8 | Temporary directories, executable files, and repository paths repeat | low | Local directory structs and their methods, executable writers, and repeated repository traversal. |
| G1-9 | An admitted-component fixture repeats thirty-two lines three times | medium | Two thirty-two-line helper copies and imports used only by them. |
| G1-10 | Router tests maintain three copies of graph construction | low | Duplicate constructors in route and terminal tests. Keep the distinct drivers. |
| G1-11 | Role observation and cleanup SQL repeat across test owners | low | Two state-query copies and two cleanup bodies. |
| G1-12 | Recording credential stores repeat as three local types | low | Two structs and their three-method trait implementations. |
| G1-13 | Offline plugin tests repeat the same PostgreSQL configuration | low | Repeated configuration literals and empty supporting-state construction. |
| G1-14 | Recursive package copying has three implementations | low | Two recursive implementations. |
| G1-15 | Small clock and ready-future helpers also repeat three times | low | Two polling bodies and two clock-conversion bodies. |
| H1-3 | The descriptor diagnostic can never pass | medium | The standalone 178-line diagnostic after its behavior moves to the host tests. |
| H1-4 | A missing argument can hang the Kubernetes runner | medium | The unchecked argument branches, followed by the shell parser when its owner moves. |
| H1-5 | Identity tests still own a separate shell orchestration system | medium | Both shell runners after their callers move, plus the test that extracts the identity cleanup function with `sed`. |
| H1-8 | The cloud version minimum runner leaves work active after failure | medium | The Job-only cleanup handler and unbounded discovery loop. |
| H1-9 | The merge queue runner omits its documented submodule check | medium | No production functionality. Remove any duplicate external invocation once the merge queue owns it. |
| H1-10 | Four installers repeat one installation algorithm | low | Repeated installer bodies and duplicate pin declarations. |
| H1-11 | Repository launchers repeat shell plumbing | low | Repeated plumbing in the retained launchers. |
| H2-3 | The previous audit gives an unsafe blanket archive list | medium | `docs/plan/docs-audit.md` from the active plan collection, its obsolete status inventory, and completed sections of mixed plans. Preserve the dated audit through Git or the history index. |
| H2-4 | Standing repository rules live inside a completed deployment plan | medium | The standing-rule block from the deployment plan and repeated “How this plan is worked” imports from other plans. |
| H2-7 | Change histories occupy pages that readers use as references | low | Redundant historical snapshots after preserving their source revisions. The three snapshot pages `gcp-install-history.md`, `platform-ui-history.md`, and `deployment-pre-apply.md` contain 3,891 lines. |
| H2-8 | Navigation omits active alignment work and lacks a history map | low | Completed plan entries, copied status lists, and snapshot pages covered by the preceding findings. Add only the compact history index. |
| H3-6 | ESLint applies different rules to equivalent application code | medium | The separate platform-only rule block and stale `web/demo` exclusion. |
| H3-7 | Browser drivers duplicate fixture and transport support | medium | Repeated fixture parsing, sign-in functions, receipt actions, and HTTP forwarding implementations. Retain every scenario entry point. |
| H3-10 | Obsolete Beads files remain tracked beside the canonical export | low | `.beads/beads.jsonl` and `.beads/.~issues.jsonl.3646117766`, about 7.6 MB combined. |

## Open questions for the owner

1. Can record-history take a shared scalar dependency? This unblocks Group 4 (A2-1). The shared contract layer itself is answered: `catalog/model`.
2. Which external consumers require current Rust APIs of the test support and fixture packages? This unblocks Groups 1 and 4 (G1-1, A1-7 through A1-9). Cargo package names and the `wamn-control` API are answered: none.
3. Can root supply retained feature results and generated packages before dependency removal? This unblocks Group 4 (A1-6, A1-10).
4. Must wiring caching remain a supported requirement despite its absent production caller? This unblocks Group 4 (A1-2).
5. Which machine and cache states define timing comparisons for retained results? This unblocks timing acceptance for Group 2 (G3-7).
6. Must authored SQL remain directly executable, or can composed installation recipes produce standalone SQL as build output? This unblocks Group 4 (A2-5, C3-2).
7. Can `environment.k` own local and deployed coordinates, with the default developer command generating its private local document? This unblocks Groups 1 and 2 (D3-4, F2-4, F3-2).
8. Which local/deployed host behaviors must match, and must WMS retain its distinct concurrency limit? This unblocks Groups 1 and 3 (D1-5, G2-4).
9. Can the identity journey replace its repository `evidence/` requirement with caller-owned temporary result storage? This unblocks Group 4 (H1-5).
10. Does any external workflow still require `kind-gate-build` or `kind-gate-image-remove`? This unblocks Group 4 (H1-12).
11. Which deployed coordinates currently produce colliding credential names? This unblocks safe replacement in Group 3 (E1-1, E2-1).
12. Must an organization control credential remain trusted with authority across every organization? This decides the database boundary in Group 3 (E2-2).
13. Is every operator certificate intentionally platform-wide, and how must one certificate be revoked without replacing its CA? This informs privileged credential boundaries in Group 3 (E2-2, E2-3).
14. Are cache delays permitted only after lost invalidation, or also after natural credential expiry? This unblocks Group 3 (E1-6, E1-7).
15. What maximum disconnected period permits continued trust in an local signing key set? This unblocks Group 3 (E1-8).
16. Which independently enforced network controls protect deployed identity databases, plaintext services, and standing development fixtures? This establishes the live boundary for Group 3 (E1-5, E2-3, E2-5).
17. Who owns refreshes for WASI-Virt, its adapter, and the pg-walstream patch, and what refresh interval applies? This unblocks Group 3 (E3-8).
18. Does another check scan the web dependency graph, including the pinned wfe dependency resolution? This determines the remaining scan scope in Group 3 (E3-8).
19. Must WASI-Virt provenance report a measured adapter digest instead of its current constant? This unblocks provenance acceptance in Group 3 (E3-8).
20. Is registry write access intentionally sufficient to bypass component admission? This decides the trusted publication boundary in Group 3 (E3-1).
21. What measured condition permits serving hosts to leave aggregate memory count mode? This unblocks Group 3 (E3-4).
22. Must poisoned authority state terminate the process, or must the host isolate the affected owner and become unready? This unblocks Group 3 (A3-2).
23. Which blobstore `Other` messages are frozen consumer contracts? This unblocks replacement of backend prose in Group 4 (A3-3).
24. Can host operations explicitly share authored shapes while resolved contract bytes and generated clients remain unchanged? This unblocks Group 4 (A2-3).
25. Must SQL literal quoting support nondefault PostgreSQL string parsing, or can callers require the default server behavior? This defines quoting acceptance in Group 4 (A2-11).
26. Must HTTP exposure remain explicit for every custom public operation? This unblocks generated publication policy in Group 4 (F1-1).
27. Can Acme consume its pinned base operation’s wire shape while retaining separate identity and inspection behavior? This unblocks Group 4 (F1-4).
28. Can pure CRUD applications use generated component crates without authored Rust files? This unblocks Group 4 (F1-3, F3-1).
29. Can WAMN consume only public built wfe exports while source qualification remains inside the submodule? This unblocks Group 4 (H3-3).
30. Which retained saved-edit acceptance run passed against the current freshness, watcher, and activation rules? This supplies the comparison result for Group 2 (F2-1 through F2-3).
31. Can the normal developer command own the complete private stack, with external services available through explicit overrides? This unblocks Groups 2 and 4 (F2-5, F2-6, F3-2).
32. Can the default author experience require Docker on Linux, or must the first release support another host platform? This unblocks Groups 2 and 4 (F2-8, F3-2).
33. Which cold-start, warm-start, and saved-edit limits must the retained results enforce? This unblocks acceptance for Groups 2 and 4 (F2-11, G3-7).
34. Does the ten-minute clock start with the installed SDK, Docker, and provisioned account, or include their installation and provisioning? This unblocks Group 4 (F3-7).
35. Can root supply live statuses for quarantined tests and archive candidates, including which ignored cases represent defects or retired contracts? This unblocks Groups 3 and 4 (G2-1 through G2-4, G3-4, H2-2, H2-3).
36. Which dashboard or collector owns the frozen PostgreSQL telemetry identifiers whose guard cites deleted documentation? This unblocks retained-contract decisions in Groups 3 and 4 (G2-6, H2-5).
37. Must startup-burst measurement remain supported despite its absent repository execution path? This unblocks Group 4 (A1-1).
38. Must benchmark summaries remain readable without their raw result files? This defines the retained result contract in Group 4 (H1-7).
39. Does an immutable Git source plus a compact history entry satisfy the rule that accepted plans remain untrimmed? This unblocks Group 4 (H2-2, H2-7).
40. Where are the twelve historical performance artifacts retained, if anywhere? This unblocks evidence disposition in Group 4 (H2-6).
41. Does any external tool still read `.beads/beads.jsonl`? This unblocks obsolete-export removal in Group 4 (H3-10).

Questions 8 and 10 of the first draft are answered in Rejected findings and proposals. Nothing from this list is filed until the owner answers.
