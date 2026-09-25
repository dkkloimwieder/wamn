# WAMN Testing Strategy

**Status:** DRAFT\
**Purpose:** define the testing layers WAMN uses, what each establishes, and where additional detail belongs.

---

The [current testing strategy](../testing/strategy.md) owns existing practice.
This proposal guides the experiment in Beads epic `wamn-ywb0`.

## 1. Principle

Use the **smallest test boundary that proves the required property**.

Different test modes establish different facts. A test at one layer does not prove behavior at another.

```text
business requirements
        ↓
formal business model
        ↓
unit / property testing
        ↓
model-based application histories
        ↓
real PostgreSQL / local application integration
        ↓
cluster / released-system testing
```

Applications own their business fixtures, rules, expected results, and workflows.

Platform test support owns shared execution infrastructure, controlled dependencies, environment setup, and reporting.

---

## 2. Current testing status

| Mode                                                  | Status                    | Establishes                                                                  |
| ----------------------------------------------------- | ------------------------- | ---------------------------------------------------------------------------- |
| Rust/WIT compilation                                  | Established               | Type and interface compatibility                                             |
| SQLx verification                                     | Established               | SQL/schema/bind/result compatibility                                         |
| Unit tests                                            | Established               | Local decision behavior and explicit boundaries                              |
| Proptest                                              | Established               | Generated input/state exploration                                            |
| Model-based application histories                     | Established for Receiving | Production behavior compared with an independent business model              |
| Deterministic router testing                          | Established               | Router invariants under generated topology, outcomes, and controlled time    |
| Deterministic run-state testing                       | Partial                   | Pure queue invariants; broader controlled SQL scheduling remains future work |
| Event simulation                                      | Established, narrow       | Repeatable external test traffic                                             |
| Guest record/replay                                   | Proposed as needed        | Controlled guest effects without real external dependencies                  |
| PostgreSQL integration                                | Established               | Transactions, authority, RLS, locking, rollback, concurrency                 |
| Local real-Wasm application tests                     | Established               | Real guest/capability/auth/database behavior without cluster overhead        |
| Cluster/application delivery tests                    | Established, selective    | Packaging, processes, brokers, deployment and released behavior              |
| Targeted mutation testing                             | Established pattern       | Important assertions detect intended defects                                 |
| Formal business-logic verification                    | Proposed                  | Business-state consistency and invariant preservation                        |
| Broad fuzzing / Kani on arbitrary implementation code | Deferred                  | Add only for a demonstrated gap                                              |

---

## 3. Static and compile-time verification

Rust and WIT compilation establish structural and interface compatibility.

SQLx verifies the exact generated and authored SQL against the intended schema, including:

- SQL validity;
- binds;
- result columns;
- result types and nullability.

These checks do **not** establish:

- business behavior;
- authorization;
- RLS;
- rollback;
- locking;
- concurrency;
- idempotency.

Those require runtime tests.

See the focused SQL/data-access testing documentation.

---

## 4. Unit and property testing

Unit tests should exercise pure decisions and explicit boundary examples.

Use Proptest where combinations or histories matter.

Generated testing should:

- call the real decision function;
- use an independently expressed expected rule/model;
- retain explicit boundary and regression examples;
- shrink failures without replacing the business failure with an infrastructure failure.

A finite property-test sample is not mathematical proof.

See [unit and property](../testing/unit-and-property.md).

---

## 5. Deterministic platform testing

Deterministic testing controls the inputs, outcomes, time, or event order owned by the test driver.

### Router

The router currently has the strongest deterministic coverage.

Generated walks exercise real router decisions and check invariants throughout execution, including:

- hop limits;
- no execution after completion;
- token accounting;
- retry limits;
- wait timing;
- terminal routing.

### Run state

Pure run-state invariants currently cover lease, claimability, retry-budget, and expiry decisions.

A broader deterministic run-state scheduler remains a possible future extension for histories such as:

```text
admit
→ claim
→ crash
→ lease expiry
→ reclaim
→ complete
```

when a concrete platform guarantee requires it.

### Guest replay

General guest effect replay is not currently required.

Introduce it only when a named test needs deterministic external effects that are otherwise expensive or difficult to reproduce.

See [deterministic](../testing/deterministic.md).

---

## 6. Business-model and application testing

Applications with meaningful state machines should maintain an independent business model where that model adds useful coverage.

Receiving currently demonstrates:

```text
generated command history
        ↓
independent expected-state model

        AND

same history
        ↓
real authenticated application
        ↓
real Wasm
        ↓
real capability / transaction
        ↓
PostgreSQL

        ↓
compare observed result and state with model
```

This is the preferred pattern for testing:

- lifecycle transitions;
- business invariants;
- idempotency;
- replay;
- refusals;
- stale revisions;
- competing commands.

