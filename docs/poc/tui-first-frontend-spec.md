# Generated operator TUI — spec, rev 5

**Status:** ruled 2026-09-08, four external review rounds applied — implementation-ready · supersedes the slice-v
deferral of Levels 3–4 · ordering rule: **IR completeness and the shared
request/submission layer precede screen emission.**

## 0. Prerequisites (their own beads, before any screen is emitted)

**P1 — IR completeness.** `OperationIr` gains `kind` (carried from the manifest,
not inferred). Commands gain an explicit **target record link** projected from
declared facts only: the `guards: <relation>` of a state command, the claim's
relation of a claim command, and the input field that carries that relation's
key — never a `*_id` name heuristic. Where no declared link exists the command
is menu-only. Every revision-bearing operation (state, update, delete) binds its record
and expected revision **only from a declared, compatible record read and revision
mapping**; where none exists it is shown as *requires composition* and its
submission is disabled until ordinary Rust composition supplies the binding.
No user-entered revisions, no guessed read routes. **Read-to-input population** (a projection's rows filling a
command's repeated input; a list supplying a selector) requires a declared
fact; none exists today and no name heuristic replaces it. Until one is
declared and ruled, that population is **ordinary Rust composition over
generated screens** — the generated form offers the repeated editor with
typed inputs and nothing pre-filled. `FieldIr` becomes a tree (nested objects, repeated rows with
declared bounds) and carries **two independent facts** — `required` (the property must be present)
and `nullable` (its value may be null) — projected from the contract; the
editor's `Absent | Null | Value` states are permitted per field by that pair.
All four combinations are tested (required/non-null, required/nullable,
omittable/non-null, omittable/nullable); `supplier_id` is one of them. `RouteIr` projects the **served
route's response contract** (the terminal wiring's output) **and the served
operation's replay guarantee** (`idempotent_by`, or none for a composed
route) — the only channel by which P2 learns whether captured retry is
permitted; absent or unknown replay information grants none, and screens
never reconstruct it from names or an inner command's claim — so a composed
route's result is typed; fields the IR cannot type render generically, **display-only and marked
opaque**; a response violating a known contract is malformed under P2, and an
unsupported input type never degrades to an unrestricted text input — it
renders as *unsupported* and blocks submission. Each query's list columns come from **that query's result
descriptors**, not the model union.

**P2 — request/submission layer** in `wamn-client-tui`, shared by generated
and scaffolded screens:
- `Draft`: per-field `Absent | Null | Value`, nested and repeated editors with
  bounds, typed inputs; builds a request only from a valid draft.
- **Canonicalization is `wamn-client`'s at request build**: descriptor-typed
  UUID, timestamp, and numeric text are re-spelled there; `invoke` stays a
  byte canonicalizer. Stated once, tested once.
- **Submission lifecycle:** `Editable → Pending → Succeeded | Refused |
  PartiallyCompleted | Uncertain`, decided in the **shared reducer** from the
  served response's evidence: a confirmed refusal without completion keeps
  the editable draft; **confirmed partial completion** shows the committed
  result beside the failure, spends the submission, and offers no
  whole-command resubmission; completion not established is `Uncertain`.
  An unknown error literal, a malformed response, or a transport failure is
  never mapped to an editable refusal. The submitted payload and key are captured; **retry replays the
  captured command byte-for-byte** (same key, same `occurred_at`, same
  `expected_row_version`); a new command is an explicit act; no submission
  while one is pending; refusal keeps the draft, success spends it,
  `Uncertain` offers **retry-captured only where the served operation's replay
  contract makes it safe** — `idempotent_by: claim` (same key, same result);
  a `state` command offers *refresh the record* — not because a retry must
  conflict (an unexecuted original might succeed) but because no same-result
  replay is guaranteed;
  a composed route with downstream effects offers refresh with an explicit
  warning that repeating may repeat effects. "Byte-for-byte" means the
  **command body**; authorization headers are re-derived. Editing a refused
  draft is a new intent; abandoning an uncertain one cancels nothing on the
  server. **Completion evidence concerns the whole submission intent, not the
  latest attempt:** a later attempt's refusal (a `403` on a captured retry,
  say) does not clear an earlier `Uncertain` — the captured submission and
  its uncertainty are preserved until evidence resolves *that* submission,
  and the retry's refusal is shown separately. Paired reducer test:
  first-attempt authorization refusal → editable draft; uncertain submission
  then retry refusal → still `Uncertain`.
- **Session binding:** the TUI is bound to `(url, host, target instance)` from
  `run served`; a re-serve with a different target instance resets all
  data-dependent state (records, revisions, cursors, drafts, pending) and
  blocks submissions during replacement; old-target mutations are never
  replayed against a new target. The binary may be reused; the state is not.

`.45` (bindings per run) rides with P1 — same stage, same IR.

## 1. What "generate" emits (after P1/P2)

A Rust crate `generated/<package>-tui/`, regenerated every run, never edited:

