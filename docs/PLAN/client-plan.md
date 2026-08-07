# Work order — authoring loop POC: auth, write path, client track

Living order for the POC effort; updated in place as state changes.
Status as of 2026-08-07 morning (`4d5b995`): the overnight platform sweep
landed roughly half of Part C — **and none of the client track**. The
platform is dogfooding its own surface (scenario-worker carries a live
authoring-loop test); the client side has zero commits. The wave's exit
gate is unchanged and is delivered by S1.

**Critical path now:** auth A1 for live round-trips; three client items
(CT1–CT3 below) start immediately with no auth dependency.

## A. Decisions — status after the overnight sweep

Landed (no ratification needed): **1** and **4** — `wamn-ftfc.1` settled the
transport-neutral application model with optimistic lifecycle transition in
the handlers and Git/CI/API/CLI as principal-supplying adapters (`36d918d`);
**6** partially — `ctc8.2` settled the disposition-authority table.
Note on **5**: `wamn-ftfc.6` settled the *workspace-scope* axis (flow-draft
only, one applied catalog) — **not** the per-principal collaboration axis,
which remains open below.

Still for ratification:

1. ~~Commands first~~ — **landed** (`36d918d`), superior formulation.
2. **Per-project directionality.** A project is git-led (repo is authoring
   truth; platform ingests) or studio-led (platform state is truth; git is
   export at most). Bidirectional simultaneous write is explicitly out.
3. **Full auth now (item 5 pulled forward, decided).** First-party identity
   core, platform-plane, in sysdb: principals (human and **service** kinds),
   argon2 verification, role claim. Two presenters over one core: PATs
   (CLI/agents, hashed at rest) and cookie sessions (SPA; HttpOnly,
   SameSite, W16 shape). OIDC/SSO enters later as an additional issuer
   against the same session seam — federation, not rewrite. The no-auth /
   shared-token stopgaps are ruled out.
4. ~~Optimistic concurrency~~ — **landed**, absorbed into ftfc.1's
   handler semantics ("optimistic lifecycle transition").
5. **Per-principal drafts, shared visibility** *(the axis ftfc.6 left
   open)*. The agent's
   draft and the human's draft of one flow coexist as separate revisioned
   work-in-progress; editing another principal's work is a successor
   revision under your own principal, lineage recorded. No locks; an
   optional soft assignee marker later.
6. **Agent = client with a principal.** No agent-specific platform
   machinery. Platform surfaces work products and executions, never agent
   cognition; an external session-log link may ride as command metadata.
   Standing workflow via roles: agents draft-and-run, humans promote
   (12g gates + the ctc8.2 verb-holder reframe).

## B. The ladder

**S0 — identity core + scripted console.**
Deliver: sysdb principal store + roles; PAT mint/verify (`wamn login`);
one auth middleware on the management surface; service principals; audit
carries principal on every command. Console: checked-in hurl/`.http`
request collection + smoke script, versioned beside the API — the fastest
API-testing surface, runnable by human, agent, and CI identically.
Accept: authenticated round-trip against the first landed endpoint;
rejected without token; two principals distinguishable in audit.

**S1 — CLI verbs = reference client = MVP.**
Deliver: `wamn validate | draft-run | suite-run | promote | runs` speaking
HTTP. Edit medium: **working-tree files in a checkout** — the CLI submits
file content through the API; no server-side git machinery. The simplest
full cycle is the acceptance: change one literal in the simplest fixture
flow → submit draft → draft-run → verdict → stored suite → promote → one
gateway call proves the deployed change. Latency of edit→run displayed.
This is the wave exit gate, demonstrated headless.

**S2 — Loop Console SPA.**
Deliver: minimal Solid 2 SPA (D2 stack, **no table/form adapters** — plain
rendering) on stage-B sessions: activity feed of drafts/runs/suites with
principal filtering (a query parameter on existing projections), draft
diff view against the applied baseline, trigger buttons for draft-run and
suite-run, verdicts/refusals rendered, exit-gate latency on screen.
Accept: a human watches an agent's run stream and opens its draft diff,
polling only.

**S3 — studio spec Stage 1 onward.** Screen derivation, WAMN-owned
adapters, registry — starts only after S1–S2 have dogfooded the surface
per D8's unstable-until-dogfooded rule. Unchanged otherwise.

## C0. Client track — starts now, no auth dependency

