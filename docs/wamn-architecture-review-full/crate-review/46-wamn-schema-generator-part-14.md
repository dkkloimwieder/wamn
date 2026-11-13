# 46 wamn-schema-generator, part 14

Scope: the client screen plan (`client_plan.rs`). Read under tab 45 (the IR is in-process and built from typed contracts). Nothing built or run.

## Files read

- `src/client_plan.rs`: header, constants, `Role`, `NoRole`, `idempotency_field`, `route_canonicalization`, `role`; the type and function list of the rest.

## Callers read

- `control/release/src/publish_release/package_sources.rs::read_input_identity` (calls `idempotency_field`, `route_canonicalization`).
- Every caller of `ClientPlan`, `unsupported`, `unserved`, `unresolved`, `unsatisfied_narrowings`, `unsearchable`, `takes_many`, `effective_result_fields`, `filter_input` (grep).

## One purpose per file

- `client_plan.rs`: decide which operations get a screen and in what role (table, detail, form, delete), which inputs the platform fills, and how screens link, for the client emitters.

## Findings

**1. Two server rules live in the client plan.**

Fact: `route_canonicalization` (how the host canonicalizes a request element before it hashes it: numeric and timestamp spellings, excluded fields, line order) and `idempotency_field` are in `client_plan.rs`. Release publish calls them on the raw input contract JSON (`read_input_identity`) and writes the result into the route. Neither is a client concern. Tab 37 finding 3 A already deletes `idempotency_field`.

Options:

- A: the build writes the route's canonicalization into the final route document (tab 35 finding 1 A), from the typed manifest and contract. Delete `route_canonicalization` and `read_input_identity`; release reads the route document.

**2. The plan has one reader.**

Fact: only `client_component.rs` (`emit_ts_components`, the wfe screen descriptions) reads `ClientPlan`. The TypeScript client reads the IR directly. The module header says "every client emitter reads" it and names terminal and native emitters; no other emitter exists in the repository (grep).

Options:

- A: merge the plan into the component emitter. One module decides screens and writes their descriptions; the claim of a UI-neutral layer goes.
- B: keep the plan as its own module (what screens exist, separate from the wfe shape) and correct its header to the one reader.
- Recommend B: the split is a real one (meaning against presentation), and merging deletes no logic.

**3. Plan diagnostics with no product caller.**

Fact: `ClientPlan::unresolved` and `ClientPlan::unsearchable` (with `UnresolvedColumn` and `UnsearchableSelector`, used only there) have only test callers. `unsupported`, `unserved` and `unsatisfied_narrowings` are used by the component emitter.

Options:

- A: delete the two methods and their two types.

## Follows from earlier rulings (no new ruling needed)

- Tab 45 finding 2 A (typed contracts): the IR carries the operation type, result class and transaction as strings, and the plan matches strings (`role`, `PRIVATE_KIND`, `PER_INPUT_TRANSACTION`). They become the catalog enums (`OperationType` exists).

## Tests deleted, changed or moved

- Finding 1: release tests of `read_input_identity` (`a_write_route_names_its_idempotency_field`) read the route document; plan tests of `route_canonicalization` move to the build.
- Finding 3: tests of `unresolved` and `unsearchable` go.

## Not read

- `client_plan.rs` bodies of `from_ir`, `page_inputs`, `row_links`, `row_forms`, `Lister`, `populated_inputs` (structure only).

## Rulings

- Finding 2: B (owner, 2026-11-11). The plan stays its own module; its header is corrected to its one reader (the wfe component emitter), and the claim of terminal and native emitters goes.
- Findings 1, 3: A (owner, 2026-11-11).
