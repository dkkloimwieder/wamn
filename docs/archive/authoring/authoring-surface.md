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

The authenticated principal is trusted adapter context and is deliberately
absent from the client-controlled JSON. A request that injects a principal,
credential, database URL, endpoint, execution bundle, frontend state, or shell
host fails closed as an unknown field.

**Source provenance is different, and is a client field.** The platform runs no
Git: it never clones a repository, never reads a commit, and never verifies a
checkout. A client is therefore the only thing that can know where it read a
definition from, so `save-flow-draft` carries an optional `provenance` object
(`commit`, nullable `ref`, `dirty`) recording the client's own claim about its
working tree. It is **attribution and never authorization**: it selects no
principal, widens no role, and changes no result. Two commands differing only
in provenance produce the same outcome and store the same document. It is
recorded verbatim on the command ledger beside the verified principal that
actually authorized the command, and no read path may substitute one for the
other. It carries no identity-shaped field for exactly that reason.

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
| `save-flow-draft` | project/environment scope, draft and flow IDs, expected revision, exact UTF-8 flow-document text, optional commit provenance | exact draft ID, flow ID, and new revision | authorization denied, revision conflict |
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

`wamn-ftfc.2` implements the checkout-file save path: an authenticated client
reads working-tree definition files and submits their content, with optional
commit provenance, through this contract. The platform hosts no server-side Git
adapter. `wamn-ftfc.14` implements the headless `validate`, `draft-run`, and
`suite-run` client verbs. Neither may introduce a private command or bypass a
canonical handler.

## Stable identity

All node and graph identities are scoped by the exact `ValidatedDraftIdentity`
carried by the projection.

- A mutable draft revision is `(draft-id, revision)`; its returned identity also
  carries the flow ID. Save stores the submitted definition **byte for byte** as
  text and does not parse or validate it, so a half-finished or emptied file is
  a preserved draft rather than a failed command. `validate` parses the stored
  text at its own stage and owns the typed refusal.
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

## Integer wire domain

The normative wire domain for every `format: uint64` field on this contract is
`[0, 2^53-1]` — inclusive, with `2^53-1 = 9007199254740991`. This is the whole
domain, not a client-side accommodation: the schema publishes `maximum`
`9007199254740991` on all six uint64 sites (`SaveFlowDraft.expected-revision`,
`DraftRevisionRef.revision`, `DraftIdentity.revision`,
`AuthoringRefusal.revision-conflict`'s `expected-revision` and
`actual-revision`, and `DraftSuiteProjection.edit-to-run-ms`).

The bound is `Number.MAX_SAFE_INTEGER`, the largest integer an IEEE-754 double
holds exactly, so a JavaScript client reproduces the exact value every time
without a lossless-number parser. Nothing is string-encoded, and no field has a
second spelling.

`maximum` is published as an integer literal, never as `9007199254740991.0`:
`serde_json` reads that float form back as `9007199254740990`, which would move
the contract one below the value it admits.

Boundary behaviour is deterministic in both directions:

| Value | Accept (decode) | Emit (encode) |
| --- | --- | --- |
| `9007199254740991` (`2^53-1`) | accepted, round-trips exactly | emitted |
| `9007199254740992` (`2^53`) | refused | unrepresentable |
| `18446744073709551615` (`u64::MAX`) | refused | unrepresentable |

`wamn_authoring_model::SafeUint64` is that boundary. Its `Deserialize` refuses
an out-of-domain integer rather than rounding it, so a refusal surfaces through
the existing `ContractDecodeError::Json` decode rejection — a malformed body,
answered `400`, with no new wire vocabulary. In the emit direction the type
itself is the guard: `TryFrom<u64>` and `TryFrom<i64>` are the only ways to
build one, so a server value outside the domain cannot reach a serializer at
all. The TypeScript client's `Number.isSafeInteger` check in
`clients/authoring-client/src/validate.ts` is the same contract stated on the
client side; it is final, not a stopgap.

Compatibility: this narrows the published schema and is recorded as such.
No valid traffic changes. Storage behind every one of these fields is
PostgreSQL `bigint` (`i64`), and each field carries a server-assigned counter
(draft revisions) or a measured latency (`edit-to-run-ms`) — none can
legitimately approach `2^53`, so no producer that was previously correct can
emit a value the bound now refuses. Settled by `wamn-ftfc.21`.

## Digest ordering

Every definition digest a client sees — `graph_hash`, `artifact_hash`,
`draft-content-hash`, `validated-draft-id`, `execution-bundle-hash`, and the
source/attachment/release definition hashes — is SHA-256 over a canonical
preimage. Three rules are normative.

