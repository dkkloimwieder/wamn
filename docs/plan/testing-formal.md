# WAMN Business Logic Formalization and Verification Proposal

**Status:** PROPOSED\
**Scope:** application business logic, requirements formalization, executable business models, formal verification, and implementation conformance\
**Initial target:** Receiving POC\
**Primary verification candidate:** Kani\
**Related testing:** Proptest/model-based histories, PostgreSQL integration tests, application integration tests

---

## 1. Objective

Introduce a lightweight process for translating business requirements into **explicit, reviewable business-state models** that can be formally verified.

The primary question is:

> **Given the application's valid states, commands, refusals, and invariants, can any permitted sequence of business operations produce a contradictory or impossible state?**

This is distinct from proving incidental implementation properties such as integer arithmetic or individual low-level functions.

The intended verification stack is:

```text
semi-technical business requirements
            ↓
structured business rules
            ↓
reviewed abstract business model
            ↓
executable Rust state/transition model
            ↓
Kani proofs
            ↓
Proptest/model-based implementation conformance
            ↓
real PostgreSQL/application integration tests
```

Each layer establishes a different fact.

Formal verification establishes consistency of the **business model**.

Implementation tests establish that production behavior conforms to that model.

---

# 2. Motivation

WAMN applications increasingly describe business behavior in terms of:

- entities and aggregates;
- lifecycle states;
- commands;
- success conditions;
- refusals;
- transactions;
- revisions;
- idempotency;
- events;
- base and overlay behavior.

This naturally creates state machines.

Traditional example-based tests can verify selected histories:

```text
open
→ receive
→ receive
→ complete
```

Property tests can generate many more histories.

Neither necessarily establishes that **no combination of otherwise legal transitions** can violate an invariant.

A finite formal model can investigate questions such as:

- Can an order become complete without all required quantities being received?
- Can an order be over-received?
- Can a cancelled order later receive stock?
- Are any declared states unreachable?
- Can the model enter a state from which no legitimate command can proceed?
- Can two transition rules apply simultaneously but imply inconsistent outcomes?
- Can replay change a business result?
- Can base and overlay rules permit contradictory combined states?

These are specification questions before they are implementation questions.

---

# 3. Key principle: do not translate prose directly into proof code

The process should not be:

```text
business prose
    ↓
Kani harness
```

Business requirements are commonly incomplete, ambiguous, contextual, or expressed through examples.

The proposed process introduces an intermediate artifact:

```text
business requirement
        ↓
structured business rule
        ↓
formalization
```

The structured rule remains readable by a solutions architect while being precise enough for an engineer to formalize.

This intermediate step is essential.

It makes unresolved business semantics visible instead of allowing the formalizer to invent them.

---

# 4. Requirement normalization

Most business requirements should be translated into one or more of four categories.

## 4.1 Invariants

A property that must hold in every valid business state.

Example:

```text
received_quantity <= ordered_quantity
```

Or:

```text
status = complete
    ->
every required line is fully received
```

These are natural candidates for inductive Kani proofs.

---

## 4.2 Preconditions and refusal rules

Conditions under which a command may succeed.

Example:

```text
COMMAND:
    record_receipt

SUCCESS REQUIRES:
    purchase_order.status = open
    line exists
    quantity > 0
    quantity <= remaining_quantity
```

Failure should normally be explicit:

```text
otherwise:
    refuse
    business state unchanged
```

This matches WAMN's command/refusal model better than silently treating invalid transitions as no-ops.

---

## 4.3 Postconditions

Properties that must hold after a successful command.

Example:

```text
record_receipt(quantity)

on success:

received_quantity'
    =
received_quantity + quantity
```

The prime notation is useful:

```text
value     = before command
value'    = after command
```

It is precise without requiring the requirements author to understand proof tooling.

---

## 4.4 Lifecycle or history properties

Properties that concern previous or future states.

Example:

```text
once cancelled:
    no later record_receipt may succeed
```

Or:

```text
same idempotency key + same intent
    ->
same business result
```

These may be expressed by:

- making relevant historical facts part of the abstract state;
- proving transition restrictions;
- or, where genuinely necessary, exploring bounded histories.

---

# 5. Proposed business-rule artifact

The initial implementation should use ordinary Markdown rather than introduce another WAMN schema or DSL.

Example:

