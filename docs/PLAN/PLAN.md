# PLAN — execution-model revision (exe-model rev 4)

Status: **active** (2026-08-18). Authority: **`docs/exe-model.md` (rev 4) is the only
authoritative doc**; where it conflicts with the charter (`docs/scope-reduction-mvp.md`),
the deployment amendment (`docs/deployment-simplification-spec.md`), the flow-execution
amendment, or the plane amendment, exe-model wins. This file is the ordering and decision
map only — **completion status lives in bd and git** (query `bd ready`, the `wamn-0h0g`
epic tree, label `exe-model`). Per the standing fold-back rule, durable decisions fold
into this file when they land; per-bead-close edits are not made here.

Tracker: `wamn-0h0g` (the spec names it as the continuing tracker). The 2026-08-18
reconciliation triaged all 172 open + 525 deferred beads against the spec and created
epics `.16`–`.26`; the disposition ledger is in bd (close reasons cite spec sections).

## 1 · The partition (the spec's master sentence)

> "`deploy-simplification` redirects: manifest/weld/OCI/gate/tenancy waves stand;
> flow-language waves stop."

| Epic | Verdict | Note |
|---|---|---|
| `.1` S — program setup | stands | `.1.3` backlog re-assessment now runs against exe-model |
| `.2` A — plan identity | **stopped** (rescoped) | only the execution-runtime upgrade rule (`.2.7`) survives |
| `.3` B — call-flow/frames | **closed** | no surviving leg; the walk rehost is epic `.16` |
| `.5` D — flowrunner/scheduler | rescoped | keeps scheduler/dispatcher/placement/readiness/warm-floor; guest work closed |
| `.7` F — wire contracts | rescoped | begin/wait + admission stand; flow-schema stops; contracts re-aim at wirings |
| `.8` G — publish gate | stands, **promoted** | the gate is the wiring semantic-safety carrier; latency is a product requirement |
| `.9` H — mgmt auth/schema | stands | |
| `.10` I — workspace/BoM | stands | member lists shift with the demolition |
| `.11` J — proof floor | redirected | check 7 closed, check 4 shelved; hermeticity bug family all stands |
| `.12` K — schema/auth | stands | auth confinements get *more* load-bearing under the spec |
| `.13` L — deferrals | re-adjudicated | 12 closed as moot, WAC shelved, node-SDK **graduated** to `.21` |
| `.15` M — deploy simplification | stands | plan-artifact legs rescoped onto release-manifest + component artifacts |

New epics (all `[SR-MVP]`, label `exe-model`, children carry grounded file:line anchors):

| Epic | Subject | Priority |
|---|---|---|
| `.16` N | Host router — rehost the graph walk (shared lib crate, linked by host **and** executor) | P0 |
| `.17` O | Pooled execution platform-wide (per-digest pools; reuse off; g2br.16 narrowing) | P0 |
| `.18` P | Wirings as gated tenant data (rows + pointer flip + provenance; attachment_activation is the template) | P0 |
| `.19` Q | Ingress: three paths onto the router + the emit/dedup boundary | P0 |
| `.20` R | Durability default + the premium shelf (class gate at four claim-path points) | P0 |
| `.21` T | Component palette & per-catalog library (R1; supply-chain floor; `implements`) | P0 |
| `.22` U | Developer data access — sqlx over `wamn:postgres` (sizing decision first) | P1 |
| `.23` V | Generated APIs — `entity` component + `generate crud` (plus the live RLS-claim defect, P0) | P2 |
| `.24` W | Observability — spans, per-wiring metrics, router-edge live view (must land with/before the R cut) | P0 |
| `.25` X | Release closure & the promote verb (four rules; frozen-manifest re-projection) | P1 |
| `.26` Y | Retirements — the flow-language demolition (~11k LOC + 8–9k tests, ordered) | P1 |

## 2 · Open decisions (owner calls, filed as type=decision)

