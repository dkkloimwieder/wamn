# WAMN codebase audit, 2026-10-31

This audit covers WAMN applications, runtime services, build and deployment tools, tests, security boundaries, and documentation at commit `8c831349d`. The lane reports are recorded at `45a5ecd31`. Twenty-four read-only Codex gpt-6.1-sol runs used high reasoning effort, with one shared measurement pass at `788cdadb4`.

Inputs: [measurements](measurements.md), [code quality](code-quality.md), [coupling](coupling.md), [upgrade](upgrade.md), [deployment](deployment.md), [security](security.md), [developer experience](developer-experience.md), [tests](tests.md), and [hygiene](hygiene.md). The synthesis also reads [the architecture overview](../architecture/overview.md) and [CLAUDE.md](../../CLAUDE.md). No build, test, or deployment ran during this synthesis.

## Findings ranked across lanes

The table covers all 222 numbered findings in 184 merged rows. Severity follows the reports. Merged rows use the highest severity and largest effort estimate. S, M, and L mean small, medium, and large effort.

Within each severity, risk reduction takes priority, followed by estimated deletion. Relocation does not count as deletion. Savings overlap and must not be added together. Each linked lane file contains the source locations for its finding IDs.

| id | lane file | severity | effort | area | one-line summary |
| --- | --- | --- | --- | --- | --- |
| E1-1, E2-1 | [E](security.md) | high | M | Tenant credentials | Valid coordinates collide in Secret, ServiceAccount, Role, and RoleBinding names. |
| E2-2 | [E](security.md) | high | L | Organization authority | An org control credential can change authority and connection records across organizations. |
| E1-2 | [E](security.md) | high | L | Authorization cache | Shared NATS credentials permit forged grant answers for another environment. |
| E3-1 | [E](security.md) | high | M | Component admission | Publication trusts registry descriptors without deriving admission facts from the component bytes. |
| E1-3 | [E](security.md) | high | L | Grant revocation | A delayed grant projection can restore application administration after revocation. |
| E1-4 | [E](security.md) | high | M | Membership | Concurrent grants can outlive deactivation, and session admission omits active org membership. |
| E1-5, E2-3 | [E](security.md) | high | L | Database transport | Native database and replication connections permit plaintext traffic, including signing material. |
| E2-4 | [E](security.md) | high | M | Event transport | Event NATS exposes credentials and event contents through plaintext connections. |
| E3-2 | [E](security.md) | high | M | Dependencies | The recorded native dependency scan reports 17 vulnerabilities and 12 warnings. |
| E3-3 | [E](security.md) | high | M | Artifact limits | Package download and extraction lack aggregate byte, layer, and file budgets. |
| E3-4 | [E](security.md) | high | S | Guest memory | Serving defaults observe aggregate guest allocations instead of refusing excess allocation. |
| A3-2 | [A](code-quality.md) | high | L | Authority lifetime | Claim cleanup can panic during unwinding before it finishes revoking authority. |
| B2-1, C2-2, C3-3 | [B](coupling.md), [C](upgrade.md) | high | L | Lifecycle ownership | Parallel upgrade drivers mutate production packages outside one encompassing environment lock. |
| C1-2, C2-1 | [C](upgrade.md) | high | M | Installed environments | Installer completion rules can skip schema preparation or replay an application with `release = none`. |
| C2-3 | [C](upgrade.md) | high | L | Package upgrades | Environment apply requires successor facts that its own upgrade phase cannot establish. |
| C1-4 | [C](upgrade.md) | high | M | Bootstrap recovery | Partial control bootstrap can leave a database that refuses every retry. |
| C1-1, C1-5, C3-2 | [C](upgrade.md) | high | L | Schema ownership | Full-schema installation, empty migration streams, and partial repair provide different schema paths. |
| C1-3 | [C](upgrade.md) | high | M | Upgrade ordering | Environment analysis reads schema-dependent facts before project migrations run. |
| C3-4, D3-2 | [C](upgrade.md), [D](deployment.md) | high | L | Qualification identity | Qualification omits parts of the deployed platform and schema transition. |
| A2-2, B2-4, D2-4 | [A](code-quality.md), [B](coupling.md), [D](deployment.md) | high | M | Process lifetime | Command adapters bypass bounded execution and can retain lifecycle locks indefinitely. |
| D2-3 | [D](deployment.md) | high | M | Platform publication | Failed publication can leave version tags for an incomplete platform artifact set. |
| C3-1, D1-1, D3-4, H3-2 | [C](upgrade.md), [D](deployment.md), [H](hygiene.md) | high | L | Desired state | KCL, JSON sidecars, host overlays, and local charts select competing deployment inputs. |
| D2-2, H1-1 | [D](deployment.md), [H](hygiene.md) | high | M | Image production | The older image pipeline builds unselected artifacts and omits real inputs from its cache identity. |
| B2-7, D2-6, D3-1 | [B](coupling.md), [D](deployment.md) | high | L | Installation | Installation builds and publishes candidates, while a platform stamp can skip application freshness. |
| D2-1 | [D](deployment.md) | high | M | Clean upgrades | A fresh upgrade worktree lacks application builds that later stages require. |
| B2-3 | [B](coupling.md) | high | L | Package execution | Package application calls back into the qualification runner that also calls package application. |
| B2-2 | [B](coupling.md) | high | L | Grant declarations | Provisioning, comparison, and schema repair independently assemble parts of grant policy. |
| B1-1, B3-1 | [B](coupling.md) | high | L | Serving dependencies | Shared package types and release-profile logic pull the compiler into deployed services. |
| B3-2 | [B](coupling.md) | high | L | Crate map | The proposed consolidation removes manifests while preserving engine, edge, and workflow restrictions. |
| B3-3 | [B](coupling.md) | high | L | Crate migration | Consolidation needs dependency ordering, live graph enforcement, and explicit guest repinning. |
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
| B2-6, F3-5 | [B](coupling.md), [F](developer-experience.md) | high | L | Qualification policy | Production qualification hard-codes Receiving and WMS package sets and rejects unfamiliar applications. |
| F3-6 | [F](developer-experience.md) | high | M | Author deployment | Authors coordinate bootstrap, publication, qualification, digest transfer, and browser upload manually. |
| D3-3, G3-3 | [D](deployment.md), [G](tests.md) | high | L | Merge evidence | A passing train does not establish all affected live behavior or deployment qualification. |
| G3-1 | [G](tests.md) | high | L | Test tiers | Default and ignored tests do not identify their infrastructure or execution boundary. |
| G3-2 | [G](tests.md) | high | M | Test selection | Selection policy spans JSON, shell branches, preparation rules, and exceptional path mappings. |
| H1-2 | [H](hygiene.md) | high | S | Selection failures | Failed Git reads can become an empty successful test selection. |
| H3-1 | [H](hygiene.md) | high | S | Web dependencies | The parent pnpm installation omits dependency patches declared by the pinned wfe submodule. |
| D1-2 | [D](deployment.md) | high | M | Cloud destinations | Telemetry and certificate rendering retain installation-specific values instead of declared destinations. |
| D1-3 | [D](deployment.md) | high | S | RC telemetry | The RC installer consumes an incomplete collector template instead of its complete renderer. |
| H2-1 | [H](hygiene.md) | high | M | Contract documentation | Active plans contradict current integration and write-log contracts. |
| D2-7, D3-6, F1-7, F3-4, H1-6 | [D](deployment.md), [F](developer-experience.md), [H](hygiene.md) | high | L | Deployment fixtures | Application and gate routes repeat builds, installation, credentials, collection, and cleanup. |
| E3-5 | [E](security.md) | medium | L | Compiler containment | Import discovery compiles components, and cancelled callers cannot terminate detached compilation. |
| C2-8 | [C](upgrade.md) | medium | L | Contract floors | The declared contract phase has no admitted executor and separates contraction from floor recording. |
| C1-6 | [C](upgrade.md) | medium | M | Migration history | Retained retired records conflict with the promised restart at migration `0001`. |
| C2-4 | [C](upgrade.md) | medium | M | Recovery | Completion markers skip mutable work without observing its current postconditions. |
| C2-5 | [C](upgrade.md) | medium | M | Helm recovery | Historical values equality bypasses pending-operation recovery and repair of live deployment drift. |
| C2-6 | [C](upgrade.md) | medium | M | Executor identity | Commit selection builds a target binary but still applies schema through the launching binary. |
| C2-7 | [C](upgrade.md) | medium | M | Credential recovery | Upgrade cleanup deletes private outputs before durable stage completion, breaking interruption recovery. |
| C2-9 | [C](upgrade.md) | medium | M | Package reconciliation | Current package versions skip repair of derived history functions and triggers. |
| D3-7 | [D](deployment.md) | medium | M | Cluster locking | A local installation lock cannot serialize different deployment machines. |
| E1-6 | [E](security.md) | medium | M | Credential expiry | Cached PAT approval can remain valid after the credential expires. |
| E1-7 | [E](security.md) | medium | M | Cache freshness | Replication gives old authorization facts a new cache lifetime. |
| E1-8 | [E](security.md) | medium | M | Offline trust | File-backed signing keys remain trusted indefinitely. |
| E1-9 | [E](security.md) | medium | M | Grant admission | CLI and HTTP mutations enforce different covering-grant and grantability rules. |
| E2-5 | [E](security.md) | medium | M | Development credentials | Standing fixtures and backup declarations share known administrator credentials. |
| E2-6 | [E](security.md) | medium | S | Secret diagnostics | Command help can print password-bearing environment values. |
| E2-7 | [E](security.md) | medium | M | Container identity | Standalone service images and deployments default to root execution. |
| E2-8 | [E](security.md) | medium | M | Image provenance | Mutable build and infrastructure tags permit different executable inputs for the same source. |
| E3-6 | [E](security.md) | medium | M | Connector retention | Retired bindings leave compiled connector templates and component bytes retained without bounds. |
| E3-7 | [E](security.md) | medium | S | Process initialization | Engine construction mutates the process environment after threads and connection work start. |
| E3-8 | [E](security.md) | medium | M | Dependency refresh | Identity gates provide no fresh advisory requirement across maintained dependency domains. |
| A3-3 | [A](code-quality.md) | medium | M | Blobstore errors | Wire errors expose backend prose and classify timeouts through text matching. |
| A3-4 | [A](code-quality.md) | medium | M | Workflow settlement | Durable retry and terminal decisions depend on hidden types in error chains. |
| A3-5 | [A](code-quality.md) | medium | L | Library errors | Public APIs expose `anyhow`, forcing callers to inspect chains or display text. |
| A3-6 | [A](code-quality.md) | medium | S | PostgreSQL contract | SQLx duplicates the generated PostgreSQL wire error taxonomy. |
| A3-7 | [A](code-quality.md) | medium | M | Diagnostics | Internal wrappers discard causes before the public redaction boundary. |
| A3-8 | [A](code-quality.md) | medium | M | Credential parsing | Five provisioning validators repeat parser scaffolding and error vocabularies. |
| A3-9 | [A](code-quality.md) | medium | S | Client decoding | Malformed error envelopes acquire fabricated codes, operation names, and revisions. |
| H1-4 | [H](hygiene.md) | medium | S | Runner arguments | A missing value can leave the Kubernetes shell runner in an endless parser loop. |
| H1-8 | [H](hygiene.md) | medium | M | Benchmark cleanup | Failed cloud floor runs can leave sampling active or triggers altered. |
| H1-9 | [H](hygiene.md) | medium | S | Submodule governance | The train omits its documented submodule guard. |
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
| B1-4, B2-5 | [B](coupling.md) | medium | L | Control coupling | The control compilation unit includes the developer application and unrelated tool orchestration. |
| B1-2 | [B](coupling.md) | medium | M | Test dependencies | Six development edges pull higher-level owners into lower-level test builds. |
| B1-3 | [B](coupling.md) | medium | L | Artifact ownership | Narrow artifact consumers depend on the runtime hub and its cloud execution dependencies. |
| B1-5 | [B](coupling.md) | medium | M | Catalog ownership | Catalog combines canonical records with installation SQL and built-in application construction. |
| B1-6 | [B](coupling.md) | medium | M | WIT ownership | Guest builds read canonical interfaces from native implementation directories. |
| B2-8 | [B](coupling.md) | medium | L | Release construction | Publication mixes immutable release assembly with local runtime representation and unused database inputs. |
| B2-9 | [B](coupling.md) | medium | M | Provisioning results | Internal callers exchange credentials through CLI output paths, temporary files, and sentinels. |
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
| C3-5 | [C](upgrade.md) | medium | L | Upgrade model | Maintenance upgrades can remove online stages, floors, progress storage, and inferred rollback. |
| D1-4 | [D](deployment.md) | medium | M | Workload variants | Historical HTTP and materializer workloads preserve a second deployment model. |
| D1-5 | [D](deployment.md) | medium | M | Host policy | Local development embeds Receiving cloud limits for every application. |
| D1-6 | [D](deployment.md) | medium | M | CDC artifact | Production CDC uses the gates image while its dedicated image stage remains unpublished. |
| D1-7 | [D](deployment.md) | medium | S | Operator pin | Host and operator installation select the upstream chart independently. |
| D1-8 | [D](deployment.md) | medium | M | Administrative Jobs | Manual invitation and PAT Jobs freeze an older runner. |
| D2-5, D3-5 | [D](deployment.md) | medium | L | Resource rendering | Helm, edited YAML, and Rust object builders compete to define deployed resources. |
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
| H1-12 | [H](hygiene.md) | low | S | Retired tools | An applied rename tool and unused gate build/removal pair retain 653 lines. |
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
| B1-7 | [B](coupling.md) | low | M | Architecture gate | Dependency enforcement omits workspaces, development edges, and relevant feature distinctions. |
| C2-10 | [C](upgrade.md) | low | M | Schema prerequisites | Several lifecycle phases repeat project migration and platform-extension responsibilities. |
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