```text
ID:
REC-INV-01

STATEMENT:
The received quantity of a purchase-order line may never exceed
its ordered quantity.

STATE:
- PurchaseOrderLine.ordered_quantity
- PurchaseOrderLine.received_quantity

INVARIANT:
0 <= received_quantity <= ordered_quantity

AFFECTED COMMANDS:
- record_receipt
- reverse_receipt
- adjust_receipt, if introduced

SUCCESS EXAMPLE:
ordered = 10
received = 8
receipt = 2

result:
received = 10

REFUSAL EXAMPLE:
ordered = 10
received = 8
receipt = 3

result:
ExcessQuantity
business state unchanged

OPEN QUESTIONS:
- Are negative corrections permitted?
- May administrator correction exceed ordered quantity?

FORMALIZATION:
state invariant + transition-preservation proof

IMPLEMENTATION EVIDENCE:
generated receipt histories
concurrent PostgreSQL receipt test
```

This artifact becomes the bridge between domain design and verification.

---

# 6. Requirement identifiers and traceability

Important business rules should receive stable identifiers.

For example:

```text
REC-INV-01
Received quantity never exceeds ordered quantity.

REC-LIFE-01
Cancelled purchase orders cannot accept receipts.

REC-IDEM-01
Exact command replay returns the original business result.

REC-IDEM-02
Reusing an idempotency key for different intent refuses.
```

The identifier should follow the rule through every verification layer.

Example:

```rust
// REC-INV-01
#[kani::proof]
fn receipt_preserves_quantity_bound() {
    ...
}
```

And:

```rust
// REC-INV-01
#[test]
fn competing_receipts_cannot_over_receive() {
    ...
}
```

A traceability view can then show:

| Requirement | Formal model    | Kani proof                 | Property/history test | Live integration      |
| ----------- | --------------- | -------------------------- | --------------------- | --------------------- |
| REC-INV-01  | Receiving model | quantity invariant         | generated receipts    | concurrent PG receipt |
| REC-LIFE-01 | lifecycle model | cancelled transition proof | generated histories   | authenticated route   |
| REC-IDEM-01 | claim model     | replay proof               | generated replay      | lost-response/retry   |

This should remain documentation/test metadata rather than require a separate runtime registry.

---

# 7. Roles

The process conceptually has three responsibilities.

## 7.1 Domain / solutions architect

Owns:

- business nouns;
- lifecycle states;
- commands;
- expected successful effects;
- refusal conditions;
- invariants;
- examples;
- business terminology.

The solutions architect should not need to write Rust or Kani.

The required output is a sufficiently explicit business rule.

---

## 7.2 Formalizer / application engineer

Owns translation into:

- abstract state;
- abstract commands;
- transition functions;
- invariant predicates;
- proof obligations;
- bounded domains where necessary.

The formalizer must **not invent unresolved business semantics**.

If the source requirement cannot be represented unambiguously, it should return to requirements analysis.

---

## 7.3 Domain reviewer

Reviews the resulting model against the business intent.

The most important review question is not:

> Does the Kani proof pass?

It is:

> **Does this formal model actually mean what the business requirement means?**

Formal verification of the wrong model provides little value.

---

# 8. Ambiguity is an output

A failed formalization attempt should be considered useful requirements analysis.

For example:

```text
Requirement:

"Completed orders normally cannot be modified."
```

This cannot yet be formalized.

Questions include:

```text
What does "normally" mean?

Which commands are exceptions?

Can a completed order be cancelled?

Can receipt reversal reopen it?

Can descriptive metadata change?

Can an administrator override the rule?
```

The process should explicitly produce:

```text
UNRESOLVED BUSINESS RULE

REC-LIFE-07

Source requirement:
"Completed orders normally cannot be modified."

Formalization blocked because:
- permitted exceptions are unspecified;
- reversal behavior is unspecified;
- administrative override is unspecified;
- modification is not defined.
```

The formalizer should not choose answers.

---

# 9. Abstract Rust business model

Once the business rule is settled, it can be represented as a deliberately small Rust model.

For Receiving:

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Open,
    Complete,
    Cancelled,
}

#[derive(Clone, Copy)]
struct Line {
    ordered: u8,
    received: u8,
}