| Bead | Question |
|---|---|
| `wamn-0h0g.20.1` | Durability-class fact shape: per-run column (capture_mode recipe) vs producer-derived vs env-policy tier |
| `wamn-0h0g.20.5` | Shelf semantics: freeze-in-tree vs accept edge deletion (parts already deleted, b2283d54) |
| `wamn-0h0g.22.1` | Size the sqlx seam: custom Database backend vs transport-swap under sqlx::Postgres vs WIT 0.2 widening — `query_as!` macro support is the make-or-break |
| `wamn-0h0g.18.4` | Where the in-draft cases contract attaches: wiring, component, or both |
| `wamn-0h0g.17.4` | What g2br.16 clamps once the router owns instantiation (it is a no-op for the executor today) |
| `wamn-0h0g.15.164` | Registration projection source — spec forecloses (c); (a) move relation vs (b) composing publisher |
| `wamn-0h0g.15.165` | Manifest digest vs release version: pin registrations at mint vs many-digests-per-release |
| `wamn-0h0g.15.135` | UdpBind authority at the fork pin (pre-existing, unchanged) |
| `wamn-dggp.39` | Loop-console run screen under the retiring read surface (one-screen ruling; track survives) |
| `wamn-0h0g.12.63` / `wamn-1nd` | Pre-existing: deferred 1nd transitively gates open .12.44; RLS re-key now also feeds `.22.4` |

## 3 · Sequencing

**Wave 13 (already planned) is unchanged** — the deploy-simplification hygiene head
(`.15.181` confinement gate → `.15.173` protected-writes regeneration, `.15.29`,
`.12.122/.12.123`, `.12.119`, `.11.27/.11.29`) stands under the redirect and still
precedes the `.15.25` RC.

Then the EXE waves, by dependency shape (hard edges are in bd):

1. **Decisions** — `.20.1`, `.22.1`, `.18.4`, `.17.4`, `.15.164/.165` (+ `.20.5`,
   `wamn-dggp.39`). Cheap, and they pin interfaces: the promote-verb design (`.25.3`)
   should be *written* in this window too.
2. **Substrate** — router crate + node-op WIT + walk-conformance port
   (`.16.1/.16.2/.16.6`); per-digest pool rekey + behavioral fresh-instance proof
   (`.17.1/.17.2`); observability floor re-reached (`.24.1`) and the two P0 defects
   (`.23.1` RLS claim injection, `.22.4` RLS re-key design start).
3. **Wirings** — store + flip + shape validation (`.18.1–.18.3`); authoring contract
   re-typing (`.7.4` rescoped); effect-authority re-key (`.16.8`).
4. **Cutover** — path 3 claim→router (`.19.6`), hot HTTP inline + 429 (`.19.1/.19.2`),
   streams batch/ack-after-completion + DLQ (`.19.3/.19.4`), emit boundary (`.19.7`);
   router spans (`.24.2/.24.3`).
5. **Durability cut** — class gate (`.20.2`), shelf parked (`.20.3`), queue-proof split
   (`.20.4`). OTel-as-the-record (`.24.x`) must be in place first.
6. **Gate & closure** — sequential case executor + get-report + mounts
   (`.8.5/.8.6/.7.4`), manifest re-projection (`.25.1`), component library + digests
   (`.21.1/.21.2`, `.25.2`), promote verb (`.25.3`).
7. **Developer surface** — `implements` (`.21.3`), requirement re-grain (`.21.4`),
   wamn:postgres 0.2 + prepare flow (`.22.2/.22.3`), SDK (`.13.7`, graduated),
   entity component (`.23.2`), standard nodes as palette components (`.21.5`),
   generate crud (`.23.3`).
8. **Demolition** — `.26.1–.26.7` in their recorded order (get-run → capture →
   node_runs (after the class gate) → guest/engine (after the conformance port) →
   flow-model (after extractions) → plans/compiler (after the catalog/model split and
   manifest re-projection) → test mass). Registries re-baseline once at wave end under
   the one-regeneration rule (`.15.161`).

## 4 · Spec corrections earned by grounding (input to rev 5)

Verified against the tree on 2026-08-18; each is recorded on the relevant bead:

1. **No typed WIT seam exists for node invocation** — node dispatch is an in-process
   Rust trait; `wamn:node@0.1.0` is archived and its package name is banned by two
   conformance guards. "Existing seam to reuse" → "archived design to revive" (`.16.2`).
2. **The execution path is already dead in production** — flowrunner refuses every run;
   the rehost is a greenfield wiring of unit-tested code, and the walk-conformance port
   (`.16.6`) is the only executable spec of its semantics.
