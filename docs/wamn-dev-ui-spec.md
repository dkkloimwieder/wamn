---
status: working-draft
genre: frontend design and implementation document
title: wamn-dev-ui-spec
date: 2026-08-14
scope: clients/loop-console. Local view-only dev tool over the authoring
  contract's read surface (read-draft / get-run / get-report via the
  query-only reader; loopback Vite proxy; PAT in memory). Those constraints
  are settled in review round 2 and not re-argued here — this document is
  the frontend design.
repo-basis: mvp branch @ 446cdca
---

# wamn-dev-ui-spec — loop console design & implementation

The subject is the authoring loop's exhaust: runs, test reports, draft
definitions. The audience is one author (or their agent) mid-loop,
alt-tabbing from a terminal, holding an id, wanting an answer in
seconds: *why did this run fail? which case broke? what exactly is
stored?* The console's single job is to answer those three questions
faster than reading JSON in the terminal would.

That fixes the design values: **terminal-adjacent, dense, monospace-
forward, keyboard-first, answer-first**. Not a dashboard, not a
product. It should feel like a well-typeset extension of the terminal
the author just left — and one screen should always lead with its
verdict, then let the eye descend into evidence.

## 1. Design language

### 1.1 Palette

Dark, because it sits beside a terminal and renders JSON and source
constantly. Six named values; nothing outside them:

| Token | Hex | Use |
|---|---|---|
| `ink` | `#101216` | page background |
| `panel` | `#181B21` | cards, table stripes, code blocks |
| `line` | `#2A2F38` | hairline borders, dividers |
| `text` | `#C9CED6` | body text |
| `dim` | `#7A8290` | labels, metadata, timestamps |
| `signal` | `#E8C15A` | **the one accent** — focus rings, links, the active verdict |

Status colors are *semantic, not decorative*, used only inside badges
and verdict bars, always paired with the verbatim status word:
`ok #4FA870` · `fail #C4554D` · `warn #C8933B` · `uncertain #9A6BD0`
· `neutral = dim`. Purple is reserved exclusively for
`effect-uncertain` — the one state that is neither success nor
ordinary failure gets the one color used nowhere else.

### 1.2 Type

Two faces, three roles:

- **Data face — system monospace** (`ui-monospace, SFMono-Regular,
  Menlo, Consolas, monospace`): ids, statuses, node names, JSON,
  source, table cells, the jump box. Most of every screen.
  `font-variant-numeric: tabular-nums`.
- **Frame face — system sans** (`system-ui, sans-serif`): section
  labels, explanatory sentences, empty states. It frames; it never
  carries data.

No imported fonts — no webfonts, no `@font-face`, no font files in
the bundle. The two system stacks above are the complete typography;
personality comes from scale, weight, spacing, and the strict
data/frame role split, not from typefaces.
- **Verdict role**: the data face at display size (28–32px,
  semibold) — used once per screen, for the answer.

Scale: 12 (labels, uppercase, +0.06em tracking, `dim`) · 13 (table
cells) · 14 (body) · 16 (section heads) · 28–32 (verdict). Line
height 1.5 body, 1.35 tables.

### 1.3 The signature: the verdict bar

Every entity screen opens with a full-width **verdict bar**: a 3px
status-colored rule across the top of the content column, then the
entity's answer in the verdict type role, then its identity line.

```
▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮  ← 3px, status color
FAILED · retry-exhausted at fetch-inventory
run 01J9…3F2K   draft orders@17   trace 4bf9…  ⧉
```

The bar is the whole design's thesis: the author gets the verdict
before their eyes move. Everything below it is evidence, ordered by
how often it answers the next question. No other element on any
screen competes with it — the rest of the chrome stays quiet.

### 1.4 Layout model

Two regions: a fixed **side panel** (260px, collapsible to 44px) and
the content column (`max-width: 88ch`, 24px gutters). A slim top bar
spans the viewport:

```
┌──────────────────────────────────────────────────────────────────┐
│ wamn loop · dev@localhost:8787 ●            [ id or /… jump    ] │
├────────────┬─────────────────────────────────────────────────────┤
│            │                                                     │
│  side      │   verdict bar                                       │
│  panel     │   content column (88ch)                             │
│  (tree)    │                                                     │
│            │                                                     │
└────────────┴─────────────────────────────────────────────────────┘
```