#[derive(Clone, Copy)]
struct State {
    status: Status,
    lines: [Line; 2],
}
```

The model should contain only business-relevant state.

Do not model implementation detail unless it affects the property.

Normally exclude:

```text
UUID representation
PostgreSQL row layouts
JSON
WIT
HTTP
database connection identity
wall-clock timestamp representation
```

Instead use:

```text
small integers
small enums
abstract identities
logical time
fixed-size collections
```

The objective is **state-space coverage**, not production-data realism.

---

# 10. Business invariants as executable predicates

Requirements should become simple predicates.

Example:

```rust
fn valid(state: &State) -> bool {
    let quantities_valid =
        state.lines
            .iter()
            .all(|line| line.received <= line.ordered);

    let completion_valid =
        state.status != Status::Complete
        || state.lines
            .iter()
            .all(|line| line.received == line.ordered);

    quantities_valid && completion_valid
}
```

This remains traceable back to business language:

```text
received quantity never exceeds ordered quantity

complete means every required line is fully received
```

---

# 11. Commands as explicit transition functions

The abstract model should distinguish successful transitions from refusals.

Prefer:

```rust
enum Refusal {
    OrderNotOpen,
    UnknownLine,
    ExcessQuantity,
}

fn record_receipt(
    state: State,
    line: usize,
    quantity: u8,
) -> Result<State, Refusal> {
    ...
}
```

over:

```rust
invalid transition -> return unchanged state
```

The explicit refusal model allows separate verification of:

```text
successful commands preserve invariants
```

and:

```text
refused commands preserve business state
```

This aligns directly with WAMN command semantics.

---

# 12. Primary Kani proof pattern: inductive invariants

For most business invariants, WAMN should prefer **one-transition inductive proofs** over arbitrary fixed history depths.

The proof structure is:

```text
initial states satisfy invariant

AND

every possible transition from a valid state
preserves the invariant

THEREFORE

every reachable state satisfies the invariant
```

Conceptually:

```rust
#[kani::proof]
fn transition_preserves_business_invariant() {
    let state: State = kani::any();
    let command: Command = kani::any();

    kani::assume(valid(&state));

    if let Ok(next) = transition(state, command) {
        assert!(valid(&next));
    }
}
```

This avoids an arbitrary statement such as:

```text
the property holds for histories up to five commands
```

when the property can instead be proven inductively for any reachable sequence.

---

# 13. Example proof obligations

A Receiving model might initially define:

## REC-INV-01 — quantity bound

```text
For every valid state and every possible receipt command:

if record_receipt succeeds,
received_quantity' <= ordered_quantity.
```

## REC-CMD-01 — exact receipt effect

```text
On successful receipt:

selected_line.received'
    =
selected_line.received + quantity

All unrelated business lines remain unchanged.
```

## REC-CMD-02 — refusal preservation

```text
If record_receipt refuses:

business_state' = business_state
```

## REC-LIFE-01 — cancelled state

```text
If status = cancelled:

record_receipt cannot succeed.
```

## REC-COMPLETE-01 — completion

```text
status = complete
    ->
all required lines are fully received
```

Potentially, depending on business intent:

```text
all required lines are fully received
    ->
status = complete
```

These are different requirements and must not be conflated.

---

# 14. Idempotency modeling

Idempotency is particularly well suited to an abstract executable model.

The model may include:

```text
claim key
intent identity
stored result
```

An abstract command execution could return:

```text
new state
+
business result
```

Then prove:

## Exact replay

```text
execute(initial, command)
    -> state1, result1

execute(state1, same command)
    -> state2, result2

state2 == state1
result2 == result1
```

## Changed intent

```text
same idempotency key
+
different intent
    ->
refusal
+
state unchanged
```

This verifies the intended business semantics independently of database implementation.

PostgreSQL integration tests still establish that the real claim and transaction implementation provides those semantics under concurrency and lost responses.

---

# 15. Concurrency

Business models should normally avoid embedding PostgreSQL locking details.

Suppose:

```text
remaining quantity = 5

receipt A = 4
receipt B = 4
```

The business model should prove:

> Every permitted serialization of these commands preserves the quantity invariant.

The integration test separately proves:

> The real PostgreSQL implementation forces concurrent execution into a permitted outcome.

Thus:

```text
formal business model
    proves valid logical outcomes

real PostgreSQL test
    proves implementation realizes one of those outcomes
```

The formal model should not need to know about:

```text
SELECT FOR UPDATE
row locks
transaction isolation internals
```

unless those are themselves the object being modeled.

---

# 16. Relation to Proptest

The same abstract model can potentially serve two complementary roles.

```text
                    abstract Rust business model
                              │
             ┌────────────────┴───────────────┐
             │                                │
             ▼                                ▼
           Kani                            Proptest
    exhaustive symbolic              generated realistic
    bounded verification                  histories
             │                                │
             │                                ▼
             │                     production application
             │                                │
             └──────── conceptual ────────────┘