3. **The pooling allocator is already on platform-wide** (512 slots, hardcoded);
   what is missing is per-digest pools — and an unwired 903-LOC instance pool exists to
   rekey. "Per-host pool sizing is a chart value" is aspiration, not fact (`.17.3`).
4. **WAMN_DISABLE_INSTANCE_POOLING cannot affect the executor** (it builds its Store
   outside the fork's InstancePolicy) — `.15.134` is rescoped and gated on `.17.4`.
5. **The queue does not survive *verbatim*** — 5 of 15 statements plus the core claim
   predicate's `EXISTS(effect_attempts)` arms are classifier machinery; the lift edits
   the hottest statement (`.20.2`), and ~45% of the file is load-bearing comment mass.
6. **"Effect authority unchanged" is false as stated** — its lookup key is
   plan/frame-shaped and the capability links only into the flowrunner; the
   binding→generation half survives, the identity re-keys (`.16.8`). Paths 1–2 have no
   run row, so the anchor must not require one.
7. **The sqlx "~200 lines" figure is unsupported** — no sqlx anywhere; frozen 0.1 WIT
   is type-lossy and prepare-less; `query_as!` macros are built-in-backend-only.
   Sizing decision `.22.1` before any commitment.
8. **`implements` is OFF in wamn's build** (fork pinned `default-features = false`;
   the conformance feature allowlist would reject it) — enable + review (`.21.3`).
9. **The publish gate's executing surface is unbuilt** — one command mounted, all
   queries 501, no sequential executor, evaluator has zero callers. It is on the
   critical path, not an integration target.
10. **Two already-ratified rulings are reversed by the spec** and are now recorded as
    such: `catalog.execution_bundles` kept (deployment ruling 2) vs plans retire
    (`.26.6` states its fate); audit-B posture (`.15.134`) vs allocator adoption.
11. **Registration projection option (c) is foreclosed** by the spec's own `.15.95`
    sentence (`.15.164` notes).
12. **pin-run / record-and-replay is vocabulary only** — nothing implements it in-tree;
    nothing retires under that name.
13. **A live correctness hole independent of the spec**: compiled RLS policies key on
    `app.role`/`app.user_id`, which production CLAIM_SQL never injects — every per-user
    policy silently denies (`.23.1`, P0).

## 5 · Reconciliation ledger (2026-08-18)

- **Created**: 11 epics (`.16`–`.26`), 60 children (8 decisions among them),
  `wamn-0h0g.13.58` (router fan-out deferral), `wamn-dggp.39` (track decision);
  29 hard dependency edges.
- **Rescoped**: 33 open beads (program root, epics `.2/.5/.7/.8/.11/.15`, and 26
  children incl. `.15.97` plan-bytes→manifest-push, `.15.134` pooling posture,
  `.15.17` registry artifact set, `.12.1/.12.6/.12.8` pins/traceparent/allowlist,
  `.5.16` readiness, `.8.5` gate executor, `.7.4` mounts) + 4 deferred rescopes
  (`wamn-0si.9`, `wamn-0h0g.15.128` — trigger fired, `wamn-ayq7.18/.19`) +
  `.13.7` graduated to `.21` @P1.
- **Closed**: 19 open (epic `.3` + `.3.7`, `.5.4/.5.5`, `.11.7`, `.15.170/.15.171`,
  12 `.13.x` call-flow deferrals) + 83 deferred (flow-language / POC / builder /
  custom-node / stored-suite / capture machinery — each close reason cites the spec
  section and, where applicable, the commit that already deleted the subject).
- **Shelved** (deferred with premium-tier notes): `.11.4`, `.11.37`, `.12.124`,
  `.13.8` (WAC fusion), plus the `wamn-0qdp` rev18 family re-annotated as
  subflow-era holds.
- **Upgraded notes**: `wamn-1nd`, `wamn-17p0`, `wamn-0j2r`, `.15.164`, `.15.165`.
- **Untouched by design**: the wave-13 set, all auth-remediation beads, the
  gate-hermeticity bug family, the event spine, `wamn-jvzx`/`wamn-b454` client
  programs (parked, owner-priority), `wamn-dggp` implementation beads (one-screen
  decision filed instead), and `.12.5` (in_progress, owner-held).