### Rejected findings

| id | reason |
| --- | --- |
| None | Every numbered finding supplies file:line evidence. Source review supports the shared defects. Conflicting remedies remain design alternatives, as resolved below. |

The evidence does not establish exploitation of every dependency advisory, live network exposure, faster builds after consolidation, or a ten-minute authoring path. The reports explicitly limit these claims in E2-3, E3-2, B3-2, F2-11, and F3-7.

## The six largest areas for improvement

### 1. Authority, credential isolation, and resource limits

Supporting findings: E1-1 through E1-9, E2-1 through E2-8, E3-1 through E3-8, A3-1, and A3-2.

The refreshed dependency measurement records 17 vulnerabilities and 12 warnings (`docs/audit/measurements.md:391`). Secret names concatenate coordinate parts ambiguously (`crates/identity/credential-store/src/kubernetes.rs:21`). Identifier rules already reserve double hyphens as separators (`crates/control/registry/src/identifiers.rs:64`).

Grant fan-out uses process-local coordination (`crates/execution/host/src/control_route.rs:323`, `:685`). Shared cache entries reach authorization through `crates/platform/runtime/src/auth_cache.rs:292`. Production database clients use plaintext transport (`services/identity/src/lib.rs:165`, `services/host/src/host.rs:782`). Publication converts carried descriptors into admitted facts (`crates/control/lib/src/publish_release.rs:1621`).