Top bar left: wordmark (frame face, 13px) · proxy target · a 7px
status dot (derived: `dim` never-contacted, `ok` last read
succeeded, `fail` last read failed — state word in tooltip and on
the start screen, never color alone). Right: the omnipresent **jump
field** (§2.6).

Vertical rhythm: 8px base grid; sections separated by 32px and a
`line` hairline with a 12px uppercase label sitting on it — the
label *is* the divider; no boxes around sections. Motion: none,
except disclosure/tree expand (120ms ease-out height) and
focus-visible rings; `prefers-reduced-motion` removes even those.

## 2. Screens

### 2.0 The side panel — routes as a tree

The panel is the console's map. Because navigation is id-addressed
(no list endpoints), the tree's entities are **what this browser has
visited** — the recents store, promoted from a flat list to the
panel's structure — plus the current entity always pinned in place
even if never stored. Three levels, never more:

```
 ◈ start
 ─────────────────────────
 RUNS                    ▾
   ✗ 01J9…3F2K   2m      ▾      ← visited runs, verdict glyph,
       failure                     relative time
       execution
       output
   ◌ 01J9…KV07   2h             ← effect-uncertain glyph
 REPORTS                 ▾
   ✗ 01J9…8Q11   9m      ▾
       cases
   ✓ 01J8…MM04   1d
 DRAFTS                  ▾
   • orders@17   1h      ▾
       source
       structure
   • orders@16   3d
 ─────────────────────────
 clear visited            ⌫
```

- **Level 1 — kinds** (`RUNS · REPORTS · DRAFTS` + `start`): fixed,
  always present, SectionLabel styling; collapse state persists.
- **Level 2 — visited entities** under their kind, most recent
  first, capped (~20 per kind): verdict glyph in its status color
  (✓ ✗ ◌ • — always with an accessible label), middle-ellipsized
  id, relative time. Click navigates; the active entity carries a
  2px `signal` left edge and `panel` background.
- **Level 3 — the entity's sections**, shown expanded only for the
  active entity: the screen's section anchors (`failure ·
  execution · output` for a run; `cases` for a report; `source ·
  structure` for a draft — `structure` dimmed with a tooltip when
  the draft doesn't parse). Click scrolls to the section; the
  section in view is marked as you scroll. Anchors, not routes —
  the URL stays the entity.
- Verdict glyphs and times are cached display text (the recents
  store), refreshed whenever the entity is visited; `clear visited`
  is the store's visible clear.
- Collapsed panel (44px): kind initials and the active entity's
  glyph only; the toggle sits in the panel footer; state persists.
- Empty state (nothing visited): the tree shows the three kind
  labels with `nothing visited yet` beneath, in frame face, `dim` —
  the panel teaches its own mechanic.

### 2.1 `#/` — Start

The only screen with no verdict — its job is to get the author to
one. Everything centers vertically in the column:

```
                    wamn loop console
              dev target: localhost:8787 → mgmt:9400
              status: last read succeeded · 12:04:31

     ┌────────────────────────────────────────────────┐
     │  ▸ paste a run, report, or draft id…           │
     └────────────────────────────────────────────────┘
        run · report · draft        (kind auto-inferred
                                     from recents; radio
                                     fallback when unknown)

```

- The jump field is focused on load; `Enter` navigates. PAT entry
  appears here only when the proxy reports 401 — a single password
  field inline under the status line, not a form.
- Visited entities live in the side panel (§2.0), not repeated here;
  the start screen stays a single calm action. First-run empty state
  (mirrored in the panel): `Nothing yet — run a draft with the CLI,
  then paste its id.`

### 2.2 `#/run/:id` — Run

Ordered by the author's actual questions: *did it work → why not →
what exactly happened, step by step → what did it produce.*

```
▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮ fail
FAILED · retry-exhausted at fetch-inventory
run 01J9…3F2K ⧉ · draft orders@17 → · trace 4bf9c1… ⧉ · captured full

RUN FAILURE ────────────────────────────────────────────
  kind        retry-exhausted
  at          fetch-inventory (http-request)
  detail      upstream 503 after 4 attempts        ▸ raw

EXECUTION ──────────────────────────────── 14 facts, all
  #  node               type          status   note
  1  ingress            request       success
  2  parse-order        transform     success
  3  check-stock        conditional   success   → in-stock
  4  fetch-inventory    http-request  error     retryable ×4
     ├ occurrence 1–4 · frame root
     ├ failure  retryable · upstream 503        ▸ detail
     └ output  — (errored)
  5  fetch-inventory    http-request  error     terminal
  …
```

