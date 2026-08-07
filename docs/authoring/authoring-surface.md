# Authoring surface contract

Status: normative for PLAN item 6A (`wamn-ftfc.13`). The Rust source of truth is
`wamn-authoring-model`; the generated language-neutral contract is
[`authoring-surface.schema.json`](../contracts/authoring-surface.schema.json).

## Boundary

An **editor is a client role, not a platform component**. The authoring surface
is the same for a checkout CLI, CI, an agent or human in an IDE, the future
Studio SPA, and another frontend. No client class receives a private handler or
projection.

The contract contains data only. It defines no HTTP routes, CLI flags, shell
hosting, canvas layout, storage records, or platform operator effects. The
canonical application handlers established by `wamn-ftfc.1` remain responsible
for authorization, validation, optimistic lifecycle transitions, and audit
attribution. Git, CLI, and HTTP are adapters to those handlers.

The authenticated principal and source provenance are trusted adapter context.
They are deliberately absent from the client-controlled JSON. In particular,
Git commit identity supplies attribution but never authorization. A request
that injects a principal, credential, database URL, endpoint, execution bundle,
frontend state, or shell host fails closed as an unknown field.

## Wire root and version

Every document is an `AuthoringDocument`, tagged as `request` or `response`.
Its body carries required `schema-version: "0.1"` and `command-id`. The command
id is the correlation and exact-retry identity; an adapter must preserve it
when retrying the same logical command.

Missing, unknown, and unsupported versions are rejected before selecting an
application handler. Unknown fields are rejected at every struct boundary.
`0.1.x` permits compatible semantic clarification only. Removing or renaming a
field or variant, changing an identity, or making an explicit state implicit
requires a new contract version and compatibility path.

## Commands

These are the complete minimum-loop commands. The names describe use cases,
not transports.

| Command | Client request | Completed result | Product refusals |
|---|---|---|---|
| `save-flow-draft` | project/environment scope, draft and flow IDs, expected revision, exact UTF-8 flow-document text | exact draft ID, flow ID, and new revision | authorization denied, revision conflict |
| `validate` | scope, exact draft revision, stored-suite reference | validated-draft ID plus exact draft, artifact, bundle, catalog, environment, and proposed runtime-version pins | authorization denied, missing draft revision, invalid draft, catalog drift, unresolved nodes |
| `draft-run` | scope, opaque validated-draft reference, one input | run ID and exact validated-draft reference | authorization denied, missing/drifted validated draft, draft connections denied |
| `suite-run` | scope, opaque validated-draft reference, stored-suite reference | durable report and execution IDs plus exact validated-draft reference | authorization denied, missing suite/draft, validated-draft drift, draft connections denied, undrivable nodes |
| `publish` | scope, exact validated-draft reference, successful report ID | immutable flow/version/artifact identity | authorization denied, missing identity, unsuccessful suite, executable drift, nonterminal-run lifecycle conflict |
| `suite-projection` | scope and report ID | not-found, pending, or finalized versioned projection | authorization denied; infrastructure faults stay on the fault plane |

The existing `.11` backend seams map without becoming public storage types:

- `save-flow-draft` maps to the canonical save use case represented internally
  by `SaveFlowDraft`;
- `validate` maps to validation of one exact saved revision and returns an
  opaque reference to `.11`'s immutable validation pins;
- `suite-run` maps to `.11`'s exact validated-draft execution and durable report
  reservation;
- `suite-projection` maps to `.11`'s missing/pending/finalized report query and
  is extended with node/branch/edge observations by `wamn-ma5`;
- `draft-run` remains a distinct canonical use case even where it reuses the
  validated-draft execution machinery; and
- `publish` consumes the exact tested executable and successful report. It may
  not rebuild, re-resolve, or substitute an artifact during publication.

`wamn-ftfc.2` implements the Git-backed save adapter. `wamn-ftfc.14` implements
the headless `validate`, `draft-run`, and `suite-run` client verbs. Neither may
introduce a private command or bypass a canonical handler.