Delete ambiguous naming formulas, shared grant-value replication, broad cross-org writes, plaintext defaults, unchecked admission assumptions, and unbounded artifact buffers. The expected effect is narrower tenant authority and bounded failure, with explicit tests for cross-tenant access, revocation, transport refusal, and resource exhaustion.

### 2. One lifecycle and one schema path

Supporting findings: B2-1 through B2-4, C1-1 through C1-6, C2-1 through C2-10, and C3-2 through C3-5.

The control crate contains 80,688 source lines, 32,437 test lines, and 71 `Command::new` expressions (`docs/audit/measurements.md:57`). The upgrade driver alone spans 2,879 lines under A3-11. It applies packages at `crates/control/lib/src/upgrade_environment.rs:784`, then enters locked environment application at `:984`.

Platform installation forces `release = none` during its initial application (`crates/control/lib/src/platform/environments.rs:46`). Environment analysis precedes schema preparation (`crates/control/lib/src/environment/mod.rs:276`). Both platform migration streams are empty (`crates/control/provision/src/schema_migrations.rs:93`, `:96`). This last behavior is deliberate under `docs/architecture/overview.md:67`, rather than an accidental missing implementation.

Delete the independent upgrade driver, mutable completion shortcuts, synthetic uninstall pass, and competing structural repair path. Under the proposed maintenance model, also delete online stages and floors, so one locked transition owns schema changes, deployment, and recovery.

The migration proposal changes the current greenfield rule. C1 and C3 support a replacement lifecycle, but the source supports full-schema installation as current behavior. No migration policy change or database reset occurs through this audit.

### 3. One artifact build and deployment path

Supporting findings: C3-1, C3-4, D1-1 through D1-8, D2-1 through D2-8, D3-1 through D3-7, H1-1, and H3-2.

The baseline contains 83 YAML files and 46,235 YAML lines (`docs/audit/measurements.md:13`). Those totals include pinned upstream bundles and are not a deletion budget. The release chart changed in 111 commits, while its Rust owner changed in 110 (`docs/audit/measurements.md:821`, `:822`). Receiving and WMS lifecycle scripts share 78 detected lines (`:711`).

WMS KCL declares four application packages, while its JSON sidecar supplies only WMS (`deploy/gcp/environments/dkk--wms--dev.k:16`, `deploy/gcp/environments/dkk--wms--dev.json:11`). Production CDC selects the gates image (`crates/control/lib/src/platform/cdc.rs:21`). Qualification excludes runner, gates, and identity images from its key (`crates/control/lib/src/release_chart.rs:54`).

Delete environment JSON sidecars, the older image builder, deployment-time compilation, historical workload snapshots, and independent fixture installers. The expected effect is one qualified artifact selection that both kind and GKE consume.

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

The train ends at source-test execution and deletes successful logs (`tools/train-run:60`, `:80`). A standalone descriptor diagnostic creates two cases but requires four passes (`tools/native-descriptor-limit-check:129`, `:158`, `:167`). H2-7 identifies 3,891 lines in three overlapping historical snapshots. H3-10 identifies about 7.6 MB in obsolete tracker files.

Delete repeated fixture mechanics, incidental snapshots, dead command surfaces, obsolete exports, and completed plan copies after preserving their unique requirements. The expected effect is explicit test selection, independently compilable cheap targets, retained execution results, and one current documentation owner for each contract.

## Proposed target architecture

### Crate map

This map combines B3’s complete inventory with B1-3’s artifact boundary and B2’s package, release, environment, developer, and delivery ownership. It does not adopt B3’s exact 44-package total. These are proposed ownership changes, not claims that the current arrangement violates its documented design.

| Target crates | Source consolidation |
| --- | --- |
| `wamn-contracts` | Pure catalog, authoring, registry, component policy, numbering, project identity, SQL vocabulary, execution, event, history, manifest, and schema contracts. Canonical WIT lives with these contracts. |
| `wamn-guest-sdk` | Shared data-access helpers and typed PostgreSQL statements. |
| `wamn-session`, `wamn-run-state`, `wamn-engine` | Retain separate verification, storage-decision, and execution boundaries. |
| `wamn-provision` | Combine control provisioning and schema-control declarations and plans. |
| `wamn-identity` | Combine identity persistence, credential storage, identity client, and identity service targets. |
| `wamn-package-artifact` | Retain verified artifact transport, registry credentials, and trust roots. Move duplicate runtime transfer code here. |
| `wamn-runtime`, `wamn-workflow` | Merge execution-host into runtime. Merge router algorithms into workflow as private pure modules. |
| `wamn-build` | Combine generation, introspection, composition, and virtualization tools. |
| `wamn-package-control`, `wamn-release-control`, `wamn-environment-control` | Replace the control umbrella with the sole package executor, release construction, and locked lifecycle owner. |
| `wamn-ctl`, `wamn-dev` | Keep administrative and delivery commands in the CLI application. Move the developer engine and `wamn` binary to `services/dev`. |
| `wamn-edge`, `wamn-host-services` | Fold SQLite storage into edge. Group host and scenario-worker packages while retaining separate binaries and credentials. |
| `wamn-cdc-reader`, `wamn-client`, `wamn-test-postgres` | Retain separate replication, native client, and private PostgreSQL lifetime owners. |
| `wamn-test-support`, `wamn-tests` | Combine harness, infrastructure, and fixture-package. Combine test/tool packages into explicit targets, with application sources kept beside their apps. |
| `receiving`, `wms`, `client-acme-receiving`, `device`, `samples`, `modbus-reader`, `netsuite`, `rest-connector`, `rest-consumer-fixture`, `wamn-integration`, `fixture`, `fixture-overlay` | Retain application component identities. Fold the five application data crates into their owning components. |
| `http-route`, `materializer`, `blob-put`, `jsonata-expression`, `wamn-command-fixtures`, `connection-http-standard`, `sqlx-command`, `std-virtualization-probe` | Retain capability-specific artifacts. Combine busyloop and sockprobe package ownership. Fold the SQLx driver into its fixture. |
| `http-page`, `http-request`, `label-render`, `transform`, `wamn-modbus` | Retain the isolated `no_std` workspace. Fold label-template into label-render and move Modbus into this workspace. |
| `pg_walstream` | Retain the vendor dependency until its required upstream API exists. |

Migration order is contracts, serving consumers, provisioning, build tools, identity, artifact/runtime/workflow owners, control applications, guests, then test packages. Update graph gates with each move. Rebuild and repin changed guests. Retain the three first-party workspaces and remove empty compatibility packages. Sources: B1-1 through B1-7, B2-3 through B2-9, B3-1 through B3-6, A1-2, and A1-12.

### Lifecycle model

Use `apply`, `apply --dry-run`, `show`, `status`, and `delete` as the common lifecycle surface. Platform and environment commands select scope. Package and schema changes run as internal phases of that reconciler, with no independent production mutation route.