- **Verdict bar**: `RunStatus` word + run-level `FailKind` and
  failing node when failed. When the status is `effect-uncertain`,
  the bar goes purple and the run-failure section is replaced by the
  **uncertain panel**: the platform's meaning verbatim, the operator
  resolution state, styled as the screen's dominant element —
  this is the one state where the console's job is to prevent a
  wrong conclusion, and the design treats it that way.
- **Execution table**: one row per returned fact, in returned order,
  `#` as the fact index. Columns: node · type · `NodeRunStatus`
  badge · a **note** column that carries the single most useful
  scrap per row (branch taken, node-level failure kind, occurrence
  count) so scanning the table alone usually answers "what
  happened." Row click (or `Enter`) discloses: occurrence/frame
  context, node failure kind + detail (FailurePanel), captured
  output (JsonView) when present. Error rows tint their left edge
  `fail`; the eye finds them without reading.
- Header count states truthfully: `14 facts, all` or
  `showing 200 of 3,412 · truncated`.
- Capture `off`: the disclosure's output slot reads
  `capture was off for this run` in frame face, `dim`.

### 2.3 `#/report/:id` — Test report

*Did the set pass → which case failed → on what expectation → take
me to its run.*

```
▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮ fail
11 / 12 PASSED
report 01J9…8Q11 ⧉ · test-set a41c…  · finalized

CASES ──────────────────────────────────────────────────
  ✓ creates-order                                   run →
  ✓ rejects-empty-cart                              run →
  ✗ backorders-when-out-of-stock                    run →
      expected  named-node-terminal
                check-stock → error
      observed  check-stock → success
  ✓ …
```

- Verdict: `N / M PASSED` (all-pass renders `ok`, any-fail `fail`);
  `pending` renders the bar `warn` with the contracted reason as the
  verdict line.
- Case rows: pass rows are one quiet line (✓ in `ok`, name, run
  link). **Failed rows auto-expand** — the author came for them —
  showing each failed assertion by family as an
  `expected / observed` pair in the data face; `terminal-respond`
  bodies render in a JsonView. Passed-case disclosure exists but
  starts closed.
- The report never shows per-node execution beyond the assertion's
  own selector line; "run →" is the handoff.

### 2.4 `#/draft/:id/:revision` — Draft

*What exactly is stored → and if it parses, show me its shape.*

```
▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮▮ neutral
DRAFT orders @ 17
saved 2026-08-14 09:12 · 4,812 bytes ·  [ source | structure ]

SOURCE ─────────────────────────────────────────────────
   1  {
   2    "nodes": [
   3      { "id": "ingress", "type": "request" },
   …                                          copy exact ⧉
```

- Two tabs, **source first and default** — the exact stored bytes in
  SourceView (line numbers, no wrapping by default, horizontal
  scroll, byte-faithful copy). This ordering is the design stating
  the contract's truth: the bytes are the draft.
- **Structure** tab only when parsing succeeds: nodes as a table
  (id · type · connection · config via disclosure JsonView), then
  edges as aligned monospace rows `from[out] ─→ to[in]`, grouped by
  source node. When parsing fails the tab is disabled with
  `does not parse — line 41: …` beside it, and the source view
  scrolls to and tints that line. A half-finished draft is a normal
  object here, not an error condition.

### 2.6 Navigation & keyboard

- `/` focuses the jump field from anywhere; `Esc` returns focus to
  the content. `Enter` on any row toggles its disclosure; `o` on a
  case row opens its run.
- `[` toggles the side panel. In the tree: `↑/↓` move, `←/→`
  collapse/expand, `Enter` activates (navigate for entities, scroll
  for sections) — the standard tree-view pattern, with
  `role="tree"` semantics and roving focus.
- Every id on every screen is a ⧉ copy affordance; hover reveals the
  full id when truncated (middle-ellipsis, first/last 4 visible).
- Back/forward are the browser's; routes are the state. Refresh is a
  single control in the section label row (`refreshed 12:04:31 ↻`),
  re-issuing the screen's one read.

## 3. Component inventory (complete)

