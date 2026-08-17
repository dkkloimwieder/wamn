# Authoring surface contract

Status: normative for the MVP authoring wire surface. The Rust source of truth
is `wamn-authoring-model`; the generated language-neutral contract is
[`authoring-surface.schema.json`](../contracts/authoring-surface.schema.json).

## Boundary and version

The authoring contract is a client boundary shared by checkout tools, CI,
agents, IDE integrations, and future frontends. It contains data only and
defines no private handler, database credential, shell host, or operator path.
Trusted principal identity is adapter context and never a client field.

The program-wide wire version remains `0.1`. Unknown fields are rejected at
every typed boundary. An unsupported or missing version is refused before an
operation handler runs.

Requests and responses are wrapped by `AuthoringDocument`, tagged as
`document: request` or `document: response`. Its body is one of two structurally
disjoint envelopes:

- a command request is exactly `{ schema-version, command-id, command }`;
- a command response echoes `schema-version` and `command-id` around an
  operation-specific completion or refusal;
- a query request is exactly `{ schema-version, query-id, query }`; and
- a query response echoes `schema-version` and `query-id` around an
  operation-specific completion or refusal.

`command-id` is correlation plus principal-scoped exact-retry identity. The
complete key is `(tenant-id, principal-id, command-id)`: a rotated or reissued
principal cannot replay its predecessor, and another principal using the same
command ID executes independently without learning a stored outcome. The
closed typed request is projected to JSON and canonicalized with the platform's
RFC 8785-style canonical JSON bytes before hashing. The same key and hash
returns the exact stored full response envelope bytes; the same key with a
different hash refuses as `command-id-reuse` without mutation.

`catalog.authoring_command_audit` is the sole retry ledger. The command,
principal-scoped ledger lock, lookup, mutation, and completed/refused outcome
insert share one transaction, so no pending row can commit. The schema-control
cutover evolves this relation only while it is empty; populated legacy history
refuses with SQLSTATE `55000` and requires archive plus reprovision (or the
documented destructive environment loop).
The tenant-scoped unique `audit-id` remains a server-assigned evidence identity;
it is not client input and does not participate in retry identity.

`query-id` is correlation only. It is a nonempty UTF-8 string of at most
`MAX_QUERY_ID_BYTES` (64) bytes. It never selects retry identity, never enters
the command ledger, and is recorded only as a trace field. The query adapter
writes no durable row and no query-log relation exists.

Optional save provenance (`commit`, nullable `ref`, `dirty`) is the client's
attribution claim about its checkout. It never selects a principal, role, or
result.

## Complete operation inventory

There are exactly five commands and three queries.

| Operation | Class | Input | Completed result |
|---|---|---|---|
| `save-flow-draft` | command | scope, draft/flow IDs, expected revision, exact definition text, optional provenance | draft identity and new revision |
| `validate` | command | scope and exact draft revision | one validated executable identity |
| `draft-run` | command | scope, validated draft, one input, optional `capture: full \| off` | run receipt |
| `test-set-run` | command | scope and validated draft; cases come from the draft's own `cases` array, not a separate inline definition (`wamn-0h0g.15.27`) | report/test-set receipt |
| `publish` | command | scope, validated draft, successful report ID | immutable published flow identity |
| `read-draft` | query | scope and exact draft revision | exact stored definition and identity |
| `get-run` | query | scope and run ID | bounded author-facing run/node projection |
| `get-report` | query | scope and report ID | pending or immutable finalized report |

Every success and refusal carrier is operation-specific. A response whose
operation discriminator differs from the request is a protocol fault.

The retained callee-validation refusals are exactly:

- `unresolvable-callee-name`;
- `missing-recorded-callability`; and
- `contract-incompatibility`.

Cycle/depth/expanded-plan vocabulary, `SuiteRun`, `SuiteProjection`,
`GrantDraftSafeGeneration`, and `RevokeDraftSafeGeneration` are absent.

## Public bounds

The one surviving named test-set ceiling is `MAX_TEST_SET_CASES = 256`, declared
by this contract's own source of truth (`wamn-authoring-model`) and mirrored by
the owning `cases` parser in `wamn-flow`.

> **`MAX_TEST_SET_BYTES` and `MAX_TEST_SET_EXPECTATIONS` were deleted by
> `wamn-0h0g.15.27`** (`3a042d96`, "flatten the test contract and delete its
> store"). Neither name survives anywhere in the tree; they are retained here
> only so the earlier three-ceiling wording is traceable.

Boundary tests pin exact-at-limit acceptance and first-over-limit refusal in the
owning parser. Test sets are non-vacuous: a caller that requires a test set
refuses an empty case list, while flow validation applies the bounds only to a
flow that already carries at least one case (absent and empty `cases` are the
same bytes). Each case carries exactly one `expect` observable, not an
expectation list — also `wamn-0h0g.15.27`.

Every `uint64`-formatted authoring integer uses the inclusive wire domain
`0..=SAFE_INTEGER_MAX`, where `SAFE_INTEGER_MAX = 9_007_199_254_740_991`
(`2^53 - 1`). Values at `2^53` or above are refused rather than rounded.

## Query and mounting boundary

This contract cut defines and generates all eight operations. Production
mounting belongs to the operation-integration owner. Until that cut, the
management query adapter is intentionally unmounted: it accepts the closed
query envelope only far enough to emit the `authoring_query` trace span with
`query_id` and operation kind, then returns bare HTTP 501. It has no storage
backend, SQL, command-ledger method, or success-construction path.

## Draft-safe connection authority

`catalog.draft_safe_connection_grants` remains the provision-seeded record for
the sole draft-safe sandbox generation. It is not a public mutation surface.
Management and runtime receive only the SELECT authority needed by validation
and draft admission; they cannot INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES,
or TRIGGER it. Publish never mutates the relation.

Provisioning ownership may seed or reconcile the owner-approved row. The
deleted Grant/Revoke command vocabulary and host-side mutation helpers do not
survive as compatibility paths.

## Generated artifacts and reference client

The checked-in JSON Schema, HTTP collection, response examples, generated
TypeScript, runtime validator, and reference CLI are one drift-checked surface.
The client exposes structurally separate command and query transport methods.
It validates query IDs by UTF-8 byte length and verifies both correlation and
operation echoes before returning a typed outcome.

The reference CLI builds every operation document. Its `validate` verb composes
save plus validate; `draft-run`, `test-set-run`, and `promote` cover the other
commands; `read-draft`, `get-run`, and `get-report` cover the correlation-only
queries. It receives a caller-supplied endpoint and token file and has no
database or operator capability.

## Regeneration

```bash
cargo run --locked --offline -p wamn-authoring-model \
  --example print-authoring-surface-schema \
  > docs/archive/contracts/authoring-surface.schema.json
cargo test --locked --offline -p wamn-authoring-model
(cd clients/authoring-client && npm run generate && npm run check:generated && npm test)
```
