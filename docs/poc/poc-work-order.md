# POC work order — Receiving base + client overlay

Status: RATIFIED as the organizing epic · 2026-08-28 · read alongside
`wamn_base_application_poc_revised.md` and
`wamn_receiving_layered_application_poc_scenario.md` (the design
authority) and `sqlx-data-access-spec.md` rev 3 (already aligned).
Sequencing authority: this document.

## Standing rules for the epic

- **Naming law, ratified now:** singular `snake_case` wire identifiers;
  package = the ownership/version/compatibility boundary, module/domain
  = internal organization only; canonical operation identity
  `<package_id>@<package_version>::<local_operation>`; closed CRUD set
  `get/query/create/update/delete`. The generator emits nothing until
  its output obeys this.
- **Demand order, not spec order.** Each slice lands end-to-end with a
  demonstrable exit before the next opens. No building slice N+2
  machinery inside slice N.
- **No RLS/role redesign.** The landed floor is the floor; findings
  file as post-epic beads unless a live hole.
- **Deletion rule applies:** superseded machinery (client-owned-copy
  remnants, hand-rolled residue the generator replaces) dies in the
  same slice that supersedes it.
- The four-way post-merge wave (.9.7/.10.8/.12.5/.24.5) finishes
  before slice i claims.

## Slices

**i — Introspection → IR.** pg_catalog reader over the closed
supported-object set → normalized catalog IR as a derived release
artifact. Refused-object enforcement. Exit: IR emitted from the
Receiving base migrations, byte-stable across two runs, refused
objects proven refused.

**ii — Generator core + verification.** From the IR: models, static
SQL corpus (generated CRUD + authored `query/*.sql`), typed accessors,
registered operations, input contracts (server-owned fields refused,
three-state update, revision-field concurrency, array envelope with
`per_input`). Two-sibling verification on the exact effective schema;
transport-parity refusal; package weld (verified_schema_state_id,
contracts, corpus identity, provenance). Exit: a broken corpus file
fails the build naming the column; revision conflict returns
`concurrency_conflict`; verified SQL byte-identical to shipped.

**iii — Receiving base package, end-to-end (the product loop).**
Platform package `receiving`: migrations → IR → generated artifacts →
component build → gate → publish → routes → traces. Commands as one
explicit transaction inside one invocation (never across a wiring
edge), through the .22.2a transport. **Folds in the deferred
cluster-proof family (.15.153 + .15.25.4):** the M1 router-era rebuild
(RouterDeliveryBridge, RouterDriver, released wiring, component
closure) runs against this package — one proof, both owners closed.
Exit: the full journey green on a disposable cluster; M1 family
closed; no fabricated routes or authority anywhere in the proof.

**iv — Client overlay.** `client_acme_receiving`: independent
migration stream, effective schema = exact base + exact overlay,
ownership at definition level, overlay operations/routes beside base
ones, base upgrade with unchanged overlay re-verified via contracts
(additive base satisfies without overlay rebuild). Exit: the scenario
document's upgrade walk executes; an overlay touching a base-owned
definition refuses with the typed literal.

**v — UI + TypeScript clients.** The new artifact class last: generated
TS clients, static UI bundles as digest-identified artifacts, npm
distribution shape. Exit: the Receiving UI drives base + overlay
operations against slice-iv's deployment.

## Tracker shape

One epic; one bead per slice; slice beads split per R12 only when a
lane claims. .15.153/.15.25.4 re-parent under slice iii. Post-epic
holds (deferred effect-writer determination .10.15, nullable-wiring
retirement, refinement-class findings) stay out of the epic.
