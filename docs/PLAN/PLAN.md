# PLAN — scope reduction

Status: **active, non-normative map**. [exe-model.md](../exe-model.md) is the
single WIP design authority. Beads and git own status; this file records only
work order and unresolved exploration or decisions.

## Finish condition

The completed `wamn-0h0g` program and final RC are the pivot's exit gate.
Promote that RC-validated tip directly to `main`; retain the displaced
pre-pivot tip at `archive/mvp` rather than merging histories.

## Work order

Hard blockers live in Beads; independent rows may run in parallel.

| Order | Work | Owners |
|---:|---|---|
| 1 | Close router invocation/driver questions; finish router, pooling and wiring substrates | `.16`, `.17`, `.18` |
| 2 | Cut over HTTP, streams and automations; land OTel before removing default durable facts | `.19`, `.24`, `.20` |
| 3 | Land the component library, data-access seam, authority partition and generated APIs | `.21`, `.22`, `.23` |
| 4 | Converge release closure and promotion after the new artifact/wiring model exists | `.25` |
| 5 | Delete the displaced flow language, guest, plans, compiler and proof mass in dependency order | `.26` |
| 6 | Regenerate governed evidence once for the final wave, run the RC, then merge | `.15.25` and the active regeneration owner |
| 12 | Receiving base + overlay POC — work order at `docs/poc/` | `wamn-10yt` |

The surviving setup, scheduler, wire, gate, management, proof and schema work
under `.1`, `.2`, `.5`, and `.7`–`.15` remains cross-cutting input to these rows;
it is not a separate architecture.

## Exploration or decision required

Do not claim the dependent implementation until its row is resolved in Beads.

| Owner | Required answer |
|---|---|
| `.15.180` | Carry the recovered conflicting run id without exposing it through anonymous HTTP, or keep separate result types. |
| `.12.151` | Keep the two release-membership conflict vocabularies or converge both tiers on one typed refusal. |
| `.13.42` | Post-MVP only: customer-hosted router residency and signed-release trust. |