`cluster.k` selects a platform bundle by digest. `environment.k` selects the application release. The release fixes package and overlay artifacts. Artifact migration streams define schema targets. Database ledgers record applied paths and hashes. Helm and Kubernetes provide observations, not independent desired versions.

One cluster-visible lock coordinates installation. Environment locks cover every target mutation. Resolve qualification, pause and drain affected writers, advance prerequisite schemas, apply package transitions, deploy, wait for readiness, then reopen admission. Retries observe current state.

This target adopts C3’s proposed maintenance assumption. It removes online stages, floors, and history-selected rollback. Earlier code requires an explicit selection qualified against the installed schema. Sources: B2-1 through B2-4, C1-1 through C1-6, C2-1 through C2-10, C3-1 through C3-5, and D3-7.

### Deployment path

The path is commit and submodule pins, selected tests, one artifact build, complete bundle publication, disposable qualification, protected selection, then `platform apply`. Application authors reach the same lifecycle through `wamn deploy`.

Keep the root Dockerfile, Helm, KCL, pinned upstream operators, and separate platform/environment Helm instances. Use one WAMN chart package with kind and GKE values. Publish the dedicated CDC image. Qualification identifies deployed artifacts, migrations, and test definitions.

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
| wamn-qpgq | reconcile-run-plane makes schema changes in code; they move under upgrade-schema (docs/plan/schema-upgrade.md) | C1-5, C3-2 | Migration-only structural changes replace the repair planner and historical cutovers. The schema-ownership requirement survives. Export: `.beads/issues.jsonl:3469`. |
| wamn-eycl | Package upgrade Epic 4: Package upgrade in the platform UI | C3-3, C3-5 | A separate `environment.upgrade` saga disappears. The UI submits desired state through the shared reconciler. UI support remains required. Export: `.beads/issues.jsonl:3458`. |
| wamn-ptxoz | gcp.md §4.19.1: the platform UI version in values-edge.yaml and url-map.yaml is a hand-edited literal that drifts with every platform bump | D3-1, D3-4, D3-6 | Artifact-selected deployment removes the manually edited version carriers and alternative installation instructions. Export: `.beads/issues.jsonl:1954`. |
| wamn-litd5 | tests/system/tests/deployment_declarations.rs:1059 pins platform-ui 0.1.31, but deploy/gcp/values-edge.yaml, url-map.yaml and the release chart are 0.1.36 | D1-10, D3-4 | Tests move from historical deployment literals to current rendered resources and selected artifacts. Export: `.beads/issues.jsonl:1938`. |
| wamn-u1qym | gcp.md §4.13: names packages wamn_receiving-2.1.0 and wamn_wms-2.1.0, not the current package versions | D3-1, D3-4, D3-6 | Deployment consumes the release’s package closure, so manual package-version instructions disappear. Export: `.beads/issues.jsonl:1958`. |
| wamn-klisb | gcp.md §4.8/§4.17: the scheduler NATS port-forward ended during the install and env apply failed with connection refused on 127.0.0.1:14223 | D3-6 | The common in-cluster installation path replaces the manual port-forward-dependent route. Broker recovery assertions remain. Export: `.beads/issues.jsonl:1953`. |
| wamn-ev5g6 | platform apply 4.17: the Job deadline (600 s) equals the host readiness budget, so a failed host start shows no error | C3-5 disposition evidence | This premise already differs from audited source: the outer Job budget is 900 seconds around 600-second readiness. Evidence: `crates/control/lib/src/platform/job.rs:15`. Export: `.beads/issues.jsonl:1919`. |

## Proposed fix epics

These proposals cover all 222 findings. Shared finding IDs close only after all listed work lands. Each crate move updates dependency gates, test selection, and affected artifact pins. Owner answers gate policy changes and deletions.

### Epic 1: Crate consolidation: shared contracts

Area 4, shared contracts. Closes B1-1, B1-5, B1-6, B1-7, B3-1.

1. Extract pure package contracts and canonical WIT below their consumers (closes B1-6, B3-1)
2. Remove compiler dependencies from serving owners and separate catalog installation behavior (closes B1-1, B1-5, B3-1)
3. Enforce dependency kinds and feature boundaries across all three retained workspaces (closes B1-7)

Depends on: none

### Epic 2: Crate consolidation: provisioning and compilation

Area 4, shared contracts. Closes B2-2, B3-2, A3-12, A1-6.

1. Consolidate provisioning and schema declarations under one grant-policy owner (closes B2-2, B3-2)
2. Combine build tools and split generation into typed semantic stages (closes B3-2, A3-12)
3. Remove confirmed unused native dependencies after isolated feature analysis (closes A1-6)

Depends on: Epic 1

### Epic 3: Crate consolidation: native execution owners

Area 4, shared contracts. Closes B1-3, B3-2, A1-2, A1-13, A3-10.

1. Consolidate identity and runtime ownership while retaining independent artifact transport (closes B1-3, B3-2)
2. Consolidate workflow and edge modules, remove the unused cache, and restrict implementation visibility (closes B3-2, A1-2, A1-13)
3. Group host service packages and separate host and CDC lifecycle modules (closes B3-2, A3-10)

Depends on: Epic 2

### Epic 4: Crate consolidation: control applications

Area 4, shared contracts. Closes B1-4, B2-3, B2-5, B2-8, B2-9, A3-11, B3-2.

1. Extract the package executor and return provisioning credentials directly to environment callers (closes B2-3, B2-9)
2. Extract immutable release construction and reuse artifact transport (closes B2-8)
3. Move developer and delivery engines to their applications and remove umbrella coordinators (closes B1-4, B2-5, A3-11, B3-2)

Depends on: Epic 3

### Epic 5: Crate consolidation: guest ownership

Area 4, shared contracts. Closes B3-4, B3-5, B3-3, A1-10, A3-6, A2-8.

1. Consolidate guest data modules and retain the canonical PostgreSQL error type in SQLx (closes B3-4, A3-6)
2. Consolidate isolated guest helpers and move Modbus and fixture roots without merging capability artifacts (closes B3-5, A2-8)
3. Remove unused guest dependencies, rebuild changed components, and repin their artifacts (closes A1-10, B3-3)

Depends on: Epic 4

### Epic 6: Crate consolidation: test ownership

Area 6, tests and tools. Closes B1-2, B3-2, B3-3, B3-6, G1-1, A1-12, A2-6.

1. Consolidate lightweight support and named database fixtures outside production libraries (closes G1-1, A1-12, A2-6)
2. Move cross-owner tests and remove lower-layer development back-edges (closes B1-2)
3. Consolidate suite and tool packages while preserving target discovery and test authority restrictions (closes B3-2, B3-3, B3-6)

Depends on: Epic 5

### Epic 7: Bounded command execution

Area 4, shared implementation owners. Closes A2-2, B2-4, D2-4.

1. Give finite child processes one executor with caller-owned cancellation (closes A2-2, B2-4)
2. Migrate retained command adapters with deadlines, secret suppression, descendant cleanup, and lock release (closes D2-4)

Depends on: Epic 4

### Epic 8: One upgrade path: schema ownership

Area 2, lifecycle and schema. Closes C1-1, C1-5, C2-10, C3-2, A2-5, E1-10.

1. Establish system, project, and package migration streams through one executor (closes C1-1, C3-2)
2. Replace structural repair and repeated prerequisites with migration ownership (closes C1-5, C2-10)
3. Compose authority SQL once and omit the unused application API-key substrate (closes A2-5, E1-10)

