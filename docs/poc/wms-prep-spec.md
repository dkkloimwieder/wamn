# WMS-prep lane — simulator, label node, blobstore capability

Status: rev 4 RATIFIED 2026-09-02 · three external review rounds incorporated · phase 2 unblocked (.4.6 closed at 56f4e34f) ·
three-phase sequencing; phase 1 is parallel-safe now, phase 2 waits for
wamn-10yt.4.6, phase 3 waits for the stable release path.

## Phase 1 — NOW (parallel-safe; touches no open slice-iv surface)

### 1a. Simulator framework (`test-support/simulator`)

Deterministic, seeded event streams; every portfolio app's driver is a
profile.

- `Profile { seed, rate, duplicate_pct, reorder_window, fault_plan }` →
  iterator of typed events. Timestamps and ids are generated
  deterministically **in-stream**; wall-clock pacing (`rate`) is an
  emitter concern. Same seed = byte-identical stream (canonical JSON via
  the existing shared canonicalization — no new serializer).
- Emission targets: trait now; HTTP route driver (array envelope, PAT
  auth) implemented now — reuse the existing authenticated route-driver
  client implementation where practical; its protocol structure is not
  an API contract. JetStream
  sink: **trait only** — app 2 defines the external ingress contract
  first; the simulator never manufactures internal WAMN CDC/event
  envelopes (post-.4.5 identity rules refuse them anyway).
- First profiles: `scan_events`, `seed_inventory`.
- Law restated in-crate: simulators drive real routes/consumers, never
  the database.

### 1b. Label-render palette node (`components/no-std/label-render`)

