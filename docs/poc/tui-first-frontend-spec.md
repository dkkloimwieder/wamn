# TUI-first frontend — slice v re-scope (rev 2)

Status: rev 2 · 2026-09-02 · external review incorporated (all ten
findings accepted) · TS bindings are explicitly LOW priority by owner
steer: the IR keeps them honest later; no TS emitter is built until a
web consumer charters.

## Architecture (revised per review)

```text
released / effective package
        ↓
canonical client-contract IR      ← ONE generator input: the
        ↓                            effective-release contract
 ┌──────────────────────┐            projection; wamn.json is a
 │ Rust emitter         │ ← slice v  contributor, not the boundary
 │ TS emitter           │ ← deferred (low priority; not built)
 └──────────────────────┘
        ↓
generated typed bindings + descriptors
        ↓
wamn-client
  - deployment base URL (construction-time)
  - auth provider (static PAT = v1 impl)
  - route construction/discovery (platform route law /
    supplied route metadata — never baked in generated code)
  - operation-driven envelope/error/paging semantics
        ↓
Ratatui primitive controls
        ↓
hand-authored application UI
```

## Rulings (amendments + review corrections)

1. **Slice v = TUI-first.** `.5.1` TS stash parked indefinitely.
2. **One client-contract IR.** Derived from effective-release
   contracts; language emitters consume it. Rust is the only emitter
   built now. Prevents Rust implementation details becoming the future
   TS contract.
3. **Endpoint rule (corrected):** no deployment host/base URL in
   generated code; route construction/discovery belongs to
   `wamn-client`.
4. **Transport semantics are descriptor/operation-driven**, not
   globally assumed — envelope, `per_input`, paging, and error detail
   apply per the operation's declared contract.
5. **Auth is a credential source/provider interface**; static PAT is
   the v1 implementation. Replaceable/refreshable by design.
6. **`occurred_at` is definite:** Receiving requires
   `value.occurred_at: timestamptz, nullable=false`; the client
   supplies it. General rule stays contract-driven.
7. **Screen reads:** compose existing operations client-side if
   sufficient; otherwise add a projection operation as an explicit
   package change (the `custom_operations` projection kind). Measure
   the required line/location data before choosing.
8. **Toolkit split:** primitive controls (table, form, field editor,
   filter bar, pager, dialogs, error/pending/status) build now;
   domain/conventional widgets (`PurchaseOrderTable`) wait for
   second-app evidence.
9. **`.5.6` developer TUI is conditional on the P2 public read API
   existing** as one coherent seam (stages, verdicts, release,
   membership, operations, routes, traces, taps). The frontend slice
   does not invent that API opportunistically.
10. **No UI DSL** (Level 5 refused); scaffolding generates
    developer-owned code only.

## Work items

1. **`.5.2a` client-contract IR** — normalized projection from
   effective-release contracts (models, fields with descriptors,
   operations, errors, filters/sort/paging, permissions, coordinates).
   Byte-stable regeneration.
2. **`.5.2b` Rust emitter** — typed models, operation IO/error types,
   operation client, descriptors. No endpoints, no TS emitter.
3. **`.5.3` `wamn-client`** — base URL + auth provider at
   construction; route construction per platform route law;
   operation-driven envelope/error/paging; `503`/`401`/`403` and
   cursor semantics as ruled.
4. **`.5.4` primitive controls** (per ruling 8).
5. **`.5.5` Receiving operator TUI** — PO list, receipt entry, typed
   failures rendered per the detail matrix; client supplies
   `request_id`, idempotency key, `occurred_at`; real authenticated
   routes.
6. **`.5.6` developer TUI** — gated on ruling 9.

## Tracker mapping

The bead ids do not match this document's item labels, and are not renumbered
to: the titles carry the labels, and renumbering for cosmetics would be churn.

| this document | bead |
|---|---|
| `.5.2a` client-contract IR | `wamn-10yt.5.2` |
| `.5.2b` Rust emitter | `wamn-10yt.5.3` |
| `.5.3` `wamn-client` | `wamn-10yt.5.4` |
| `.5.4` primitive controls | `wamn-10yt.5.5` |
| `.5.5` Receiving operator TUI | `wamn-10yt.5.6` |
| `.5.6` developer TUI | `wamn-10yt.5.7` (blocked) |

`.5.1`'s TS stash is `wamn-10yt.5.1`, parked per ruling 1.

## Exit gates (revised)

1. IR: regeneration byte-identical on unchanged release; contract
   change surfaces in IR without hand edits.
2. Emitter: generated code carries no endpoint; new field/error
   appears in types from IR alone.
3. Transport: stale `row_version` → `concurrency_conflict` with both
   revisions; `401` indistinguishable / `403` names token; cursor
   opaque round-trip — asserted against client + transport + view
   model, **below the terminal layer**.
4. Operator TUI: one terminal-level smoke proof of the receipt-entry
   workflow; all other assertions at the reducer/view-model layer.
5. Developer TUI: renders dev-loop stages from the P2 API; no direct
   DB or platform-verb access.
6. Promotion rule: conventional widgets only on second-app (WMS)
   evidence.

## Authentication

PAT via credential provider (env/config v1); dev tokens through the
existing identity surface. Station/device/badge identity belongs to
portfolio app 8 / identity epic.

## Web transition

Reuse boundary = the client-contract IR and everything above it in the
contract layer. Ratatui and SolidJS remain framework-specific. TS
emitter charters only with a real web consumer.