Depends on: Epic 4

### Epic 9: One upgrade path: bootstrap and history

Area 2, lifecycle and schema. Closes C1-4, C1-6.

1. Make control bootstrap, domain assignment, and ledger recording transactional (closes C1-4)
2. Define supported baseline histories and refuse unknown, altered, or newer records (closes C1-6)

Depends on: Epic 8

### Epic 10: One artifact path: deployment declarations

Area 3, artifacts and deployment. Closes C3-1, D1-1, D3-4, H3-2.

1. Pin the platform bundle and application release in their owning KCL documents (closes C3-1)
2. Derive package closure and host inputs from compiled declarations (closes D1-1, D3-4)
3. Remove environment JSON sidecars and migrate every reader (closes H3-2)

Depends on: Epic 4

### Epic 11: One artifact path: complete publication

Area 3, artifacts and deployment. Closes D2-1, D2-2, H1-1, D2-3, D1-6.

1. Build the complete application closure from a clean checkout (closes D2-1)
2. Remove the older image pipeline and publish the dedicated CDC image through the canonical build (closes D2-2, H1-1, D1-6)
3. Publish content-addressed outputs before exposing one complete platform manifest (closes D2-3)

Depends on: Epic 5, Epic 10

### Epic 12: One artifact path: installation inputs

Area 3, artifacts and deployment. Closes B2-7, D2-6, D3-1, D1-7, D1-8.

1. Make installation consume published package, browser, and platform artifacts (closes B2-7, D2-6, D3-1)
2. Select one pinned operator artifact for cluster and host installation (closes D1-7)
3. Replace manual administrative Jobs with the bundle-selected runner (closes D1-8)

Depends on: Epic 11

### Epic 13: One deployment renderer

Area 3, artifacts and deployment. Closes D2-5, D3-5, D1-2, D1-3, D1-4, D1-5.

1. Render platform and environment resources through one Helm package with separate installation scopes (closes D2-5, D3-5, D1-4)
2. Derive cloud destinations and render one complete collector definition (closes D1-2, D1-3)
3. Render local and deployed hosts from shared policy and explicit environment differences (closes D1-5)

Depends on: Epic 10, Epic 12

### Epic 14: One upgrade path: locked maintenance transitions

Area 2, lifecycle and schema. Closes B2-1, C2-1, C2-2, C2-3, C2-6, C3-3, C2-8, C3-5, D3-7, C1-2, C1-3.

1. Route CLI and UI mutations through one executable transition with cluster and environment locks (closes B2-1, C2-2, C2-3, C3-3, D3-7)
2. Advance schemas before dependent reads through the selected runner and remove synthetic uninstall passes (closes C1-2, C1-3, C2-1, C2-6)
3. Drain affected writers and remove online stages, floors, and history-selected rollback under the maintenance model (closes C2-8, C3-5)

Depends on: Epic 7, Epic 9, Epic 13

### Epic 15: One upgrade path: observed recovery

Area 2, lifecycle and schema. Closes C2-4, C2-5, C2-7, C2-9, A3-11.

1. Replace mutable completion shortcuts with current resource and Helm observations (closes C2-4, C2-5)
2. Preserve durable credential recovery before deleting private outputs (closes C2-7)
3. Reconcile current package objects and finish removing the independent upgrade coordinator (closes C2-9, A3-11)

Depends on: Epic 14

### Epic 16: Complete transition qualification

Area 3, artifacts and deployment. Closes C3-4, D3-2, B2-6, F3-5.

1. Identify complete predecessor and target bundles, schemas, artifacts, and test definitions (closes C3-4, D3-2)
2. Qualify transitions through the production executor on isolated disposable targets (closes C3-4)
3. Replace application-name selection with capability-based platform cases and explicit application delivery policy (closes B2-6, F3-5)

Depends on: Epic 15

### Epic 17: Shared deployment fixtures

Area 6, tests and tools. Closes D2-7, D3-6, F1-7, F3-4, H1-5, H1-6, H1-4.

1. Give cluster qualification one fixture that consumes canonical artifacts and provider values (closes D2-7, D3-6)
2. Move common application setup and generic platform assertions to platform owners (closes F1-7, F3-4, H1-6)
3. Migrate identity and RC orchestration while preserving refusal cases and bounded argument handling (closes H1-5, H1-4)

Depends on: Epic 6, Epic 16

### Epic 18: Retire deployment prototypes

Area 3, artifacts and deployment. Closes D1-9, D1-10, H1-12.

1. Move surviving assertions to current renderers and remove obsolete deployment examples (closes D1-9, D1-10)
2. Remove the applied rename tool and retired gate build and image-removal commands (closes H1-12)

Depends on: Epic 17

### Epic 19: Tenant credential and organization isolation

Area 1, authority and credentials. Closes E1-1, E2-1, E2-2.

1. Replace ambiguous credential resource names without an ambiguous-name fallback (closes E1-1, E2-1)
2. Bind database authority routines to the authenticated organization and remove broad runtime writes (closes E2-2)

Depends on: Epic 13

### Epic 20: Membership and grant ownership

Area 1, authority and credentials. Closes E1-3, E1-4, E1-9.

1. Serialize grant projection across hosts and recover from committed authority (closes E1-3)
2. Serialize membership changes and require active membership at session admission (closes E1-4)
3. Enforce covering-grant and grantability rules in the common mutation boundary (closes E1-9)

Depends on: Epic 14, Epic 19

### Epic 21: Bounded cached and offline authority

Area 1, authority and credentials. Closes E1-2, E1-6, E1-7, E1-8.

1. Keep grant answers local and scope broker identities to invalidation authority (closes E1-2)
2. Preserve absolute credential and cache deadlines (closes E1-6, E1-7)
3. Give offline signing keys a finite trust period (closes E1-8)

Depends on: Epic 20

### Epic 22: Authenticated internal transport

Area 1, authority and credentials. Closes E1-5, E2-3, E2-4.

1. Require authenticated TLS for native database clients (closes E1-5, E2-3)
2. Require authenticated TLS for replication and refuse plaintext database admission (closes E2-3)
3. Encrypt event clients and authenticate broker peers with owned certificate renewal (closes E2-4)

Depends on: Epic 13

### Epic 23: Secret and service boundaries

Area 1, authority and credentials. Closes E2-5, E2-6, E2-7.

1. Replace standing administrator fixtures and restrict backup credentials by cluster prefix (closes E2-5)
2. Suppress secrets in help and diagnostics and remove unnecessary secret arguments (closes E2-6)
3. Run standalone services without root privileges and restrict their writable mounts (closes E2-7)

Depends on: Epic 17, Epic 22

### Epic 24: Maintained supply-chain inputs

Area 1, resource limits and supply chain. Closes E3-2, E3-8, E2-8.

1. Upgrade vulnerable native dependency families while preserving source identity rules (closes E3-2)
2. Require fresh advisory results and assign refresh ownership across maintained dependency domains (closes E3-8)
3. Pin shipping build and infrastructure images by reviewed digest (closes E2-8)

Depends on: Epic 6, Epic 11

### Epic 25: Artifact admission and execution limits

Area 1, resource limits and admission. Closes E3-1, E3-3, E3-4.

1. Derive component admission facts from exact bytes and enforce runtime socket refusals (closes E3-1)
2. Bound artifact transfer, extraction, entry counts, and temporary storage (closes E3-3)
3. Enforce aggregate guest memory budgets in serving engines (closes E3-4)

