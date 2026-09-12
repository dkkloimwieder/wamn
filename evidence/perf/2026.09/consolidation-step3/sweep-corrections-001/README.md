At source `c1d674892cac4305651d3f0a31f9b810cc70481c`, all three [focused test commands](results.json) exited 0.
The [development command tests](command-1.log#L169) passed 7 tests in 0.25 seconds after a 20.850-second command run, and the [dependency tests](command-2.log#L196) passed 3 tests in 0.48 seconds after an 81.450-second command run.
The [host credential test](command-3.log#L171) passed 1 test in 0.10 seconds after a 103.229-second command run, with 8 tests filtered out.
These runs exercise the corrected inputs and expectations for the three new ordinary failures in [sweep run 001](../sweep-001/run-001/classification-002/report.md).
They do not replace the retained workspace sweep or execute the Receiving application cases.
