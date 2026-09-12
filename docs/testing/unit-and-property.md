# Unit and property tests

Unit tests exercise a function with explicit examples.
Property tests generate inputs and test a rule across those samples.
Finite samples are not a mathematical proof over all possible inputs or histories.

## Inputs and expected results

Call the real decision function.
Use the existing Proptest strategies or `Arbitrary` pattern where combinations matter.
Retain explicit boundary examples and regression cases alongside generated inputs.

Generated command inputs cover numeric boundaries, absent/null/value distinctions, missing records, invalid status, stale revisions, repeated keys, and changed bodies.
Bound each generated history and begin from a valid fixture.
Compare the observed result and settled state with a small, independently expressed model.
Do not use the production validator as the expected-answer function.
Do not translate SQL into a second implementation just to obtain an expected result.

The [Receiving history model](../../apps/wamn_receiving/tests/receiving_history/model.rs) supplies application steps and expected state.
Its [test owner](../../apps/wamn_receiving/tests/receiving_command_histories_live.rs) drives the real authenticated routes.
The model alone does not execute those routes or establish their database behavior.

## Shrinking failures

Shrinking reduces a failing generated input while preserving the failure.
Keep fixture dependencies and the original business property intact.
An infrastructure error must not replace the business counterexample.
Retain the minimized regression with a stable test name.

The Receiving history tests exercise both failure-preservation cases explicitly.
Keep the reproduction inputs described in [evidence](evidence.md).
For controlled execution, see [deterministic tests](deterministic.md).