Depends on: Epic 4, Epic 24

### Epic 26: Compiler and connector containment

Area 1, resource limits and admission. Closes E3-5, E3-6, E3-7.

1. Inspect imports before compilation and make compilation bounded and cancellable (closes E3-5)
2. Bound connector code and byte retention by active binding ownership (closes E3-6)
3. Set required process environment values before runtime and library initialization (closes E3-7)

Depends on: Epic 25

### Epic 27: PostgreSQL authority lifetime

Area 1, authority and credentials. Closes A3-1, A3-2, A2-4.

1. Preserve binding failure categories at the PostgreSQL boundary (closes A3-1)
2. Consolidate invocation scope ownership and make authority removal infallible (closes A3-2)
3. Share transaction and tracing machinery after resolving named or default authority (closes A2-4)

Depends on: Epic 3

### Epic 28: Explicit runtime boundary results

Area 4, shared contracts. Closes A3-3, A3-4, A3-9.

1. Replace blobstore prose classification with contextual errors and fixed boundary messages (closes A3-3)
2. Return explicit workflow settlement decisions and deadline adjustments (closes A3-4)
3. Refuse malformed client error envelopes without fabricated details (closes A3-9)

Depends on: Epic 3

### Epic 29: Contextual library errors

Area 4, shared contracts. Closes A3-5, A3-7, A3-8.

1. Replace the identified public `anyhow` boundaries with owner-defined results (closes A3-5)
2. Preserve internal causes and translate diagnostics at public boundaries (closes A3-7)
3. Share credential parsing errors while retaining capability-specific authority rules (closes A3-8)

Depends on: Epic 4

### Epic 30: Canonical contract implementations

Area 4, shared contracts. Closes A2-1, A2-3, A2-11, A2-12, F1-6, A3-13, A2-15.

1. Unify scalar spelling and reuse shared application cursor and scalar functions (closes A2-1, F1-6)
2. Share explicit host shapes and separate routing and manifest ownership without changing canonical bytes (closes A2-3, A3-13)
3. Reuse SQL quoting and digest formatting and unify missing-attachment handling (closes A2-11, A2-12, A2-15)

Depends on: Epic 1, Epic 5

### Epic 31: Generated operation boundaries

Area 5, application authoring. Closes F1-1, F1-2, F1-4.

1. Generate custom publication schemas and definition hashes (closes F1-1)
2. Generate Rust boundary types and conversions from operation declarations (closes F1-2)
3. Derive overlay wire shapes from pinned base artifacts (closes F1-4)

Depends on: Epic 4, Epic 30

### Epic 32: Standalone application assembly

Area 5, application authoring. Closes F1-3, F1-5, F3-1, A1-11.

1. Generate a standalone starter and assemble CRUD components without authored wrappers (closes F1-3, F3-1)
2. Generate direct statement projection handlers (closes F1-5)
3. Emit only the codec support and metadata that declared operations require (closes A1-11)

Depends on: Epic 2, Epic 5, Epic 31

### Epic 33: Web consolidation: dependency boundaries

Area 5, application authoring. Closes H3-1, H3-3, H3-9.

1. Apply the pinned wfe repairs through the parent installation (closes H3-1)
2. Declare workspace dependencies and consume public package entries (closes H3-3)
3. Move chart type requirements to their owner and remove unused exports and dependencies (closes H3-9)

Depends on: none

### Epic 34: Web consolidation: build and browser support

Area 5, application authoring. Closes A2-9, H3-4, H3-6, H3-8, H3-7.

1. Consolidate Vite, Solid, and worker build policy (closes A2-9, H3-4)
2. Share bundle parsing assertions and apply one authored-code lint policy (closes H3-6, H3-8)
3. Share browser fixture and transport support while retaining scenario entry points (closes H3-7)

Depends on: Epic 33

### Epic 35: Web consolidation: standard application screens

Area 5, application authoring. Closes F1-8, F3-3, H3-5.

1. Move shared sync presentation into the public platform UI owner (closes H3-5)
2. Drive standard CRUD controllers from generated descriptions (closes F1-8)
3. Build default application screens through the SDK shell without an authored web project (closes F3-3)

Depends on: Epic 31, Epic 32, Epic 34

### Epic 36: Saved-edit correctness

Area 5, application authoring. Closes F2-1, F2-2, F2-3.

1. Prepare changed source before enforcing artifact freshness (closes F2-1)
2. Derive watched files from actual build inputs (closes F2-2)
3. Preserve the active application until a compatible candidate can activate (closes F2-3)

Depends on: Epic 4

### Epic 37: Private development startup

Area 5, application authoring. Closes F2-4, F2-5, F2-6, F2-9, F3-2.

1. Own private databases, brokers, telemetry, ports, and cleanup (closes F2-5, F2-9)
2. Derive coordinates, schemas, and routine startup inputs from their owning documents (closes F2-4, F2-6)
3. Make bare `wamn dev` prepare and watch an application before its first Git commit (closes F3-2)

Depends on: Epic 13, Epic 32, Epic 36

### Epic 38: Reproducible checkout preparation

Area 5, application authoring. Closes F2-7, F2-8.

1. Establish supported host prerequisites and report missing tools before preparation (closes F2-8)
2. Prepare only selected artifact closures and propagate preparation failures directly (closes F2-7)

Depends on: Epic 4, Epic 32

### Epic 39: Measured author deployment

Area 5, application authoring. Closes F3-6, F3-7, F2-11.

1. Deliver exact runtime and browser artifacts through one retryable author command (closes F3-6)
2. Exercise the standalone hello-app journey within the agreed 600-second boundary (closes F3-7)
3. Record cold startup, warm startup, and saved-edit durations with cache state (closes F2-11)

Depends on: Epic 16, Epic 35, Epic 37, Epic 38

### Epic 40: Test support: shared mechanics

Area 6, tests and tools. Closes G1-2, G1-3, G1-4, G1-5, G1-6, G1-8, G1-11, G1-14, G1-15, A2-13.

1. Share PostgreSQL connections, command capture, URL replacement, and disposable-role operations (closes G1-2, G1-3, G1-4, G1-11)
2. Share cluster command capture and distinct private-file creation and replacement operations (closes G1-5, G1-6)
3. Share filesystem ownership, repository paths, package copying, and small clock helpers (closes G1-8, G1-14, G1-15, A2-13)

Depends on: Epic 6, Epic 7, Epic 22

### Epic 41: Test support: fixture data

Area 6, tests and tools. Closes A2-7, G1-7, G1-9, G1-10, G1-12, G1-13.

1. Share minimal package, environment, and scoped workflow fixtures (closes A2-7, G1-7)
2. Share admitted-component and router graph builders (closes G1-9, G1-10)
3. Share recording credential stores and offline runtime builders (closes G1-12, G1-13)

Depends on: Epic 40

### Epic 42: Restore disabled behavioral coverage

Area 6, tests and tools. Closes G2-1, G2-2, G2-3.

1. Restore history reconstruction, retention, and authority assertions (closes G2-1)
2. Replace shared-fixture grant counts with explicit visibility contracts (closes G2-2)
3. Restore complete introspection equality and refusal coverage (closes G2-3)

Depends on: Epic 9, Epic 41

### Epic 43: Retain contracts and remove incidental pins

Area 6, tests and tools. Closes G2-4, G2-5, G2-6, G2-7.

