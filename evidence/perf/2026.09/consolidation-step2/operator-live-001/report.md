The live Receiving test failed at `36b6f575` after 167.382 seconds.
The operator loaded the empty order list, refreshed the seeded order, and opened its reference editor.
The test then expected HTTP request details that the private host log did not contain.
The [result](result.json) records this failure and all seven passing cleanup conditions.

The host uses INFO, and pinned `wash-runtime` emits request messages at DEBUG.
The private log retained startup messages and remained an owned file with mode 0600.
The test did not reach source editing or the replacement operator.
Source bytes and HEAD stayed unchanged.
The owner decision on request logging remains pending.

The [launch command](launch-command.json) records the clean environment policy and source commit.
The runner exported redacted evidence and withheld private host diagnostics.
The runner removed its processes, Docker containers, volumes, and reserved listeners.
This run does not satisfy the live operator acceptance criterion.