`AppShell` (top bar · side panel · column) · `SidePanel` ·
`NavTree` / `TreeKind` / `TreeEntity` / `TreeAnchor` (three fixed
levels, `role="tree"`, roving focus, persisted collapse) ·
`JumpField` ·
`VerdictBar` · `StatusBadge` (word + color, never color alone) ·
`PassGlyph` (✓/✗ + accessible label) · `FactTable` /
`CaseList` (semantic tables, caption, disclosure rows, truncation
footer) · `Disclosure` · `FailurePanel` (kind · at · detail ·
raw via JsonView) · `UncertainPanel` · `JsonView` (collapsible,
copy) · `SourceView` (exact text, line numbers, error-line tint) ·
`KeyValue` · `CopyId` · `SectionLabel` (the rule-with-label
divider) · `RefreshControl` · `EmptyState` / `LoadingBlock` (region-
reasoned) · `ErrorPanel` (typed reader errors, refusal literals
verbatim).

One plain CSS file defining §1's tokens as custom properties;
system font stacks only (no imported fonts); no library. Accessibility floor: visible
`signal` focus rings, keyboard-operable disclosure with accessible
names, table captions, status words always present, reduced motion
respected.

## 4. Held constraints (from round 2, unchanged, compressed)

View-only (query-only reader import; no mutation affordance) ·
loopback Vite proxy, target fixed at startup, PAT in memory only ·
four screens; logs/list-endpoints/coverage/canvas/polling deferred ·
starts when the generated read results, typed error envelope, and
mounted read routes exist; execution-fact reads carry
bounded/truncated semantics · toolchain and test level per the D2
owner decision (design is toolchain-neutral; nothing here needs more
than Solid + CSS).

## 5. Acceptance

An author pastes a failing run id and, without scrolling, reads what
failed and why; expands one row and reads the node's failure and
output; jumps to the report and reads which case broke on which
expectation; opens the draft and copies the exact stored source —
and the whole thing looked and felt like one deliberate tool, not a
JSON viewer with buttons.

## 6. Implementation guide

Ordered steps, each independently completable and issue-shaped: scope,
deliverables, and its own done-check. Steps 1–8 need no platform work;
step 9 is the only integration point. Fixtures throughout are shaped
from the durable schema (run-state DDL, status/failure vocabularies,
assertion families), so the later reconciliation is against stable
structures, not invented DTOs.

### Step 0 — Toolchain decision (owner; blocks step 1 only)

Choose D2 option A (Solid 1, policy unchanged) or B (Solid 2 RC via
one explicit dependency-policy amendment: exact prerelease versions
for this package + exact internal workspace deps, nothing else).
*Done:* the decision recorded; policy change (if B) is its own commit;
`pnpm install` and the existing toolchain test green.

### Step 1 — Skeleton

Migrate the null stub to the chosen toolchain. Hash router (the four
routes, opaque-id rules: nonempty, bounded length, encode/decode).
`tokens.css` — §1's custom properties, the two system font stacks,
reduced-motion rules. AppShell: top bar (wordmark · proxy target ·
derived status dot · ⌘K hint), side-panel region (empty), content
column.
*Done:* four routes render placeholder columns inside the shell;
tokens are the only colors/fonts in the tree.

### Step 2 — Reader seam + fixtures

`AuthoringReader` interface (`runs.get / reports.get / drafts.get`,
resource-named); `FixtureReader` returning canned results: one passing
run, the §2.2 failing run (branch, ctx write, retried effectful node,
two occurrences), one effect-uncertain run, one truncated run, one
capture-off run; finalized 11/12 report + pending report; parsing and
non-parsing drafts (with CommitProvenance, incl. dirty). Typed error
values (not-found, refusal, network) constructible for ErrorPanel
work. A dev toggle selects fixture vs (later) HTTP reader.
*Done:* fixtures type-check against reader types; every later screen
state has a fixture that produces it.

### Step 3 — Display primitives

StatusBadge (word+color, unknown-safe) · PassGlyph · VerdictBar ·
SectionLabel · KeyValue · CopyId (middle-ellipsis, hover-full, copy) ·
Disclosure · DataTable (caption, disclosure slot, empty slot,
truncation footer) · JsonView (collapse depth, copy) · SourceView
(line numbers, no-wrap, error-line tint, byte-faithful copy) ·
FailurePanel · ErrorPanel · LoadingBlock/EmptyState (region-reasoned)
· RefreshControl. Pure-selector unit tests per §7's chosen level;
accessibility floor (no color-alone, focusable, captions) checked
here, once, for everything downstream.
*Done:* each primitive renders every state from step-2 fixtures.

