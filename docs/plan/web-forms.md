# Web forms

Updated through: 2026-10-01, `main` at `4e5d5f2bd`. Phase 1 of the generated web forms review. Bead `wamn-j856`.

Implementation touches `web/runtime`, `web/ui`, the form emitter in `crates/schema/generator` and `crates/client/tui/tests`. D names the owner at acceptance.

## 1. Goal

A generated browser form submits one intent the way the terminal client does. A pending submission blocks a second one. The form captures its supplied values once per intent. A retry is offered only where the served route permits it, and it sends the captured request byte for byte. The browser gets the reducer that the terminal client has today (`crates/client/tui/src/submission.rs`), with the same states, the same transitions and the same test table.

Phase 1 also makes typed values correct at the form boundary. A numeric input becomes a number. An optional input can return to absent, and a nullable input can be set to null. A value that a row fills into a form keeps its contract type through the form address and a reload.

Phase 1 adds no form architecture. The generated forms keep their present shape. Phases 2 to 5 are named in §6 with their goal and boundary, and each is scoped after its predecessor closes.

## 2. Fixed rules

- The submission rules are the ones in `docs/architecture/execution.md:654-670`. This spec adds no submission rule. The web reducer implements those rules, and it is not a new design.
- One test table states the reducer. `crates/client/tui/tests/data/submission-cases.json` holds every case. The terminal reducer and the web reducer read the same file, as both clients read `classification-cases.json` today. A case that one client reads differently is a defect in one of them.
- An intent is one call. A form submits one item, and a bulk form submits N items in one `callEach`. Each is one intent, and uncertainty belongs to the whole intent (`execution.md:661`).
- Supplied values (`request_id`, `idempotency_key`, `occurred_at`) are written once, when the intent begins. A retry sends the captured items unchanged, with their supplied values and their expected revisions (`execution.md:663`). Only the transport derives the authorization header again (`execution.md:664`).
- A retry is offered only when the state is `Uncertain` and the route serves `replay: "claim"` (`execution.md:665`, `wire.ts:37`). With `state` or no replay fact, the form offers a refresh and no retry.
- The generated request field map carries the contract type of each leaf. That one typed map serves the prefill decoder and the scalar codec of the numeric inputs. The zod schema stays the validator, and nothing reads types back out of it.
- An empty string is never a surrogate for null. A control yields absent, null or a value, and the three are distinct.
- An input that the operator never touched stays absent, as it does today (`client_component.rs:1670-1681`).
- No author-facing configuration is added. No manifest field names a control, a presence state or a prefill spelling.

## 3. Current state

Measured on `main` at `01ca2425d` on 2026-10-01. The commits from there to `4e5d5f2bd` change only `docs/plan/gcp-deployment.md` and `docs/plan/release-qualification.md`.

First task: list every served form with an operator-typed `int32` or `float64` input. The served web applications are `apps/wamn_receiving` and `apps/wamn_wms`. Their generated components hold 12 forms and no other form.

| Form | File | Operator-typed `int32` or `float64` input |
| --- | --- | --- |
| `PurchaseOrderUpdateForm` | `apps/wamn_receiving/generated/client-ts/components/purchase_order.tsx:231` | none |
| `ReceivingRecordReceiptForm` | `apps/wamn_receiving/generated/client-ts/components/receiving.tsx:259` | none |
| `SupplierCreateForm` | `apps/wamn_receiving/generated/client-ts/components/supplier.tsx:78` | none |
| `InventoryAdjustForm` | `apps/wamn_wms/generated/client-ts/components/inventory.tsx:159` | none |
| `InventoryMergeForm` | `apps/wamn_wms/generated/client-ts/components/inventory.tsx:501` | none |
| `InventoryMoveForm` | `apps/wamn_wms/generated/client-ts/components/inventory.tsx:751` | none |
| `InventorySplitForm` | `apps/wamn_wms/generated/client-ts/components/inventory.tsx:1011` | none |
| `LocationCreateForm` | `apps/wamn_wms/generated/client-ts/components/location.tsx:94` | none |
| `LocationUpdateForm` | `apps/wamn_wms/generated/client-ts/components/location.tsx:314` | none |
| `PackagingCreateForm` | `apps/wamn_wms/generated/client-ts/components/packaging.tsx:110` | none |
| `ProductCreateForm` | `apps/wamn_wms/generated/client-ts/components/product.tsx:91` | none |
| `ProductUpdateForm` | `apps/wamn_wms/generated/client-ts/components/product.tsx:308` | none |

