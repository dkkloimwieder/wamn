# Mutation tests

A mutation deliberately changes code or an input to test whether an assertion detects the intended defect.
Use focused controls for the named behavior under test.
This does not require a new campaign engine or a mandatory matrix for every test.

## Apply and observe the defect

Make sure that the intended source or fixture change actually occurred.
A substitution that changes nothing is not a surviving mutant.
A substitution that changes extra sites does not isolate the intended defect.
Inspect the actual changed input rather than relying on the mutation command's exit alone.

Judge the test's exit status and its reached assertion, not the number of printed failure lines.
A mutant is detected only when the test completes and fails on the mutated property.
A crash before that assertion or an unrelated setup failure is not that result.

## Distinguish the intended assertion

Use a known-bad input that must fail the relevant guard.
The control must isolate that guard, rather than trigger a neighboring refusal first.
Assert the distinguishing step or observed value, not a count that an incorrect implementation can also produce.
Use fixture values that an incorrect default or guessed constant cannot accidentally match.

When another real constraint still protects the invariant, record the surviving mutant.
Do not weaken that constraint to demand unsafe application behavior.
For Receiving, this matters when removing one lock still leaves line locks and quantity constraints active.

Retain the failed property and reproduction inputs under the [evidence rules](evidence.md).
Restore the exact original source and fixture after the controlled change.
Do not count deliberate source changes as application fixes or merge them into the application.
