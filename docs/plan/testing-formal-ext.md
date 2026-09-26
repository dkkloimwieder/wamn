# WAMN Business Logic Verification Plan

## Goal

Establish a repeatable method for verifying important business rules as applications grow:

```text
Business requirements
    ↓
Executable business model / kernel
    ↓
Kani proofs
    ↓
Production implementation
    ↓
Model ↔ implementation conformance
    ↓
PostgreSQL/runtime integration tests
    ↓
Cross-application composition proofs
```

The objective is not to formally verify the entire platform. It is to make high-value business invariants incrementally explicit, executable, and difficult to violate.

---

## Current baseline — WMS

The first WMS slice establishes the approach:

- explicit `Inventory`, `Packaging`, and immutable `InventoryTransaction`;
- Kani proofs for conservation, lineage, lifecycle, replay, refusals, and complete history;
- deliberate defects produce counterexamples;
- production aligns with the target model;
- inventory mutation, history, and replay state commit atomically in PostgreSQL;
- rollback, concurrency, replay, refusals, and ledger immutability are tested;
- the released direct inventory cluster path passes.

This is the baseline to extend rather than redesign.

---

# Phase 1 — Expand one application model

Add **packaging relocation** as the next WMS increment.

A relocation should exercise:

- one packaging identity;
- multiple inventory identities;
- explicit inventory location changes;
- one transaction row for every affected inventory identity;
- locking and atomicity;
- rollback;
- replay;
- preservation of existing history.

Extend the Kani model only for the new semantics.

Prove at minimum:

- all affected open inventory remains co-located with its packaging;
- quantities remain unchanged;
- every affected inventory identity receives complete history;
- partial relocation cannot commit;
- refusal preserves state;
- previous history and replay results remain unchanged.

Keep the formal state small.

---

# Phase 2 — Move verified logic into production business kernels

Reduce the current separation between formal model and implementation.

Extract suitable business decisions into pure Rust functions such as:

```rust
decide(state, command) -> Result<Transition, Refusal>
```

`State` contains only facts required for the decision.

`Transition` describes required business effects, for example:

```text
inventory updates
inventory creation/closure
transaction rows
command result
```

Infrastructure stays outside:

- PostgreSQL;
- HTTP;
- WIT;
- authentication;
- deployment.

Production becomes approximately:

```text
BEGIN
lock/load state
call decide(...)
persist transition
persist immutable history
persist replay result
COMMIT
```

Do this incrementally per operation. Do not introduce a generic framework prematurely.

---

# Phase 3 — Prove production decision logic

Where the production decision kernel is pure enough, have Kani verify the actual code used by production.

Typical obligations:

```text
valid state + successful transition
    -> valid resulting state
```

plus:

- quantity conservation;
- lifecycle rules;
- lineage;
- disposition rules;
- transaction completeness;
- refusal preserves state.

The preferred end state is:

> Kani proves the same business decision code production executes.

This substantially reduces the model/implementation correspondence problem.

---

# Phase 4 — Model ↔ implementation conformance

Even with shared business kernels, production retains behavior outside the formal abstraction.

Build a differential test harness that executes generated command histories against:

1. the executable business model/kernel;
2. the real WMS application over PostgreSQL.

After each command compare:

```text
model outcome
==
production outcome

model state
==
abstract(production state)

model history
==
abstract(production history)

model replay result
==
abstract(production replay result)
```

Use property-based generation and shrinking so failures produce small reproducible histories.

This becomes the main evidence that the real implementation conforms to the verified semantics.

---

# Phase 5 — Keep persistence/runtime guarantees separate

Kani should not model PostgreSQL unless a database behavior is itself the subject of the proof.

Integration tests establish the assumptions the business model relies on:

- conflicting operations lock correctly;
- state and immutable history commit atomically;
- history-write failure rolls everything back;
- idempotency claims are atomic;
- replay returns stored original results;
- ledger entries cannot be updated or deleted by application authority;
- structural constraints are enforced;
- deployed behavior matches local behavior.

This separation should remain explicit:

```text
Kani
    proves business semantics

PostgreSQL/runtime tests
    prove those semantics are executed atomically and correctly
```

---

# Phase 6 — Validate the method on another application

After WMS has exercised a more substantial model and production kernel, apply the same method to a second application/domain with different business rules.

Prefer a domain containing:

- lifecycle transitions;
- cross-record invariants;
- refusal conditions;
- idempotent commands;
- potential transactional participation.

Only after at least two domains should reusable formal infrastructure be considered.

Do not create a formal DSL based solely on WMS.

---

# Phase 7 — Define composition contracts

WAMN applications can participate in one PostgreSQL transaction through the transaction participant mechanism.

Formal models should compose through **business contracts**, not modeled database handles.

Example:

```text
Participant contract

Requires:
    preconditions X

On success:
    guarantees Y
    preserves Z

On refusal:
    no modeled state change
```

Each application proves its own invariants independently.

The participant application then proves that it satisfies the contract assumed by its caller.

This provides an assume/guarantee boundary between applications.

---

# Phase 8 — Prove composed business operations

For important compositions, create a small combined model:

```rust
struct CombinedState {
    app_a: AppAState,
    app_b: AppBState,
}
```

Model the composed operation as one atomic transition:

```text
A prepares
    ↓
B participates
    ↓
A finalizes
    ↓
success

any failure
    ↓
original combined state
```

Prove:

- App A invariants remain true;
- App B invariants remain true;
- cross-application invariants hold;
- participant refusal leaves both unchanged;
- successful composition is atomic;
- replay remains stable;
- previous history remains immutable.

Do not combine entire applications unnecessarily. Model only the state required by the composition contract.

---

# Phase 9 — Production composition conformance

Exercise the same composed operation through real WAMN applications and the shared PostgreSQL transaction mechanism.

Test failures at important boundaries:

```text
A changes state, B refuses
B writes, A later fails
history insertion fails
participant traps
transaction aborts
```

Persistent state must correspond to the atomic result assumed by the formal composition model.

---

# Near-term sequence

1. Add WMS packaging relocation across multiple inventory identities.
2. Extend the WMS Kani model and proofs.
3. Implement and test the real PostgreSQL operation.
4. Extract a suitable WMS operation into a pure production business kernel.
5. Have Kani prove that production kernel.
6. Add model-vs-production generated history tests.
7. Repeat for another significant WMS operation.
8. Apply the method to a second application.
9. Define one real transactional participant contract.
10. Prove and integration-test one two-application composition.

---

## Design principle

Prefer:

> **small verified business kernels + explicit invariants + strong production correspondence**

over:

> **large formal models increasingly detached from production.**

The formal model should grow with business semantics, not with infrastructure or application size.