No generated component contains `z.number()` or `type="number"`. Every `int32` request member of a served form is a revision input, for example `expectedRowVersion`, which the form writes from the chosen row (`inventory.tsx:798`, `:832`). The input schemas omit revision and supplied inputs. So the numeric defect is P1, and phase 1 fixes it with its test.

| Place | Today |
| --- | --- |
| Terminal reducer | `Submission` owns `Editable`, `Pending`, `Succeeded`, `Refused`, `PartiallyCompleted` and `Uncertain` (`crates/client/tui/src/submission.rs:75-91`). `begin` captures the whole `BuiltRequest`, including `request_id` (`:157-171`). `retry` requires `Replay::Claim` and `Uncertain` and reuses the capture (`:174-184`). `resolve_evidence` keeps an earlier uncertainty through a later refusal (`:250-256`). `invalidate` and `activate` reset on a target replacement (`:283-303`). |
| Terminal tests | `crates/client/tui/tests/submission.rs`, 1080 lines of test functions, not a table. The classification cases are already one shared table: `tests/data/classification-cases.json`, read by `classification_table.rs:56` and `web/runtime/test/classification.test.ts:12`. |
| Web runtime | `Outcome` has `completed`, `partiallyCompleted`, `refused` and `uncertain` (`web/runtime/src/wire.ts:81-99`). `ResponseContract.replay` is `"claim" \| "state" \| null` (`wire.ts:37`). There is no reducer. `supplied.ts:13-18` says that every attempt gets a new request id. |
| Form submit | The emitted `submit()` builds the item from `initial` and the held values (`client_component.rs:1673-1760`). It writes fresh supplied values on every call through `write_supplied` (`:1704`, `:1884-1897`). No state blocks a second call, and the submit button has no disabled state (`:1872`). |
| Bulk submit | `emit_many_submit` builds one item per row with fresh supplied values and sends all of them in one `callEach` (`:1921-1977`). The per-item outcomes go to `props.onEach`. |
| Delete submit | The delete screen writes fresh supplied values and calls once (`:2560-2590`). No state blocks a second confirm. |
| Numeric input | The schema states `z.number()` for `int32` and `float64` (`:1222`). The control is a `TextField` with `type="number"` whose `onInput` stores the text (`:2425-2434`). The table inline editor converts the text with `Number` (`web/ui/src/table/edit-cell.tsx:78-83`). The other numeric types travel as text and have a text pattern (`:1205-1212`). |
| Presence | The submit holds a member only when its signal is not `undefined` (`:1677-1681`), so an untouched input stays absent. A control that the operator changed cannot return to absent. No control sets null. `ChoiceField` sends `""` for its empty choice (`web/ui/src/fields.tsx:91`, `:96`, `:105`), and `allowEmpty` is `!required \|\| nullable` (`client_component.rs:2399`). `CheckField` has true and false only (`fields.tsx:60-64`). |
| Served presence cases | The three update forms each have one optional, non-nullable change member: `change.supplierId` (`purchase_order.tsx:200`), `change.locationCode` (`location.tsx:283`) and `change.productCode` (`product.tsx:277`). No served form has a nullable operator input. |
| Request field map | `FieldMap` maps a wire key to a member name or a nested map, and carries no type (`wire.ts:153-161`). For example `INVENTORY_MOVE_REQUEST_FIELDS` names five members of `value` (`apps/wamn_wms/generated/client-ts/inventory.ts:331-343`). The types appear only in doc comments. The emitter is `crates/schema/generator/src/client_ts.rs:449`, `:615-648`. |
| Prefill | `fillPath` writes each leaf with `String(value)` (`web/shell/src/fill.ts:16`). A null leaf becomes the text `null`, because the object branch excludes null. `filledValues` returns every leaf as text and casts the result to `T` (`fill.ts:25-42`). WMS fills three forms this way (`apps/wamn_wms/web/src/routes.tsx:71-73`, `:85`). |
| Tests | Generated component tests are in `web/components/test` (26 files, including `prefill.test.tsx`, `update.test.tsx` and `delete.test.tsx`). The prefill helper test is `web/shell/test/fill.test.ts`. |