## Stable identity

All node and graph identities are scoped by the exact `ValidatedDraftIdentity`
carried by the projection.

- A mutable draft revision is `(draft-id, revision)`; its returned identity also
  carries the flow ID. Save preserves invalid intermediate text and does not
  parse or validate it.
- `validated-draft-id` is the opaque handle accepted by later commands. The
  validation result and finalized projection expose the exact draft revision,
  artifact hash, execution-bundle hash, catalog ID/version, environment, and
  proposed runtime flow version behind that handle.
- A durable report is `report-id`; its execution is `execution-id`; every case
  supplies stable `case-id` and `run-id` values.
- A node is keyed by `node-id` from the authored graph.
- A branch is the exact `(from-node-id, from-port)` tuple.
- An edge is the exact `(from-node-id, from-port, to-node-id, to-port)` tuple.
  `from-port` is always explicit. `to-port` is required and nullable, so omitted
  defaults cannot produce ambiguous keys.

Clients must compare these structural fields. Display labels and prose are not
identities.

## Digest ordering

Every definition digest a client sees — `graph_hash`, `artifact_hash`,
`draft-content-hash`, `validated-draft-id`, `execution-bundle-hash`, and the
source/attachment/release definition hashes — is SHA-256 over a canonical
preimage. Three rules are normative.

