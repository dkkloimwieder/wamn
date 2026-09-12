# Testing

Testing combines ordinary examples, generated inputs, real database execution, and deployed application cases. Each method establishes behavior only at the boundary it exercises. These pages define test expectations and limits. Use [running tests](../operations/running-tests.md) for commands, inputs, and database isolation.

- [Strategy](strategy.md): choose the tested boundary and its owner.
- [Unit and property tests](unit-and-property.md): exercise decisions and shrink generated failures.
- [Database tests](database-tests.md): inspect transactions, authority, and contention.
- [Application tests](application-tests.md): test commands, overlays, events, and operator outcomes.
- [Deterministic tests](deterministic.md): distinguish controlled execution from live scheduling.
- [Cluster tests](cluster-tests.md): exercise packaged applications and process boundaries.
- [Evidence](evidence.md): retain actual execution, failure inputs, and limitations.
- [Mutation tests](mutation.md): establish that assertions detect the intended defect.
