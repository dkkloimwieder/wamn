# Native C source checkpoint

`wamn-0ct2.7` records the reviewed source at `26fdd3eda8e194730a016bf933f2917a6120c05b`.
The [patch](source.patch) contains the native materializer binding, scoped broker credentials, event coordinates, and their direct callers.
The [source record](source.json) gives the exact file hashes and modes.

The host binding test passes, and two credential tests pass.
Twelve runtime tests execute successfully, while one unarmed live test skips explicitly.
Both changed integration targets compile, but that command executes no tests.
The [handoff](handoff.json) retains all six commands, their results, and the remaining live cases.
It also retains the initial lock-file failure and the first binding run with concurrent source changes.
The later binding run passes with unchanged source.

Automatic approval review initially refused the local commit.
The owner then approved it explicitly.
The [approved commit record](../approved-commit-001/result.json) confirms the committed source matches the reviewed files.

Native C remains open.
Live correctness, authority, delivery pressure, source-payload retention, and the integrated workspace test run remain pending.
Observer access to shared advisory metadata still needs the owner's answer.
The development activation and executor declaration need a separate direct-caller change after this checkpoint.