```

Kani asks:

> Does the model itself preserve its declared invariants for every value in the bounded domain?

Proptest asks:

> Does the real implementation agree with the independent model across generated command histories?

These are different guarantees.

---

# 17. Independence of the oracle

The model must remain an independent expression of business intent.

Avoid creating:

```text
production implementation
        ↓
model mechanically copied from production implementation
        ↓
proof
```

That risks proving the implementation against itself.

Instead:

```text
business requirement
        ↓
abstract model
        ↘
         Kani

business requirement
        ↓
production implementation
        ↘
        conformance test
```

A defect in production can then disagree with the model.

A defect or ambiguity in the model can be reviewed against the business rule.

---

# 18. Potential production pure-business kernel

A stronger future architecture is worth experimenting with.

Some commands may be factored as:

```rust
fn decide(
    state: &OrderState,
    command: &RecordReceipt,
) -> Result<Decision, BusinessRefusal>
```

`decide` performs no SQL or external effects.

It describes what should happen.

Production execution becomes:

```text
load + lock current state
        ↓
decide()
        ↓
typed decision
        ↓
apply generated writes
        ↓
commit
```

If practical, Kani could verify the **actual production decision function**.

Then:

```text
Kani
    proves production business decision preserves invariants

PostgreSQL integration
    proves the decision is applied atomically

application integration
    proves command behavior across real boundaries
```

This would provide a particularly strong connection between formal verification and production code.

It should be explored rather than mandated.

Some business commands may not decompose cleanly this way.

---

# 19. WMS applicability

WMS is a strong second candidate because its business rules include conservation properties.

Potential model:

```text
Pallet
Location
Movement
Quantity
Adjustment
Split
Merge
LabelOutcome
```

Candidate invariants:

```text
A pallet has at most one current location.
```

```text
Move preserves total inventory.
```

```text
Split preserves quantity:

source_before
    =
source_after + new_pallet_quantity
```

```text
Merge preserves quantity.
```

```text
Only an explicit adjustment may create or remove inventory quantity.
```

```text
One movement may finalize at most once.
```

```text
A downstream label failure cannot reverse an already committed inventory movement.
```

This is sufficiently different from Receiving to determine whether common formalization patterns actually exist.

---

# 20. Tool boundaries

Kani should not become the universal formal tool.

Use the tool appropriate to the property.

| Problem                                     | Likely technique                |
| ------------------------------------------- | ------------------------------- |
| Finite executable business transition model | Kani                            |
| Sampled implementation histories            | Proptest                        |
| PostgreSQL transaction/authority/locking    | Live PostgreSQL                 |
| Highly relational structural model          | Alloy may be useful             |
| Multi-actor temporal/liveness protocol      | TLA+ may be useful              |
| Deployed system behavior                    | Cluster/application integration |

This proposal initially adopts **Kani only for finite business transition models**.

Other formal tools should be introduced only if a concrete problem requires them.

---

# 21. State-space discipline

Formal models must remain deliberately small.

Prefer:

```text
2 purchase-order lines
quantities 0..3
2 idempotency claims
small enum state
small command set
```

rather than:

```text
arbitrary strings
unbounded collections
real UUID representation
real timestamps
large JSON payloads
thousands of records
```

A logical contradiction usually has a small counterexample.

Example:

```text
ordered = 1

receipt A = 1
receipt B = 1