## 4. Design

### 4.1 The shared submission table

`crates/client/tui/tests/data/submission-cases.json` holds the reducer cases. Each case is a start state, a sequence of steps and the expected state after each step. A step is `begin`, `retry`, `resolve` with an outcome, `newCommand`, `invalidate` or `activate`. An outcome uses the classification spelling that both clients already share.

`crates/client/tui/tests/submission.rs` becomes table-driven over that file. Each present test function becomes one or more cases. A behavior that is not a reducer transition, such as a response classification, stays where it is or moves to `classification-cases.json`. The terminal reducer does not change. The table states what it does today.

### 4.2 The web reducer

`web/runtime/src/submission.ts` implements the same states and the same transitions. It is framework-independent, as the rest of `web/runtime` is. It holds the captured items, the replay fact of the route, the attempt counter and the earlier uncertainty. `web/runtime/test/submission.test.ts` reads `submission-cases.json` and runs every case.

For a bulk call, the reducer folds the per-item outcomes into one state. All items completed gives `Succeeded`. All items refused gives `Refused`. Some completed and the rest refused gives `PartiallyCompleted`. Any uncertain item gives `Uncertain`, because uncertainty belongs to the whole intent. A retry sends all items again, byte for byte, together.

`supplied.ts` keeps its three value functions. Its comment changes: a value is written once per intent, and a retry keeps it.

### 4.3 The form emitter

Each form, bulk form and delete screen calls the reducer, and it no longer builds its own lifecycle.

- `submit()` validates, writes the supplied values, applies the revision and calls `begin`. A refused or blocked `begin` leaves the form as it is.
- The submit button is disabled while the state is `Pending`. The delete confirm is disabled in the same state.
- In `Uncertain`, the form shows the reason. It offers a retry when the route serves `replay: "claim"`, and otherwise a refresh. The retry calls `retry` and sends the captured items.
- `Succeeded` and `PartiallyCompleted` spend the intent. A new command starts from `newCommand`, as in the terminal client.
- The refusal marks and the announcements stay as they are.

### 4.4 Typed request field map and the scalar codec

`client_ts.rs` writes each leaf entry of a request field map with its contract type, for example `"packaging_id": { member: "packagingId", type: "uuid" }`. A nested entry keeps `member` and `fields`. The result field maps do not change. `toWire` and `fromWire` read the member name from either form.

`web/runtime/src/scalar.ts` converts control text to a typed value with the field's type. `int32` admits an optional sign and digits, within the 32-bit range. `float64` admits a finite JavaScript number. Any other text type passes the text through. The codec returns a value or an error text. The generated numeric control and `edit-cell.tsx` both use it, so the inline editor has no second conversion.

### 4.5 Absent, null and value

A control states one of three things: absent, null or a value. `web/ui` gains one presence action beside a field. An optional input gets "leave unchanged", which returns it to absent. A nullable input gets "set empty", which sets null. An input that is both gets both. The emitter adds the action from `required` and `nullable` of the input, and the submit holds null as a member.

`ChoiceField` yields absent or null for its empty choice, never `""`. `CheckField` for an optional or nullable boolean gets the same presence action. A required text input still sends `""` when the operator clears it, because an empty string is a value of a text type.

### 4.6 Typed prefill

`fillPath` writes a leaf by its type from the typed request field map. Text types are written as text. `int32` and `float64` are written as decimal text, and `boolean` as `true` or `false`. A null leaf is listed by its path in one `null` parameter, so it does not collide with the text `null`. `filledValues` takes the same map, decodes each leaf with the scalar codec, and refuses a leaf that its type does not admit. The cast to `T` goes away, and the result has the form's initial type.

## 5. Issues

One chain, one owner (D names the owner at acceptance). Each issue lands with its tests.