- Pure transform: a field record in, one ZPL label out. Effect-free,
  allowlist imports only, gate-caseable. no_std, existing workspace.
  **`template_id` is a wiring PARAMETER, not an input field** (owner ruling
  2026-09-02, superseding this line's original `{ template_id, fields } →
  { zpl }` sketch). Template choice is authoring intent: a wirer picks
  "pallet label" once and the wiring then declares honestly what it renders,
  so gate cases can pin golden output per wiring; an input-supplied template
  would make every caller a template chooser and the wiring's behaviour
  caller-dependent. The closed set therefore rides the declaration's parameter
  schema as a typed `enum`, so an unknown `template_id` refuses at
  gate/config-validation time, not at first invocation.
- Embedded closed template set (`pallet`, `location`, `product`);
  template authoring demand-gated.
- Golden ZPL outputs for all three templates as the unit gate. Vectors state
  label geometry (`^PW812 ^LL1218 ^MD0`, i.e. 4in × 6in at 203 dpi) because
  geometry-free ZPL is not printable; the stock is a **provisional** guess and
  moves with the eventual WMS printer fixture decision (owner ruling
  2026-09-02).

### 1c. Blobstore S3 spike (isolated; no platform integration)

- Standalone prototype of the host-side S3 client against disposable
  MinIO. Prior art **to be pinned by the spike** (exact repo + revision
  is a required spike output): the upstream standalone S3 provider
  (upstream v2.8's built-in blobstore dir has filesystem/in-memory/NATS
  only — no S3), plus the `blobby` and `http-blobstore` examples for
  interface usage.
- Output: a measured client choice (crate, size, auth model) and a spike
  report. No admission, connection, or catalog changes.

## Phase 2 — AFTER .4.6 closes (platform capability work)

### 2a. Effectful-standard-interface admission (the real design item)

Current `is_effect_package()` hardcodes "WASI namespace = ambient,
wamn namespace = effect" — true only by accident of the 4-element
allowlist. Replace the heuristic with declaration:

- **Capability registry**: one row per admitted package —
  `{ package, version, posture: ambient | effect }` — closed set,
  ruled expansion. Posture is package-grain; a mixed-posture package
  triggers a split ruling if one ever appears. **Version matching is
  exact**: admission requires the imported package version to equal the
  registry row's version — no normalization, no ranges (compatible-range
  rules are lifecycle machinery, parked on .7).

  **Row set superseded 2026-09-02 by measurement** (owner ruling; see
  `docs/architecture/2a-capability-registry.md`). This line previously read
  `wasi:clocks/io/random/logging: ambient`; `wamn:postgres`,
  `wasmcloud:blobstore`: `effect` — six rows. The measured tenant surface is
  **seven existing plus one new**: it omitted `wamn:node` and
  `wamn:connection`, both really granted today
  (`components/no-std/publish.sh` grants `wamn:connection` to
  `http-request`). The basis also changes: rows record what the host
  **offers**, not what a guest currently imports, because a registry keyed on
  imports drops a live capability the moment its last importer churns —
  `wasi:logging` is host-implemented (`plugins/wamn_logging.rs`) with zero
  importers today.

  Ambient: `wamn:node@0.1.0`, `wasi:logging@0.1.0-draft`, `wasi:io@0.2.12`,
  `wasi:clocks@0.2.12`, `wasi:random@0.2.12`. Effect:
  `wamn:postgres@0.1.0`, `wamn:connection@0.1.0`,
  `wasmcloud:blobstore@0.1.0` (new, 2b).

  The `wasi:*` versions are **not authored by us**: measured on `receiving`,
  the authored WIT says `0.2.12`, the raw build imports `0.2.9`, and the
  virtualized artifact admission actually sees imports `0.2.12`. Those rows
  are pinned to the WASI-Virt rev and adapter sha of ledger row 5, and a
  conformance test asserting them against virtualized output is required, not
  optional. Scope is the tenant path (`analyze_tenant`) only, deliberately;
  the `wash push` workload path re-converges at the first
  non-platform-authored pushed workload.
- Admission reads the registry; `is_effect_package()`'s namespace
  heuristic deletes. Interfaces then carry name, shape, AND posture —
  no second classifier.
- Acceptance: effect-posture facts derive from the registry; the gate's
  effect-free-cases law keys on posture rows; an import absent from the
  registry refuses admission (fail-closed); denial arm proves an
  effect-posture component cannot qualify for the effect-free case
  path (effectful components still gate — validation + compatibility).
- Owner review of this design precedes any phase-2 landing.

### 2b. Blobstore capability integration

- **Contract: `wasmcloud:blobstore@0.1.0`** (async shape; exact, single).
  Upstream v2.8 provides the interfaces and implementation patterns.

  **NO FORK FEATURE-SET CHANGE — measured unnecessary, owner ruling
  2026-09-02, superseding this bullet's original claim** that WAMN's build
  disables `wasi-blobstore` and `wasm_component_model_implements` and that
  enabling both is a deliberate change with its own ledger row. Neither half
  held. `wasm_component_model_implements` was already ON —
  `services/host`, `services/executor` and `crates/execution/host` all enable
  it, and `runtime_inventory.rs`'s feature policy requires it. And
  `wasi-blobstore` is not needed at all: the plugin follows the
  `wamn_postgres` pattern — vendored WIT plus its own `bindgen!` — so it
  needs nothing from upstream's `wasi_blobstore` module. Proven by compiling
  a `bindgen!` of the full 16-method async contract with the feature off,
  with the probe itself proven live by a deliberate world-name break.

  The outcome is stronger than the plan: upstream's blobstore backends never
  compile, so **"never a second registered runtime" becomes a structural
  impossibility** rather than a convention about not calling
  `multiplexed_plugins()`, and the reviewed `wash-runtime` feature allowlist
  is untouched. There is no deviation, so there is no ledger row.
- Connection vocabulary: `ComponentConnectionType` gains `Blobstore`;
  `push_component` maps it to a `blobstore_v1()` descriptor;
  reuses the connection **framework** (requirement → instance →
  binding, admin-bound, host-held credentials); semantics per
  capability — blobstore: endpoint + bucket + prefix + credential;
  postgres: host-selected database authority.
- **Runtime owner: WAMN.** Our plugin implements
  `wasmcloud:blobstore@0.1.0` (pattern: `wamn_postgres`), consuming
  upstream interfaces/types; the upstream S3 provider is prior-art
  source only — never a second registered runtime. WAMN adds
  confinement: binding-scoped credentials, bucket/prefix walls,
  per-effect spans, admission facts.
- Acceptance: credential bytes never cross a guest-visible WIT,
  parameter, config, or binding boundary (provable boundary — not a
  memory claim); put/get/delete/list green through a bound connection
  from a fixture guest; undeclared blobstore import refuses at
  admission.

### 2c. `blob-put` palette node

Blobstore is a guest import, not a `wamn:node` operation — wirings
compose operations. `blob-put` is the small palette node that imports
the capability; label-render stays pure. **At-least-once rule: the
caller supplies a deterministic object key; `put` is overwrite-safe;
the node never generates keys** — redelivery must be an idempotent
overwrite, not a duplicate object.

## Phase 3 — AFTER the release path is stable

- Composed cluster proof: `label-render → blob-put` wiring authored,
  gated (label-render cases effect-free; blob-put carries effect
  posture), published, routed; one label lands in MinIO; span shows both
  nodes with the originating caller.

## Ledger rows

1. `wasmcloud:blobstore@0.1.0` adopted; wamn plugin + confinement;
   deviation only in refused verbs (list them if any).
2. Fork feature-set change: enable `wasi-blobstore` +
   `wasm_component_model_implements` (config-class, deliberate).
3. MinIO as disposable gate infra (config-class).

## Out of scope

WMS package (waits for .4.6 + this lane's phase 2), MQTT seam (app 2),
any change to dispatch/registration/release code paths in phase 1.
