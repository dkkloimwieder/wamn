# Testing strategy

Use the smallest existing test that reaches the changed behavior.
Compile checks, generated examples, database tests, and deployed tests establish different facts.
A test of one layer does not establish a boundary that it never crosses.

## Ownership

Applications own fixtures, commands, expected results, business rules, and workflows.
Platform test support owns invocation, controlled dependencies, environment setup, and failure reporting.
Extend those owners instead of creating a parallel runtime or test-definition language.

An invariant is a rule at a stated observation boundary.
Record its identifier, rule, affected state, observation boundary, enforcing code or constraint, and named tests.
Keep these facts in the application requirements and ordinary test code, without another registry.

Choose the observation that matches the rule:

- State assertions inspect coherent committed state, such as quantities within their limits.
- History assertions inspect permitted transitions and the original result of a replayed claim.
- Eventual assertions require a result within stated time bounds and recovery assumptions.

A confirmed refusal before commit leaves the named business state unchanged.
List permitted claim or audit changes separately.
Unknown completion is not a refusal.
For critical invariants, retain success, refusal or failure, and [deliberate-defect evidence](mutation.md).

## Scope

Use real application and platform functions through existing Rust test owners.
No complete simulation framework is a prerequisite for application testing.
Generated claim tests complement full-command execution and do not replace it.

Application testing does not wait for the proposed workflow in [the delivery plan](../plan/delivery.md).