1. Restore registry rendering and explicit local/deployed host parity cases (closes G2-4)
2. Run owned PostgreSQL credential retry cases by default (closes G2-5)
3. Remove incidental counts and complete DDL snapshots while retaining semantic and frozen identity assertions (closes G2-6, G2-7)

Depends on: Epic 13, Epic 40

### Epic 44: Test execution: tiers and selection

Area 6, tests and tools. Closes G3-1, G3-2, G3-6, F2-10, H1-2.

1. Split mixed targets and assign each target to one execution tier (closes G3-1, G3-6)
2. Centralize selection policy and fail when Git change discovery fails (closes G3-2, H1-2)
3. Prepare selected artifacts and services and require every selected case to execute (closes F2-10)

Depends on: Epic 6, Epic 17, Epic 41

### Epic 45: Test execution: isolated contracts and prerequisites

Area 6, tests and tools. Closes G3-4, G3-5, G2-8.

1. Separate ordinary client tests from generated-contract compatibility targets (closes G3-5)
2. Normalize prerequisite declarations and name identity executable discovery (closes G2-8, G3-4)
3. Move ignored subprocess helpers into support executables (closes G3-4)

Depends on: Epic 32, Epic 44

### Epic 46: Qualified merge and promotion evidence

Area 6, tests and tools. Closes D3-3, G3-3, H1-9, D2-8, G3-7.

1. Run the submodule guard and required live suites against one combined merge head (closes G3-3, H1-9)
2. Qualify deployment artifacts and refuse stale or unqualified promotion (closes D3-3)
3. Retain selected and executed cases, artifact identities, cache outcomes, and separate stage durations (closes D2-8, G3-7)

Depends on: Epic 16, Epic 17, Epic 24, Epic 42, Epic 43, Epic 45

### Epic 47: Remove dormant native surfaces

Area 4, shared implementation owners. Closes A1-1, A1-3, A1-4, A1-5, A1-7, A1-8, A1-9, A3-14.

1. Remove unreachable startup measurement and unused recovery machinery (closes A1-1, A1-3)
2. Remove unused pool, engine, authority, wiring, and credential-placeholder APIs (closes A1-4, A1-5, A1-8, A1-9)
3. Remove unused generator methods and enforce scoped handwritten lint overrides (closes A1-7, A3-14)

Depends on: Epic 6, Epic 32

### Epic 48: Shared repository tooling

Area 6, tests and tools. Closes A2-10, A2-14, H1-10, H1-11, H1-3.

1. Share pinned installer declarations and installation mechanics (closes A2-10, H1-10)
2. Share minimal launcher plumbing without changing runner failure policies (closes A2-14, H1-11)
3. Move descriptor-limit assertions into discoverable host tests and derive their verdict from selected cases (closes H1-3)

Depends on: Epic 7, Epic 17

### Epic 49: Benchmark result integrity

Area 6, tests and tools. Closes H1-7, H1-8.

1. Retain source, artifact, workload, conflict, and failure details in one benchmark result contract (closes H1-7)
2. Bound cloud discovery and own sampler cleanup, trigger restoration, and disposable resources (closes H1-8)

Depends on: Epic 37, Epic 40

### Epic 50: Documentation ownership: current rules

Area 6, documentation. Closes H2-1, H2-3, H2-4, H2-8.

1. Give implemented contracts one architecture or operations owner (closes H2-1)
2. Move standing repository and operational rules out of completed plans (closes H2-4)
3. Preserve unresolved design and replace stale archive recommendations and incomplete navigation (closes H2-3, H2-8)

Depends on: none

### Epic 51: Documentation ownership: history and obsolete records

Area 6, documentation. Closes H2-2, H2-5, H2-6, H2-7, H3-10.

1. Reconcile live work before replacing completed plans and copied histories with immutable references (closes H2-2, H2-7)
2. Repair retained links and distinguish available historical evidence from unavailable artifacts (closes H2-5, H2-6)
3. Remove the obsolete tracker export and temporary file after resolving external readers (closes H3-10)

Depends on: Epic 18, Epic 46, Epic 50

The table counts distinct finding IDs within each epic. Shared IDs make those counts overlap. Effort describes the complete epic.

| Epic | Issues | Findings closed | Estimated effort |
| --- | ---: | ---: | --- |
| 1 | 3 | 5 | L |
| 2 | 3 | 4 | L |
| 3 | 3 | 5 | L |
| 4 | 3 | 7 | L |
| 5 | 3 | 6 | L |
| 6 | 3 | 7 | L |
| 7 | 2 | 3 | M |
| 8 | 3 | 6 | L |
| 9 | 2 | 2 | M |
| 10 | 3 | 4 | L |
| 11 | 3 | 5 | M |
| 12 | 3 | 5 | L |
| 13 | 3 | 6 | L |
| 14 | 3 | 11 | L |
| 15 | 3 | 5 | L |
| 16 | 3 | 4 | L |
| 17 | 3 | 7 | L |
| 18 | 2 | 3 | M |
| 19 | 2 | 3 | L |
| 20 | 3 | 3 | L |
| 21 | 3 | 4 | L |
| 22 | 3 | 3 | L |
| 23 | 3 | 3 | M |
| 24 | 3 | 3 | M |
| 25 | 3 | 3 | M |
| 26 | 3 | 3 | L |
| 27 | 3 | 3 | L |
| 28 | 3 | 3 | M |
| 29 | 3 | 3 | L |
| 30 | 3 | 7 | M |
| 31 | 3 | 3 | M |
| 32 | 3 | 4 | L |
| 33 | 3 | 3 | M |
| 34 | 3 | 5 | M |
| 35 | 3 | 3 | L |
| 36 | 3 | 3 | M |
| 37 | 3 | 5 | L |
| 38 | 2 | 2 | M |
| 39 | 3 | 3 | L |
| 40 | 3 | 10 | M |
| 41 | 3 | 6 | M |
| 42 | 3 | 3 | M |
| 43 | 3 | 4 | M |
| 44 | 3 | 5 | L |
| 45 | 3 | 3 | M |
| 46 | 3 | 5 | L |
| 47 | 3 | 8 | M |
| 48 | 3 | 5 | M |
| 49 | 2 | 2 | M |
| 50 | 3 | 4 | M |
| 51 | 3 | 5 | M |

## Open questions for the owner