### Step 4 — Run screen

`/runs/:id` over the reader: verdict bar; run-failure panel;
uncertain panel (purple, dominant, verbatim meaning + resolution
state); execution table (one row per fact, note column, duration
column, error-edge tint, ctx chip); the three-lane inspector as row
disclosure — DATA (input | output→port, capture-off / too-large /
errored states), CONTEXT (before | write | after via the last-wins
fold; single-cell "final only" mode until ctx capture exists), STATE
(status, attempt, frame/occurrence, timings, plan hash, effect-
attempt drill-down for effectful facts, "— pure node" otherwise).
Trace mode: expand-all/collapse-all as visible section-header
controls, compressed-inspector rules (2-line JSON, `[json]` pop,
"after unchanged"). CONTEXT and DETAILS run-level sections.
*Done:* all five run fixtures render correctly; the §2.2 wireframe
and the whole-run trace are reproduced against fixtures.

### Step 5 — Report and draft screens

Report: verdict `N/M PASSED` (pending→warn+reason), case list with
pass rows quiet and failed rows auto-expanded, per-family
expected/observed rendering, run links. Draft: title `flow · rev N`,
provenance block with dirty badge, source tab default (SourceView),
structure tab on parse success (nodes table + edges rows), disabled-
with-reason on failure, raw-bytes copy.
*Done:* both screens against their fixtures, including pending and
non-parsing cases.

### Step 6 — Side panel

NavTree, three fixed levels: kinds → visited entities (verdict glyph,
ellipsized id, relative time; grouped under flow name for drafts) →
active entity's section anchors with scroll-tracking. Visited store:
namespaced by (target, project, environment, kind), capped, display-
text cache refreshed on visit, ids only. Collapse to 44px, persisted;
`role="tree"` native semantics; clear-visited lives in the palette.
*Done:* navigation entirely mouse-driven works end to end; active
edge + anchor tracking correct; store survives reload and clears.

### Step 7 — Command palette

Ctrl/⌘-K overlay: one input, three ranked groups (go to — recents
first, typed-id detection with kind pre-ranking; this screen —
anchors + view actions; console — panel toggle, refresh, start,
clear visited). No keybinding column, no direct keys anywhere else;
palette-internal keys are list convention only (type, ↑↓, Enter,
Esc). Focus restore on close.
*Done:* every navigation and view action reachable via palette alone;
`⌘K ↵` reopens the last entity.

### Step 8 — Start screen + polish pass

Start: jump box (palette-consistent), status line, PAT field appearing
on 401 only. Sweep: empty/loading/error states everywhere reachable,
reduced-motion, focus-visible, tab order, contrast check against §1
tokens, truncation footers, copy affordances.
*Done:* the §5 acceptance journey runs end to end on fixtures.

### Step 9 — Live wire-up (blocked on platform read legs)

HTTP reader over the generated read surface + Vite proxy
(`/wamn-api/*`, 127.0.0.1, fixed target); PAT header attach from
memory; typed non-2xx decode. Reconcile fixture shapes against the
generated result types — **the diff is a deliverable**: file the
mismatches and refusal-wording friction as the read contract's
ergonomics review before it hardens.
*Done:* §5's acceptance journey against a real dev environment with
a CLI-driven draft; reconciliation findings filed.

### Step 10 — Evidence report (closes the loop)

One short findings note: enumeration friction observed (the /runs
collection case), inspector fit, ctx-capture demand, refusal wording,
toolchain verdict (if B: RC issues encountered). This is job 2 of §
"What this is" — the console exists partly to produce this document.
*Done:* findings filed; deferred-item proposals (collection reads,
ctx capture) either raised with evidence or explicitly not raised.

**Ordering constraints:** 0→1→2→3 strictly; 4, then 5–7 in any order
(6 and 7 read the visited store, 4–5 write it); 8 after 4–7; 9 when
the platform read legs exist (any time after 2 for transport work,
after 8 for acceptance); 10 after 9. Steps 1–8 run fully parallel to
MVP platform work.
