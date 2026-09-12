# Test evidence

Evidence records what executed, which inputs it used, and what it established.
Use the existing test owner's structured results and raw output.
Do not create another result registry or a handwritten run narrative.
Retention paths and command capture belong to [running tests](../operations/running-tests.md).

## Execution and limits

Record the command and exit status with the package, target, and full test name.
Report executed passes, failures, ignored cases, explicit skips, and filtered cases separately.
Compilation alone does not execute an assertion.
A selected name can match zero tests and still return a successful process exit.
Require the expected result row and executed count before reporting that a named case ran.

Some live tests return early when an input is absent and appear in the reported pass count.
Treat that result as an explicit skip, not live execution.
Unavailable prerequisites leave required cases incomplete.
A known or classified failure remains a failure.
Preserve unrelated failures rather than changing assertions to obtain a passing report.

## Reproduction

Retain the invariant identifiers, source revision, component and SQL identities, fixture, inputs, and command history used by the test.
Include generator configuration and relevant schema and toolchain versions.
A seed alone does not identify the environment or artifacts.
Keep minimized counterexamples and the original failure, including infrastructure failures encountered during shrinking.

Record observed scheduling for live races without promising identical repetition.
For bounded outcomes, retain the actual deliveries, observations, recovery assumptions, and limits.
Keep credentials and private production data out of fixtures and public output.

A result applies to the source and artifacts that the test actually exercised.
A partial result cannot complete the required boundary.
Test evidence does not change the pilot's separate grading rubric or the repository's integration policy.
