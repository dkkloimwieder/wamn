The full Receiving operator test passed at `ce789398` in 94.284 seconds.
The operator loaded its live order data, opened the reference editor, and saved a draft.
A native source edit stopped the old operator, host, and socket before their replacements started.
The replacement used a new target identity and an empty reference draft.

The test confirmed draft discard and closed the whole development session.
The terminal returned to its original state.
Both host logs remained private INFO records and did not enter the operator screens.
After both hosts exited, the test read their completed purchase-order query traces from Tempo.
The traces carried different host identities and HTTP status 200.

All seven outer cleanup conditions passed.
The test removed its rows, processes, containers, volumes, and reserved listeners.
It restored the source edit, and the runner found unchanged source bytes and HEAD.
The [operator result](operator-result.json) records each transition and both retained trace identities.
The [outer result](result.json) records source stability, cleanup, and complete redaction.