| IR fact | Screen |
|---|---|
| `kind: query` | List: that query's result columns; filter bar over `FilterIr` plus any other declared inputs under the same input rule; keyset pager; opaque cursor preserved, reset on filter/sort change |
| `kind: get` | Detail: inputs populated only through declared mappings or Rust composition; remaining user-editable inputs (optional included) exposed; invoke only when every required input is satisfied; then field/value view |
| `kind: projection` | Same input rule as `get`; then by result cardinality: one row → detail; bounded list → table; paged → table + pager |
| `kind: command` / `create` / `update` | Form over the operation's `FieldIr` tree; envelope fields supplied by the layer from the bound record and the submission intent |
| `kind: delete` | Form + confirmation; binds its record and expected revision under the same rule as state/update commands |
| `ErrorCaseIr` | Error rendering per case (presentation only — whether the submission is `Refused`, `PartiallyCompleted`, or `Uncertain` is P2's decision) (field highlight, both versions, missing token, named id); unknown cases in the status line |
| operation with no `RouteIr` | Listed as *not exposed*; never given an invented call path |
| `event_handler` | Not shown |

**Batch scope:** the initial TUI submits **one outer envelope item per
submission**; nested repeated fields (receipt lines) are supported inside it.
Multi-item envelopes are a later, separately ruled behavior.

Navigation: models menu → list → detail → linked commands (P1's declared link) →
form → result; unlinked commands under an operations menu. One binary,
`wamn-<package>-tui`, shared terminal driver, one terminal owner; `Esc` from an
editing form asks before discarding a draft, `q` refuses while `Pending`.

## 2. Where in the loop

`Generate` emits it beside the IR and bindings, **per emission**: re-emit when
the IR or the emitter/generation inputs change; recompile when emitted source
or build dependencies change; identical generation inputs produce identical
output. Unrelated application generation is never skipped on the TUI's account. `Build` compiles it as a native host binary. It is
a client: no Virtualize/Admit/Publish/Release/Activate involvement.

CLI axes stay independent: `--tui <package>` launches after `Activate`
succeeds; `--hold` keeps the run served; `--watch` rebuilds — with
`--watch --tui <package>`, a change rebuilds and **restarts** the TUI after the
re-serve, applying the session-binding rule above. No live reload.


## 3. Customization

`wamn ui scaffold <package> [screen]` copies a generated screen into a
developer-owned crate as ordinary Rust over the same primitives and the same
P2 layer. Composition is plain Rust: the generated crate exports each screen as
a function; the custom crate's `main` calls generated screens for what it
doesn't override and its own for what it does. Deleting a custom screen means
calling the generated one again — no registry. Guarantee stated exactly:
**typed API incompatibilities fail the scaffold's build; a scaffold must pass
its declared interaction tests against regenerated bindings**
(`scaffold_tracks_contract`) — which catches regressions its assertions
cover, not additive contract changes in general; an additive field a
scaffold ignores is legitimate.

## 3a. Inherited rules

The generated binary carries no endpoint, host, or token — all arrive at
launch from `run served` and `dev.json`. Server authorization is
authoritative: a `403` renders, nothing is hidden client-side. Envelope fields
are platform law and never user-typed; the cursor is opaque.

## 4. Proof

1. Determinism: identical generation inputs → byte-identical crate.
2. Coverage: every callable operation in Receiving and WMS has a screen, a
   *not exposed over HTTP* entry, or a *requires composition* entry (the
   three are distinct); every `ErrorCaseIr` a rendering; one case of a
   revision-requiring command with no usable exposed record read; one read
   entered from a list with an additional required input still unbound,
   proving invocation waits for it.
3. **Request correctness** (actual outgoing bytes): absent/null/value for
   `purchase_order.update.supplier_id`; nested `value.line[]` with bounds;
   typed canonicalization; cursor opaque and reset on filter change.
4. **Lifecycle:** double-submit blocked; lost-response retry replays the
   captured command **only where the replay contract allows** — tested both
   ways: a direct claim-backed call offers captured retry; a composed route
   whose first node is claim-backed but whose downstream effects are not
   does not inherit that safety and offers refresh with the warning; target replacement with unchanged IR resets state and
   blocks; a failed run that leaves the previous activation intact keeps its TUI
   usable; once the old target is invalidated the TUI is unavailable until a
   matching activation succeeds (both sides tested); late responses from an
   old session cannot reach the new one (process restart isolates); restart during
   `Pending` does not resubmit and does not claim the request was cancelled.
5. **Receiving parity:** the *composition* — generated screens plus a small
   developer-owned Rust crate wiring projection → line editing → location
   selection → submission — passes the hand-written reducer's interaction
   tests. The hand-written screens are deleted only when that passes, not
   when every operation has a form.
6. **WMS:** move through the form; the label key renders because the served
   route's response contract is projected (P1), not special-cased. *Movement
   committed, label failed* renders **only when the served response carries
   that evidence** — the composed route's error contract must include the
   committed node's result beside the failed node's outcome (a platform item
   filed with P1; the effects epic's outcome vocabulary is its source). Without
   that evidence the TUI shows *outcome unknown*. The mapping is tested in P2's shared reducer, not
   only in WMS rendering; the platform's partial-completion payload stays
   narrow (committed result + failed outcome), never a dump of node results.
7. `[GENERATED-TUI]` recipe, verified by running it.

## 5. Work items, in order

P1 IR completeness (+ `.45`) → P2 request/submission layer → emitter →
loop launch/restart → scaffold → Receiving parity + deletion → **WMS proof,
which depends on the platform's partial-completion response contract
(filed with P1; the effects epic's outcome vocabulary is its source)** →
recipe.
