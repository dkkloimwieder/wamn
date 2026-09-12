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
| 5 | Delete the displaced flow language, guest, plans, compiler and tests in dependency order | `.26` |
| 6 | Regenerate governed evidence once for the final wave, run the RC, then merge | `.15.25` and the active regeneration owner |
| 12 | Receiving base + overlay POC — work order at `docs/poc/` | `wamn-10yt` |
| 13 | Repository consolidation — charter at `docs/history/consolidation-findings.md` | `wamn-47wm` |

The surviving setup, scheduler, wire, gate, management, test and schema work
under `.1`, `.2`, `.5`, and `.7`–`.15` remains cross-cutting input to these rows;
it is not a separate architecture.

HTTP transport reuse continues independently under `wamn-ctc8.16`, the native-alignment plan's D work.
The owner retains WAMN's pinned-address transport with bounded reuse and excludes an upstream patch.
Serialize its shared host, executor, and driver edits with native-dispatch work.
Its [report](../../evidence/perf/2026.09/ctc8-16-http-reuse/README.md) records correctness evidence without a performance claim.
The origin/executor authorization correction under `wamn-ctc8.33` follows pooling immediately.

The reviewed [native C source checkpoint](../../evidence/perf/2026.09/native-c-advisories/source-checkpoint-001/handoff.json) is owned by `wamn-0ct2.7`.
It replaces custom materializer delivery and settlement with the platform-owned native `events` binding and scoped broker credentials.
Registration checks, drift refusal, derived publishing, and the separate scheduler doorbell remain.
The [native delivery and retention test](../../evidence/perf/2026.09/native-c-advisories/scoped-native-007/README.md) at `b0a3f045` passed after observed connection closure and redelivery of the same unacknowledged sequence.
With 65 payloads of 1,047,552 bytes, pulls stayed below 4,194,304 bytes and stopped at 64 pending acknowledgements.
Delivery resumed after acknowledgement, and real source expiry left the termination advisory readable with its source payload unavailable.
Application correctness and the integrated retained workspace test run remain pending under `wamn-0ct2.7`.
Coordinate its source integration with the application moves under `wamn-47wm` before building final artifacts and updating their digest pins.
The owner requires separate source and advisory streams for each organization, project, and environment.
Observers use that environment's credentials and cannot read another environment.
Provisioning creates the declared streams and consumers.
Activation compares their stored configuration, including stream replicas and duplicate window, and refuses disagreement without changing broker objects.
Runtime credentials cannot create, change, or delete those objects.
The stream replica count remains separate from the workload replica count.

## Exploration or decision required

Do not claim the dependent implementation until its row is resolved in Beads.

| Owner | Required answer |
|---|---|
| `.15.180` | Carry the recovered conflicting run id without exposing it through anonymous HTTP, or keep separate result types. |
| `.12.151` | Keep the two release-membership conflict vocabularies or converge both tiers on one typed refusal. |
| `.13.42` | Post-MVP only: customer-hosted router residency and signed-release trust. |