1. **Preimages use canonical stable-ID ordering.** Object keys are RFC 8785:
   UTF-16 code-unit order, no insignificant whitespace (`wamn-flow`'s
   `canonical` module; `wamn-catalog`'s `write_json`). Repeated preimage frames
   are ordered by the member's stable id — node type for resolved nodes
   (`validate_interface_order`), node id for occurrence recovery
   (`conservative_occurrence_recovery` with `validate_occurrence_recovery`),
   logical name for execution plugs and adapters (`validate_named_order`), and
   member id for release artifacts/sources/attachments and attachment source
   ids (`validate_sorted_unique`). A client that supplies another sequence is
   refused (`CatalogIdentityError::NonCanonicalInterfaceOrder` /
   `NonCanonicalMemberOrder`, or `wamn-flow`'s `unsorted-connection-requirements`,
   `unsorted-credentials`, `unsorted-allowed-hosts`), never silently hashed.
2. **Author-meaningful order is an explicit order field.** Where order carries
   meaning it is a value the client sets — `version`, `catalog-version`,
   `draft-revision` — never an element's position in a document array. Changing
   an explicit order value changes the digest; permuting a sequence that has no
   explicit order must not.
3. **Display never enters a preimage.** Labels, prose, and canvas coordinates
   are not identity. The graph document has no coordinate field at all and
   rejects one as an unknown field.

Known deviations from rules 1 and 3 are carried as ignored proof fixtures, not
as prose: `node_sequence_position_must_not_change_the_graph_digest`,
`edge_sequence_position_must_not_change_the_graph_digest`, and
`editor_labels_must_not_enter_the_graph_preimage` in
`crates/execution/flow-model/tests/digest_ordering.rs`, plus
`artifact_hash_must_not_depend_on_node_document_sequence` in
`crates/catalog/model/tests/digest_ordering.rs`. Each names the follow-up that
closes it. Until they close, a client must treat a flow document's node and
edge array order and its `name`/`label`/`description` text as digest-affecting
and must not reorder or relabel a graph it intends to keep identical.

## Suite projection

A finalized `DraftSuiteProjection` carries:

- `projection-version`, report/execution identity, exact validated-draft pins,
  and stored-suite identity;
- a structurally exclusive suite outcome (`passed`, `failed`, or a typed
  refusal), case pass/fail, linked case/run IDs, structured failure kind and
  optional failing node;
- edit-to-run latency when execution reached admission;
- a complete node list with `passed`, `failed`, `not-observed`, or `unknown`;
- complete branch and edge lists with `covered`, `not-covered`, `not-observed`,
  or `unknown`; and
- refusal outcome variants for undrivable nodes, validated-draft drift, or
  denied draft-safe connections.

The state meanings are deliberately explicit:

| State | Meaning |
|---|---|
| node `passed` | the node was observed and every observed case passed it |
| node `failed` | at least one observed case linked a failure to the node |
| node `not-observed` | the complete run evidence contains no execution of the node |
| node `unknown` | retained evidence cannot establish an honest node result |
| coverage `covered` | at least one retained case observed the branch/edge traversal |
| coverage `not-covered` | the source node was observed, but the branch/edge was never traversed |
| coverage `not-observed` | the source node itself was never observed |
| coverage `unknown` | retained evidence cannot establish traversal or non-traversal |

An absent array member never means uncovered. Implementations enumerate nodes,
branches, and edges from the exact pinned graph and emit a state for each.
Similarly, `to-port: null` is different from an omitted edge-key field.

Pending report reservations preserve `.11`'s honest durability boundary.
`awaiting-admission` and `capture-interrupted { run-ids }` are pending states,
not refusals. Retrying a capture-interrupted identity must not rerun, resume,
fabricate, or finalize missing evidence.

Infrastructure failures are faults, not product refusals. An adapter may map a
fault to its transport's failure mechanism, but it must not rewrite one as a
validation, authorization, node, or coverage result.

## Headless reference composition

The wave reference client is a checkout and CLI/API, with no frontend artifact:

1. Edit the canonical flow files.
2. The Git adapter submits `save-flow-draft` with authenticated provenance and
   expected-revision concurrency.
3. Run `validate` and retain the returned `validated-draft-id`.
4. Run `draft-run` for an authored input.
5. Run `suite-run`, then read the durable `suite-projection` until finalized.
6. Submit `publish` with that exact validated-draft ID and successful report ID.

CI must prove this edit → validate → draft-run → suite-run → publish sequence
with zero frontend code and without a database URL, platform credential,
private handler, or privileged `wamn-ctl` recovery/operator path. Studio later
renders the same command results and projection; it does not define another
authoring model.

## Regeneration and checks

```bash
cargo run --locked --offline -p wamn-authoring-model \
  --example print-authoring-surface-schema \
  > docs/contracts/authoring-surface.schema.json
cargo test --locked --offline -p wamn-authoring-model
```

The package test compares the generated schema byte-for-byte with the checked-in
file and guards the command inventory, typed refusals, explicit coverage states,
full edge key, version checks, and privileged/frontend-field rejection.

## Schema-first request collection

[`authoring-surface.v0.1.http`](../contracts/authoring-surface.v0.1.http) is the
transport-neutral request console for the six commands in schema version 0.1.
Its paired
[`authoring-surface.v0.1.examples.json`](../contracts/authoring-surface.v0.1.examples.json)
contains one typed success and refusal for every command. The collection sends
every document to the complete adapter endpoint supplied in
`WAMN_AUTHORING_ENDPOINT`; it does not define a route. Each request requires
`WAMN_AUTHORING_BEARER_TOKEN`, supplied for the current principal by the caller.
There is no checked-in token, fallback token, or unauthenticated request.

For a human, export those two variables in a private shell and run an individual
section with an HTTP-file client. For an agent, preserve each `command-id`,
consume responses as `AuthoringDocument`, and never insert principal, token,
database, endpoint, or operator authority into a JSON body. For CI, run the
static, network-free gate:

```bash
cargo test --locked --offline -p wamn-authoring-model \
  --test request_collection
```

The gate decodes every request, success, and refusal through the Rust source of
truth, compares the collection inventory with the generated and checked-in
schema, requires the environment-only Bearer header on every executable
request, and rejects privileged fields. A later authenticated smoke is owned by
`wamn-jvzx.4`.

The headless CLI word `promote` maps to the public `publish` command. Login and
token issuance and a generic `runs` route are intentionally absent: the current
public schema defines neither, and `wamn-jvzx.13` owns their collection entries
after those contracts land. Report reads use the schema's `suite-projection`
command.