1. **Preimages use canonical stable-ID ordering.** Object keys are RFC 8785:
   UTF-16 code-unit order, no insignificant whitespace (`wamn-flow`'s
   `canonical` module; `wamn-catalog`'s `write_json`). Repeated preimage frames
   are ordered by the member's stable id — node id for graph nodes
   (`wamn-flow`'s `FlowPreimage`), node type for resolved nodes
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
   `draft-revision`, and an edge's `ordinal` — never an element's position in a
   document array. Changing an explicit order value changes the digest; permuting
   a sequence that has no explicit order must not. An edge's `ordinal` is its
   fan-out position within its `(from, from-port)` group, which is the order the
   engine dispatches a branch's targets in (`Plan::successors`). A client may
   omit it: the platform then materializes the edge's position within its group
   once, at parse, and returns it on export. Array position is read at that one
   point and never again — after it, moving an edge in the array changes nothing
   and changing an `ordinal` changes both the run order and the digest.
3. **Display never enters a preimage.** Labels, prose, and canvas coordinates
   are not identity. The graph document has no coordinate field at all and
   rejects one as an unknown field. The display fields it does carry — a flow's
   `name`, a node's `label`, and a credential declaration's `kind` and
   `description` — are stored and returned verbatim but never hashed, so
   renaming is not a new artifact. `kind` is display by decision: it is
   documented as a hint for the editor's credential picker, the vault resolves a
   credential by `name`, and nothing in the platform reads it.

Every graph digest hashes one projection, `wamn-flow`'s `FlowPreimage`, which
both `Flow::canonical_bytes` and `DraftContentHash::for_flow` build on, so a
field reaches a graph digest only by being listed there. Node frames are ordered
by node id (wamn-jvzx.14) and edge frames by the stable edge key
`(from, from-port, ordinal, to, to-port)` (wamn-jvzx.15, which also adds the
`duplicate-edge` refusal that makes that key total). Permuting a document's
`nodes` or `edges` array is proved not to move `graph_hash` or `artifact_hash`
by `node_sequence_position_must_not_change_the_graph_digest`,
`artifact_hash_must_not_depend_on_node_document_sequence`, and
`edge_sequence_position_must_not_change_the_graph_digest`; the converse — that an
explicit `ordinal` *is* identity — by
`explicit_edge_ordinals_are_materialized_at_parse_and_do_change_the_digest`; and
the exact preimage bytes by `graph_preimage_bytes_are_pinned`. Display text is
excluded by construction (wamn-jvzx.16) and proved excluded by
`editor_labels_must_not_enter_the_graph_preimage` and
`artifact_hash_must_not_depend_on_editor_labels`, while
`every_flow_field_is_classified_as_identity_or_display` pins which document
fields are identity and which are display at the flow, node, edge, and
credential level — so a field added to the document cannot reach a digest, or be
silently kept out of one, without that test failing.

Because these are the digests of an already-content-addressed store, a change to
the projection changes every previously computed digest. There is no backfill:
the project is greenfield (FLOW-SPEC §preamble), `catalog.flow_artifacts` and
`catalog.validated_flow_drafts` are physically immutable, and a row written
before the change fails closed in `PinnedArtifact::from_storage` with
`GraphHashMismatch` rather than loading under the wrong rules. From-zero
reprovisioning is the migration story.

All three rules now hold with no carried deviation: a client may reorder a
document's `nodes` or `edges` arrays and may retitle a flow, a node, or a
credential declaration without minting a new artifact identity. What does change
an identity is a change to the graph itself, including an edge's explicit
`ordinal`. Note that a rename is still a new *document*: `catalog.flow_artifacts`
is immutable and keyed by `(tenant, flow, version)`, and `register_flow_artifact`
compares `graph_json`, so re-registering a renamed graph at the same flow version
is refused with `flow-version-content-conflict` even though its hashes match.

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
2. The client submits `save-flow-draft` with the file's exact bytes, its own
   optional commit provenance, and expected-revision concurrency.
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
  > docs/archive/contracts/authoring-surface.schema.json
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
request, and rejects privileged fields.

`clients/authoring-client/scripts/smoke.mjs` (wamn-jvzx.4) is the authenticated S0
smoke over that same collection. It logs in as a real principal, presents the
issued token, and sends the collection's own `save-flow-draft` request, deriving
every executable field from the checked-in section so a hand-rolled divergence on
either side fails before a byte is sent. `--check` is its network-free drift half
and runs inside `node scripts/test.mjs`. It reads no ledger and holds no database
URL: attribution evidence is a runner-side step documented in the
`[6A / wamn-jvzx.4]` section of `docs/archive/build-and-test.md`.

The headless CLI word `promote` maps to the public `publish` command. Login and
token issuance and a generic `runs` route are intentionally absent: the current
public schema defines neither, and `wamn-jvzx.13` owns their collection entries
after those contracts land. Report reads use the schema's `suite-projection`
command.

## Headless reference client

`clients/authoring-client/scripts/wamn.mjs` (`wamn-ftfc.14`) is that client: five
verbs — `validate` (`save-flow-draft` then `validate`), `draft-run`, `suite-run`,
`promote`, and `runs` — over the generated HTTP client, with the first-party PAT
flow and no frontend artifact. Each invocation emits one machine-readable
document carrying typed identities, a typed product refusal, a typed `unmounted`
answer for a command kind the adapter has not mounted, or a fault, and reports
edit-to-run latency measured from the working-tree file it submitted. Its gates,
including the composed edit-to-publish cycle and the checks that unversioned,
unauthorized, privileged-database, direct-handler, and frontend-only shortcuts
all fail, are the `[6A / wamn-ftfc.14]` section of `docs/archive/build-and-test.md`.