received = 2
```

Increasing data volume does not make the logical defect more meaningful.

---

# 22. Proposed initial experiment

## Phase F0 — Receiving formalization POC

The prototype work order and Beads epic `wamn-ywb0` control this experiment.
The earlier examples are illustrative, not additional Receiving requirements.
Receiving exposes no cancellation command. Its cancelled status supplies a refusal fixture.
Exact replay returns its stored result even when the order is now complete.
Completion claims start from the valid states in the application model.
Production refactoring, shared infrastructure, and generated formal models remain outside this prototype.

Create a small Rust/Kani model for:

```text
PurchaseOrder
PurchaseOrderLine
Receipt
IdempotencyClaim
```

Initial commands supported by the current application:

```text
record_receipt
```

Initial business properties:

1. received quantity never exceeds ordered quantity;
2. complete implies all required lines are fully received;
3. a cancelled initial order refuses a new receipt;
4. successful receipt changes quantity exactly as specified;
5. refusal leaves business state unchanged;
6. receipt contributes exactly once;
7. exact replay returns the original result;
8. changed intent under the same claim refuses.

The model should contain no PostgreSQL, WIT, HTTP, or Wasm behavior.

---

# 23. POC acceptance criteria

The experiment is useful if it demonstrates all of the following.

### Requirements translation

A solutions architect can review the structured rules without understanding Kani.

### Ambiguity detection

Record any real ambiguity as an unresolved business question. Do not invent a requirement or require an ambiguity quota.

### Verification

Kani proves the selected invariants across the bounded model.

### Counterexample quality

An intentionally introduced business-rule defect produces a small, understandable counterexample.

### Implementation connection

At least one verified property maps directly onto an existing Receiving model-based or PostgreSQL integration test.

### Maintainability

The business model remains materially smaller and simpler than the production implementation.

---

# 24. Phase F1 — connect proofs and implementation histories

After the model is useful independently, connect it to existing Receiving history testing.

The intended pattern is:

```text
structured business rule
          ↓
abstract Rust model
       ↙       ↘
    Kani      Proptest expected state
                 ↓
        production application
```

Where practical, a counterexample discovered formally should become an explicit regression history against production.

This establishes a useful path:

```text
formal counterexample
        ↓
real implementation reproduction
        ↓
specification defect or implementation defect identified
```

---

# 25. Phase F2 — WMS

Repeat the experiment for WMS.

Focus on:

- quantity conservation;
- movement state;
- split;
- merge;
- adjustment;
- downstream partial completion.

Do not introduce common WAMN formal abstractions merely because Receiving used them.

Only abstractions that recur naturally across materially different applications should be considered common.

---

# 26. Phase F3 — evaluate generation

Only after several applications should WAMN consider whether existing application metadata can generate some formal-model structure.

Possible future relationship:

```text
WAMN model/operation metadata
        +
authored business rules
        ↓
generated model skeleton
        ↓
reviewed business semantics
        ↓
proofs
```

Generation should not infer business semantics that are absent from the source requirements.

For example, a schema can identify:

```text
ordered_quantity
received_quantity
```

It cannot safely infer:

```text
received_quantity <= ordered_quantity
```

without an authored business rule.

---

# 27. Non-goals

This proposal does **not** propose:

- a new WAMN business-rule DSL;
- automatic translation from English directly into trusted proof code;
- automatically proving arbitrary application code;
- replacing Proptest;
- replacing integration testing;
- modeling PostgreSQL internals in business models;
- making every application formally verified;
- formalizing simple CRUD with no meaningful business state machine;
- requiring solutions architects to learn Rust or Kani.

---

# 28. Long-term possibility

If repeated applications reveal a stable pattern, WAMN may eventually support something conceptually equivalent to:

```text
business state
business invariant
command
success precondition
postcondition
refusal condition
replay semantics
```

But that abstraction should be **discovered from applications**, not designed before the POCs demonstrate what is common.

The near-term implementation remains:

```text
Markdown business contract
        ↓
human-reviewed Rust model
        ↓
Kani proof
```

---

# 29. Proposed responsibility of formalization

Formalization should be treated as part of **requirements analysis**, not merely testing.

A successful outcome may be:

```text
proof succeeds
```

but it may equally be:

```text
requirement cannot be formalized because the business rule is incomplete
```

or:

```text
two accepted business rules contradict each other
```

or:

```text
a declared state is unreachable
```

or:

```text
the application architecture cannot atomically enforce the rule
```

Those are valuable design findings.

---

# 30. Proposed direction

WAMN should experiment with a layered business-verification model:

```text
SOLUTIONS ARCHITECT
business terminology + requirements
            ↓
STRUCTURED BUSINESS CONTRACT
state + invariants + commands + refusals + examples
            ↓
APPLICATION ENGINEER
small executable Rust business model
            ↓
KANI
model consistency / invariant proofs
            ↓
PROPTEST
production implementation vs model
            ↓
POSTGRESQL / LOCAL APPLICATION TESTS
real atomicity, authority, contention and capabilities
            ↓
CLUSTER TESTS
released-system behavior
```

The important new seam is the **structured business contract**.

It allows semi-technical business requirements to become formal without asking domain authors to write verification code and without allowing engineers to silently invent missing semantics.

The guiding principle is:

> **Business experts define the rules; formalization makes those rules precise; Kani checks whether the resulting state machine can contradict them; implementation tests establish that the real application behaves like the verified model.**
