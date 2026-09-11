# WAMN repository consolidation — findings

**Status:** rev 2 · 2026-09-11 · external review applied · measured at
`79879412` · this document lists defects in the repository's *shape*, not its
behavior. The application proofs pass; **the retained workspace sweep does
not** — that is the baseline, recorded as such. Numbers are measured.

## 0. Common cause

Every wave optimized for the provable correctness of one change and never for
the legibility of the whole. Each ruling added a file, a location, a document,
a bead, or a law entry to whatever it found; nothing was ever split, merged,
derived, or deleted for shape. Seven consequences follow.

## 1. Proofs are opaque shell

| Measured | Consequence |
|---|---|
| `tools/receiving-cluster-journey-run`: 3,683 lines; `wms-cluster-journey-run`: 2,743; `rc-gate-run`: 663; `agent-pilot-run`/`-grade`: 2,062 | The gates of record are bash. |
| Receiving journey: ~110 assertion lines among ~117 `kubectl/docker/helm/curl` lines and ~3,400 of plumbing | A reader cannot find what is proven without reading the whole script. |
| Eight "lifted harnesses" made plumbing reusable | They did not make any proof legible. |

**Defect:** correctness was ruled per journey; form never was. A proof whose
assertions can't be read is a claim.

## 2. An application has no home

Receiving exists in **seven locations under three naming grammars**:

```text
packages/receiving/                          snake, manifest + SQL + generated/
packages/receiving/generated/receiving-tui   a generated TUI
components/application/receiving             kebab, the app guest
components/data/receiving-data               kebab + "-data" suffix, the data guest
crates/client/receiving  (bin wamn-receiving) a second, hand-written TUI
tests/integration/tests/receiving_history    its tests
packages/client_acme_receiving/…             the overlay, following none of the above
```

**Defect:** no directory can be opened and read as "the Receiving app"; the
two TUIs exist because a parity plan was ruled and the hand-written one never
retired.

## 3. Files nobody can hold

| File | Lines |
|---|---:|
| `crates/schema/control/src/run_plane.rs` | 8,292 |
| `services/ctl/tests/run_plane_live.rs` | 5,873 |
| `services/ctl/src/provision_project_env.rs` | 5,436 |
| `crates/platform/runtime/src/plugins/wamn_postgres/claims.rs` | 4,594 |
| `crates/schema/generator/src/generate.rs` | 4,530 |
| `crates/execution/host/src/router_driver.rs` | 3,981 |

256k lines of Rust across 107 crates for two applications. The largest files
are the ones every wave touches — which is why "contended file" became a
protocol instead of a smell.

## 4. The gate document is a transcript

`docs/operations/build-and-test.md`: **5,121 lines**. It has headings; its
defect is scope and accumulated history. It is the recipe book, the gate registry, the law book, the
standing-check list, and the lane-fence registry at once. Laws ruled into it
are prose paragraphs nobody reads before doing the thing they forbid.

## 5. Hand-maintained inventories

Ten sites — workspace member lists, tier and role JSON, conformance counts, the
feature allowlist — that a new crate updates by hand, guarded by tests that go
red until every site is found. This is the `.10.39` wall the pilot hit,
retained for the platform's own crates.

## 6. The tracker has no signal

**639 open, in-progress, blocked, or deferred beads.** Every finding became a
bead, every deferral a trigger bead, every ruling a note. The dozen items that
matter are indistinguishable from six hundred of filed residue.

## 7. Documents rewrite each other

**166 markdown files**: twelve "specs", 82 perf reports in one flat directory,
a ledger, a review, a plan, a protocol, a cutover charter — several superseding
one another (TUI spec over slice v; alignment plan over the review; cutover
over both). Probe crates with `Cargo.lock`s live under `tools/`. **No single
document says what WAMN is today.**

## 8. Direction — the consolidation wave

**Objective: delete unnecessary machinery, not organize it.** Nothing below
changes runtime behavior. No governance framework is added.

1. **Delete the inventories; do not derive them.** Cargo owns packages,
   dependencies, features, and targets; application manifests own application
   declarations. Package-role classifications, tier inventories, copied
   counts, and the tests that keep them synchronized are removed. Consumers
   read the authoritative declarations. Dependency changes are reviewed in
   their manifests, not through a second approval model. Tests of product
   behavior and security stay; bookkeeping tests go.
2. **One home per application, without flattening it.** `apps/<package>/`
   holds application-owned source, SQL, generated output, UI composition, and
   tests; shared infrastructure stays outside; base and overlay packages stay
   distinct; one directory may contain several crates and components. **One
   operator application per app, composed from generated screens plus
   application-owned Rust** — the current Receiving crate is that composition,
   not a duplicate. One name, the package id, everywhere.