- **CT1** *(p1)* — Frontend workspace scaffold: Solid 2 + Vite per spec D2,
  pnpm with the D10 supply-chain gate live from the first lockfile. No
  adapters, no screens — the workspace and its gates only.
- **CT2** *(p1)* — Typed client from `docs/contracts/authoring-surface.schema.json`
  (`ftfc.13`, 1,562 lines): generated request/result types + a thin fetch
  client. Schema-first; live calls arrive with A1.
- **CT3** *(p1)* — Request collection (hurl/`.http`) drafted schema-first
  against the same contract, versioned beside it; smoke script wired to
  run authenticated the day A1 lands.
- **CT4** *(p2, after A1)* — S1 CLI verbs over CT2's client.
- **CT5** *(p2, after A2)* — S2 Loop Console per the ladder.

## C. Work items — platform side, with status

**Auth epic.**
- A1 *(p1, blocks S0)* — identity core + PAT + middleware + service
  principals, per decision 3. Session seam shaped for the later cookie
  presenter and the OIDC issuer.
- A2 *(p2, blocks S2)* — cookie sessions, login/logout on reserved routes,
  CSRF per W16.
- A3 *(docs)* — record the item-5 decision (build thin first-party;
  federate later) in PLAN; close the spec's O8 IdP entry against it.

**Write path.**
- ~~ftfc.1~~ — **settled** (`36d918d`), conflict semantics included; the
  2A/6A seam resolved in the same fold (6A builds ABI-facing).
- ftfc.2 *(re-scope, open)*: MVP form is CLI-reads-files→API submission; the
  async git sync job (webhook/poller → commands; verdicts as commit
  checks; never a blocking pre-receive) is the follow-on bead, filed
  separately and demand-gated on a git-led project existing.
- New *(decision bead, owner-held, open)* — per-project directionality
  flag and its enforcement surface (decision 2).
- ~~ftfc.6~~ — **settled** on the workspace-scope axis (`1446ede`); the
  per-principal axis is decision 5 above.

**Landed overnight, platform side:** ftfc.4/.5/.7/.12 decision sweep;
ftfc.11 stored-suite execution; ftfc.13 public authoring contract +
`wamn-authoring-model` + live loop test in scenario-worker; ko5r.14 retired
the provider candidate; 4tob.10 scoped error conventions; 4q3c.12 closed
the egress gap with NetworkPolicy + escape-proof mutants; jole per-root
isolation; the 4u7p.42.x disposition family is in flight.

**Projections & visibility.**
- ma5 *(as re-scoped, open)* — suite-result projection keyed to stable node/edge
  identity; the S2 console is its second renderer after the CLI.
- ftfc.7 — run visibility; add principal attribution to the projection if
  absent.
- New — **draft-diff projection**: agent-or-human draft vs applied
  baseline, addressed by stable IDs (the one genuinely new deliverable in
  the collaboration story).
- One sentence in PLAN, not a bead: SSE live-query spine (platform plan
  6.5) is evaluated as the feed's push transport when polling proves
  laggy; nothing header-independent is built before that evaluation.

**Studio-spec deltas (spec is v0.15; apply as v0.16).**
- Insert the S2 console as the spec's Stage 0; renumber; Stage 1 gains the
  "starts after dogfood" gate.
- New working position: dev auth rides the identity core above (replaces
  the unstated Stage-2 "session against dev").
- Add the **minimum management surface table** (per stage: needed vs
  exists/landing/absent). Initial S1 row: submit-draft, draft-run,
  run/suite read, suite-run, promote, auth endpoints — with `ftfc.11`'s
  stored-suite execution marked landed.
- W2 fix: digest preimage is always canonical stable-ID ordering;
  order-as-semantics is an explicit `order` field, never sequence
  position.
- File the five fold-back beads the §9 register proposed and left inert:
  the D10 layout-hedge contradiction (PLAN:2509 vs platform-plan:213),
  `Field.id` immutability sentence, changeset-machinery ownership item,
  float palette, plus this document's directionality decision landing in
  the same PLAN pass.

## D. Non-goals and guardrails

No bidirectional git sync. No blocking pre-receive hooks. No lock/lease
machinery for drafts. No agent-specific platform features — branch envs
(W15) remain the later, stronger isolation, not a prerequisite. No
platform storage of agent cognition. No table/form adapters before S3. No
OIDC buildout now — only the seam. Port-forward no-auth explicitly ruled
out. Spec Stage 1 does not start on optimism.
