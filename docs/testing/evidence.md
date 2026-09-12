# Test results

Report pass, fail, or skip with the command and source revision.
Use the test owner's output to identify what ran and what it established.
Ordinary test logs are not permanent repository deliverables.
Do not create a per-run archive, result registry, or handwritten run narrative.
[Running tests](../operations/running-tests.md) explains commands and optional output directories.

## Execution and limits

Include the exit status, package, target, and full test name when reporting a selected case.
Report executed passes, failures, ignored cases, explicit skips, and filtered cases separately.
Compilation alone does not execute an assertion.
A selected name can match zero tests and still return a successful process exit.
Require the expected result row and executed count before reporting that a named case ran.

Some live tests return early when an input is absent and appear in the reported pass count.
Treat that result as an explicit skip, not live execution.
Unavailable prerequisites leave required cases incomplete.
A known or classified failure remains a failure.
Do not change unrelated assertions to obtain a passing result.

## Reproduction

For a failure, identify the relevant fixture, inputs, source, and artifacts needed to reproduce it.
Include generator configuration and schema or toolchain versions when they affect the result.
A seed alone does not identify the environment or artifacts.
Keep a minimized regression in its owning test when the defect requires one.
Distinguish the original failure from infrastructure failures encountered during shrinking.

Report observed scheduling for live races without promising identical repetition.
For bounded outcomes, state the observations, recovery assumptions, and limits.
Keep credentials and private production data out of fixtures and shared output.

A result applies to the source and artifacts that the test actually exercised.
A partial result cannot complete the required boundary.
Result reporting does not change the pilot's separate grading rubric or the repository's integration policy.