WMS provides complementary coverage around movement, adjustment, split/merge, conservation, and partial completion.

See [application tests](../testing/application-tests.md).

---

## 7. Formal business-logic verification

Formal methods should initially target **business-state consistency**, not arbitrary implementation details.

The intended question is:

> Given the valid business states, commands, refusals, and invariants, can any permitted transition produce an invalid or contradictory state?

The preferred initial approach is:

```text
semi-technical business requirement
        ↓
structured business rule
        ↓
small executable Rust business model
        ↓
Kani proof
```

Kani is a good first fit when the business model can be expressed as a finite Rust transition system.

Typical proof obligations include:

```text
initial state satisfies invariant

every successful transition preserves invariant

refusal leaves business state unchanged

cancelled state cannot accept forbidden commands

exact replay returns the same business result

changed intent under the same claim refuses
```

Prefer inductive transition proofs over arbitrary fixed history depths when possible.

The verified business model should remain independent of database and runtime implementation.

Implementation conformance is then established separately through Proptest and integration testing.

Use other tools only where the problem demands them:

| Problem                                     | Likely tool            |
| ------------------------------------------- | ---------------------- |
| Finite executable business transition model | Kani                   |
| Highly relational state model               | Alloy                  |
| Temporal/distributed/liveness protocol      | TLA+                   |
| Implementation histories                    | Proptest               |
| Database concurrency/authority              | PostgreSQL integration |

See [formal business proposal](testing-formal.md).

---

## 8. PostgreSQL integration

Use real PostgreSQL whenever the guarantee depends on database behavior.

This includes:

- transactions;
- rollback;
- RLS;
- grants and identities;
- constraints;
- optimistic concurrency;
- locking;
- competing transactions;
- history;
- claim behavior.

Concurrency tests should use independent database connections and establish real overlap without relying on sleeps.

The business model proves the permitted logical outcome.

The PostgreSQL test proves the real implementation reaches one of those permitted outcomes.

See [database tests](../testing/database-tests.md).

---

## 9. Local application integration

Use the local application boundary for application correctness that requires real WAMN behavior but not Kubernetes:

```text
real Wasm
→ real WAMN host/runtime
→ authenticated request
→ real capability
→ real transaction
→ disposable PostgreSQL
```

This should be the default integration boundary for business commands where deployment infrastructure is irrelevant to the assertion.

---

## 10. Cluster and released-system testing

Cluster tests are reserved for properties requiring deployed infrastructure, including:

- packaged/released artifacts;
- process boundaries;
- broker configuration;
- workload identity;
- deployment/readiness;
- restart and shutdown;
- real event delivery;
- environment boundaries.

A successful deployment or ready Pod does not establish application correctness; application-owned assertions must still execute.

See [cluster tests](../testing/cluster-tests.md).

---

## 11. Mutation testing

For critical invariants, deliberately break the intended protection and confirm that the relevant assertion detects the defect.

A mutation counts only when the intended test fails for the intended reason.

Unrelated setup or infrastructure failure is not evidence.

Broad automated mutation campaigns are not currently required.

See [mutation](../testing/mutation.md).

---

## 12. Evidence and execution

Required tests must actually execute.

A successful command that:

- matched zero cases;
- skipped the required case;
- ignored the required case;
- lacked a prerequisite;

does not establish success.

Test evidence should distinguish:

- pass;
- failure;
- skip/ignore;
- missing prerequisite;
- partial execution.

Use change-aware test selection during development and broader qualification at release boundaries.

See [evidence](../testing/evidence.md) and the repository delivery/testing commands.

---

## 13. Testing responsibilities by layer

```text
BUSINESS MODEL
formal verification
→ is the business state machine coherent?

IMPLEMENTATION
unit + Proptest + model histories
→ does production behavior conform to the business model?

DATABASE
real PostgreSQL
→ are atomicity, authority, locking and concurrency correct?

APPLICATION
real local Wasm integration
→ does the real command work through WAMN boundaries?

SYSTEM
cluster/release tests
→ does the deployed system preserve the behavior?
```

No layer substitutes for another.

---

## 14. Near-term direction

1. Continue expanding model-based application histories where applications have meaningful state machines.
2. Pilot the structured business-rule → Rust model → Kani workflow on Receiving.
3. Apply the same approach to WMS before designing any reusable formal-model abstraction.
4. Extend deterministic run-state scheduling only when a named execution guarantee requires it.
5. Add guest replay only for concrete guest-testing needs.
6. Keep broad fuzzing and implementation-level formal verification demand-driven.

The intended long-term split is:

> **Formal verification for business-model consistency; deterministic simulation for platform execution; model/property testing for implementation conformance; real integration for database, runtime, and deployment guarantees.**