3. **Readable tests by extraction, not rewrite.** Separate scenario steps and
   meaningful assertions from provisioning, processes, credentials, waiting,
   and cleanup; reuse the Rust assertions already written; keep shell
   entrypoints thin. Local and live commands are straightforward; a missing
   live prerequisite is reported explicitly; a skipped test never counts as
   executed proof.
4. **Split by responsibility; delete the obsolete.** File length is a warning
   sign, not a criterion. Reduce shared mutable state, unrelated
   responsibilities, and over-broad interfaces; prefer private modules to new
   crates. Sequence against the approved native-alignment deletions so nothing
   about to disappear is reorganized first. Mechanical moves, internal
   refactors, and behavior changes land as separately reviewable commits.
5. **Documents and tracker without replacement registries.** One current
   architecture overview; short, runnable build/test/operation instructions;
   one maintained home per essential contract; obsolete plans and incident
   narration become history; maintained tools move out of evidence
   directories. Beads triaged by relevance and actual disposition — no numeric
   target; known unresolved defects stay visible; deferred ideas stop
   dominating the active list.
6. **Baseline first, verify last.** Record the actual sweep result as the
   starting state; preserve build isolation and application behavior through
   relocation; run the resulting commands on the integrated tree; classified
   failures remain failures.

**Success:** a developer can locate an application, understand its
dependencies, change it, and run its tests without updating unrelated
inventories or reconstructing a chain of historical rulings.


## 9. Owner directive, 2026-09-11

This document is the charter for the consolidation wave and the record of why the tree changes shape.
Beads owns the implementation plan under `wamn-47wm` (`PLAN-13`).
The [starting baseline](../perf/2026.09/consolidation-baseline/report.md) belongs to `wamn-47wm.1`.
The measurements above remain the dated review at `79879412`.
The starting sweep measures the integrated tree at `a0e833b9398c829030a055f5c38755aebd98d358`.

The owner sets these execution conditions:

1. Land this charter and the actual starting sweep before consolidation changes.
   List every failure and its classification in the retained evidence.
2. Follow the six directions in their stated order.
   End each step with the retained sweep on the integrated tree and record its comparison with the starting baseline.
3. Keep `router_driver.rs`, the manual linker, the component cache, and `wamn_jetstream.rs` in place until native B and C land their deletions.
   Native B is integrated under `wamn-0ct2.2`.
   Native C remains with `wamn-0ct2.7`.
   Consolidate other owners around those files.
4. Keep each commit to one kind of change: a mechanical move, an internal refactor, or a behavior change.
   Use `git mv` for moves.
   Keep each commit reviewable alone.
5. Add no registry, framework, governance, or tracker-only task.
   If a step needs a decision, ask the owner.
   If it needs new machinery, stop.
6. Demonstrate the exit through a developer walkthrough.
   Locate Receiving, change one application data field, and run its tests without touching an unrelated file.

Application directories use their exact package IDs: `apps/wamn_receiving/`, `apps/wamn_wms/`, and `apps/client_acme_receiving/`.
Keep crate names when renaming them changes generated output, digests, component names, or operation tokens.
A suffix such as `-data` or `-tui` stays only when it names a role inside the application.
Each application README contains one line that maps its directory to its crates and components.
A rename that preserves artifacts and contracts can land in a separate mechanical commit when it improves navigation.

Use the Simple English skill for necessary renames.
Use the same simple terms in comments and identifiers.
If a term has more than one possible meaning, ask the owner.
This instruction does not authorize a contract rename.

Retire `m1` and update its callers.
Do not move its component list into Cargo `default-members` or another profile.
Application builds read component declarations from the named app manifests.
Proof builds select all guest workspace members from Cargo.
Both paths keep one Cargo invocation per guest to preserve digests.

The materializer is platform infrastructure.
Native C uses a platform-owned named NATS binding for it.
Credentials apply to one environment and permit attachment only to its allowed streams.
The binding credentials enforce authority.

The composed Receiving operator is the application and owns its `main`.
Retire the standalone launcher in a separate mechanical commit.
Keep the generated library and update the consumers that select the retired launcher.

Run the Receiving field walkthrough on a disposable branch.
Keep its field changes inside `apps/wamn_receiving/`.
Land only the transcript as the exit evidence, with its field diff, executed tests, and comparison with the baseline.
The integrated branch retains the original field.

When the current architecture overview exists, move this charter to `docs/history/` with the specifications that it retires.
Keep the historical evidence and the reasons for the change.
Beads and git continue to own completion status.