1. Can shared contracts sit below generation, provisioning, and serving, including a shared scalar dependency for record-history? This unblocks Epics 1 and 30 (B1-1, B3-1, A2-1).
2. Which external consumers require current Rust APIs or Cargo package names, including the control, harness, and fixture packages? This unblocks Epics 1–6 and 47 (B3-2, B2-5, G1-1, A1-7 through A1-9).
3. Can root supply retained feature results and generated packages before dependency removal? This unblocks Epics 2 and 5 (A1-6, A1-10).
4. Must wiring caching remain a supported requirement despite its absent production caller? This unblocks Epic 3 (A1-2).
5. Can grouped host and scenario binaries share broader Cargo feature selection while retaining separate processes and credentials? This unblocks Epic 3 (B3-2).
6. Can guest moves, component repinning, and resulting qualification occur in one release cycle? This unblocks Epic 5 (B3-3 through B3-5).
7. Which machine and cache states define timing comparisons, and can the build owner supply isolated cold and incremental results for the six development back-edges? This unblocks timing acceptance for Epics 6 and 46 (B1-2, G3-7).
8. Which installed databases require retained data, and which permit one explicit reset before migrations replace full-schema installation? This unblocks Epics 8 and 9 (C1-1, C1-6, C3-2).
9. Must authored SQL remain directly executable, or can composed installation recipes produce standalone SQL as build output? This unblocks Epic 8 (A2-5, C3-2).
10. Can Epics 8 and 14 require maintenance windows and remove online stages and floors (C3-5, C2-8)? If old and new images must serve together, must the floor executor land now or remain unsupported?
11. Which external workflows still consume deployment JSON or prototype backup, retention, registry, and bootstrap manifests? This unblocks Epics 10 and 18 (H3-2, D1-9, D1-10).
12. Can `environment.k` own local and deployed coordinates, with the default developer command generating its private local document? This unblocks Epics 10 and 37 (D3-4, F2-4, F3-2).
13. Can kind and GCP consume one release build graph, with local builds retained as a developer adapter? This unblocks Epic 11 (D2-2).
14. Which local/deployed host behaviors must match, and must WMS retain its distinct concurrency limit? This unblocks Epics 13 and 43 (D1-5, G2-4).
15. Can `upgrade-environment` disappear after its preparation, resume, and credential-retirement requirements move to the shared owners? This unblocks Epic 14 (B2-1, C2-2, C2-7, D2-1).
16. Can production `apply-package` become an internal environment phase, with any retained standalone command limited to explicitly owned disposable targets? This unblocks Epic 14 (C2-2).
17. What live result closes `wamn-m6or2` after fixture correction `92cf2aded`? This defines retained preload acceptance for Epic 14 (C1-2).
18. Can R12 change so that qualification identifies complete transitions, including charts, schemas, CDC, auxiliary images, and qualification executables? This unblocks Epic 16 (C3-4, D3-2, D1-6).
19. Who approves qualification cases for unfamiliar applications, and which application business tests must deployment require beyond generic platform qualification? This unblocks Epics 16 and 39 (B2-6, F3-5).
20. Can the identity journey replace its repository `evidence/` requirement with caller-owned temporary result storage? This unblocks Epic 17 (H1-5).
21. Which disposable GKE project, existing fixtures, and budget support provider-specific tests before merge and promotion? This unblocks Epics 17 and 46 (D3-6, D3-3, G3-3).
22. Does any external workflow still require `kind-gate-build` or `kind-gate-image-remove`? This unblocks Epic 18 (H1-12).
23. Which deployed coordinates currently produce colliding credential names? This unblocks safe replacement in Epic 19 (E1-1, E2-1).
24. Must an organization control credential remain trusted with authority across every organization? This decides the database boundary in Epic 19 (E2-2).
25. Is every operator certificate intentionally platform-wide, and how must one certificate be revoked without replacing its CA? This informs privileged credential boundaries in Epics 19 and 22 (E2-2, E2-3).
26. Must tenant isolation survive compromise of one native host or theft of its scheduler certificate? This defines acceptance for Epic 21 (E1-2).
27. Are cache delays permitted only after lost invalidation, or also after natural credential expiry? This unblocks Epic 21 (E1-6, E1-7).
28. What maximum disconnected period permits continued trust in an edge signing key set? This unblocks Epic 21 (E1-8).
29. Which independently enforced network controls protect deployed identity databases, plaintext services, and standing development fixtures? This establishes the live boundary for Epics 22 and 23 (E1-5, E2-3 through E2-5).
30. Who owns certificate renewal and the chart CA’s recorded 365-day expiry? This unblocks transport lifecycle acceptance in Epic 22 (E2-3, E2-4).
31. Who owns refreshes for WASI-Virt, its adapter, and the pg-walstream patch, and what refresh interval applies? This unblocks Epic 24 (E3-8).
32. Does another gate scan the web dependency graph, including the pinned wfe dependency resolution? This determines the remaining scan scope in Epic 24 (E3-8).
33. Must WASI-Virt provenance report a measured adapter digest instead of its current constant? This unblocks provenance acceptance in Epic 24 (E3-8).
34. Is registry write access intentionally sufficient to bypass component admission? This decides the trusted publication boundary in Epic 25 (E3-1).
35. What measured condition permits serving hosts to leave aggregate memory count mode? This unblocks Epic 25 (E3-4).
36. Must poisoned authority state terminate the process, or must the host isolate the affected owner and become unready? This unblocks Epic 27 (A3-2).
37. Which blobstore `Other` messages are frozen consumer contracts? This unblocks replacement of backend prose in Epic 28 (A3-3).
38. Can host operations explicitly share authored shapes while resolved contract bytes and generated clients remain unchanged? This unblocks Epic 30 (A2-3).
39. Must SQL literal quoting support nondefault PostgreSQL string parsing, or can callers require the default server behavior? This defines quoting acceptance in Epic 30 (A2-11).
40. Must HTTP exposure remain explicit for every custom public operation? This unblocks generated publication policy in Epic 31 (F1-1).
41. Can Acme consume its pinned base operation’s wire shape while retaining separate identity and inspection behavior? This unblocks Epic 31 (F1-4).
42. Can pure CRUD applications use generated component crates without authored Rust files? This unblocks Epic 32 (F1-3, F3-1).
43. Can WAMN consume only public built wfe exports while source qualification remains inside the submodule? This unblocks Epic 33 (H3-3).
44. Which retained saved-edit acceptance run passed against the current freshness, watcher, and activation rules? This supplies the comparison result for Epic 36 (F2-1 through F2-3).
45. Can the normal developer command own the complete private stack, with external services available through explicit overrides? This unblocks Epic 37 (F2-5, F2-6, F3-2).
46. Can the default author experience require Docker on Linux, or must the first release support another host platform? This unblocks Epics 37 and 38 (F2-8, F3-2).
47. Which cold-start, warm-start, saved-edit, build, qualification, and commit-to-running limits must the retained results enforce? This unblocks acceptance for Epics 39 and 46 (F2-11, D2-8, G3-7).
48. Does the ten-minute clock start with the installed SDK, Docker, and provisioned account, or include their installation and provisioning? This unblocks Epic 39 (F3-7).
49. Can root supply live statuses for quarantined tests and archive candidates, including which ignored cases represent defects or retired contracts? This unblocks Epics 42, 45, and 51 (G2-1 through G2-4, G3-4, H2-2, H2-3).
50. Which dashboard or collector owns the frozen PostgreSQL telemetry identifiers whose guard cites deleted documentation? This unblocks retained-contract decisions in Epics 43 and 51 (G2-6, H2-5).
51. Which CI service and protected reference own integration and promotion? This unblocks Epic 46 (D3-3).
52. Must startup-burst measurement remain supported despite its absent repository execution path? This unblocks Epic 47 (A1-1).
53. Must benchmark summaries remain readable without their raw result files? This defines the retained result contract in Epic 49 (H1-7).
54. Does an immutable Git source plus a compact history entry satisfy the rule that accepted plans remain untrimmed? This unblocks Epic 51 (H2-2, H2-7).
55. Where are the twelve historical performance artifacts retained, if anywhere? This unblocks evidence disposition in Epic 51 (H2-6).
56. Does any external tool still read `.beads/beads.jsonl`? This unblocks obsolete-export removal in Epic 51 (H3-10).

The separate developer-engine reuse question is omitted because B2-5 supplies a library target for the proposed `services/dev` owner. External API requirements remain in question 2. Nothing from this list is filed until the owner answers.