1. The shared table. Add `submission-cases.json` with every case that `tests/submission.rs` asserts today. Make `tests/submission.rs` table-driven over it. The terminal reducer does not change, and its test run passes as before.
2. The web reducer. Add `web/runtime/src/submission.ts` and `web/runtime/test/submission.test.ts` over the shared table, including the bulk fold. Change the comment of `supplied.ts`.
3. The typed request field map and the scalar codec. The generator writes the type of each request leaf. `wire.ts` reads both entry forms. Add `scalar.ts` and its tests for `int32`, `float64`, nullable, optional, and optional and nullable. `edit-cell.tsx` uses the codec. Regenerate the served applications. The bytes of each binding change only in its request field map.
4. The form emitter. Forms, bulk forms and delete screens call the reducer, disable submit while pending, and offer retry or refresh in `Uncertain`. The numeric control uses the codec. Component tests in `web/components/test` show five things. Two rapid submits make one pending intent. An uncertain retry sends the original supplied values. A refused retry keeps the earlier uncertainty. `int32` and `float64` inputs become numbers. A bulk call folds to each of the four states.
5. Absent, null and value. The presence action in `web/ui`, the `ChoiceField` and `CheckField` changes, and the emitter. Component tests show that an optional update input returns to absent, that a nullable input sends null, and that an optional and nullable input sends each of the three. Snapshots of the changed controls in light and dark mode.
6. Typed prefill. `fillPath` and `filledValues` over the typed map, with the `null` parameter. Tests in `web/shell/test/fill.test.ts` show that `42`, `true`, `null` and a text `null` each survive encode and decode. A component test in `prefill.test.tsx` reloads a filled form address.
7. Closeout. `docs/architecture/execution.md` says that the web client uses the same reducer and the same table. `pnpm run check` and the workspace tests pass. The log path goes in the close reason.

## 6. Out of scope

Phases 2 to 5 of the review. Each is scoped after its predecessor closes.

- Phase 2, typed application composition. Goal: exact generated types for `onOpen`, `onFill` and the form initial values, so that a typo in an operation key fails compilation. Boundary: generated types only, with no change to routes or navigation (`client_component.rs:553`, `:560`).
- Phase 3, the form-definition experiment. Goal: one complex form, as a generated form definition and a shared `OperationForm` (review §5.3). Boundary: one form, as an experiment. Its success list includes: generated bytes of every other form unchanged.
- Phase 4, reference projection. Goal: tables that show and use display text for a reference. Boundary: display fields beside ids (review §7.3) are application contract work, and this plan does not design them.
- Phase 5, generator cleanup. Goal: split `client_component.rs`, and state imports explicitly instead of reading them from emitted text. Boundary: after the forms are definition-driven.

Review §14, verbatim:

> ### Do not move UI behavior into `wamn.json`
>
> The application manifest should not become a general form/table layout language.
>
> Presentation composition belongs in application TypeScript until repeated needs demonstrate a smaller declarative requirement.
>
> ### Do not generate routes/navigation automatically
>
> The current developer-owned composition layer is valuable.
>
> Automation should generate reusable typed screens, not guess an application's workflow.
>
> ### Do not merge `web/runtime`, `web/ui`, and `web/shell`
>
> They represent distinct concerns and currently have a good dependency direction.
>
> ### Do not replace the current table architecture merely because it is large
>
> The table's complexity mostly corresponds to real capabilities.
>
> The recent `QueryTable` / `SetTable` separation is an improvement and should settle before further structural changes.
>
> ### Do not add generic abstraction merely to shrink generated source
>
> A shared form engine is worthwhile because it centralizes behavior that must be correct and consistent—not simply because generated files are long.

The owner's rulings on the review that bind later work, and not phase 1:

- An inferred relationship with more than one candidate refuses generation and names both operations. No contract selector exists until a real application hits the refusal. Today the table update takes the first candidate (`client_plan.rs:1217`).
- Columns hidden by default, if kept, follow a mechanical rule only: `revision`, the four record-history stamp columns, and the row key when the relation has a display field.
- Unavailable complete-set functions stay visible and disabled with their reason. The review's "browsing versus complete-set analysis" (§8.4, last paragraph) is struck, because it is a mode.
- `let kind` in `client_component.rs` is a forbidden name, filed as `wamn-ld93.35` under the kind to type sweep.

## 7. Questions

1. The terminal reducer resets on a target replacement (`invalidate` and `activate`, `submission.rs:283-303`, `execution.md:670`), and the shared table carries those cases. What invalidates a web submission? One choice is the end of the browser session in `web/runtime/src/session.ts` (sign-out or expiry). The other is that the web reducer has no binding and the table marks those cases terminal-only.
2. The three bound rules in §6 (ambiguous inference, hidden columns, disabled with reason) are in no phase of the review. Which phase carries each of them?
