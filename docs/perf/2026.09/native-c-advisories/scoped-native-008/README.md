# Native C scoped test result — 2026-09-11

The [command](record.json) ran at `5ef028880f4c83283ae344008bbb298adbd88003` and exited 0 after 196.824 seconds, with one passed test executing for 19.42 seconds.
The [Cargo output](cargo.log) records three environment declarations and 18 runtime management refusals, eight foreign metadata and data refusals, six foreign runtime refusals, and two foreign materializer attachment refusals.
The broker redelivered after disconnection before acknowledgement and bounded pulls and pending acknowledgements across 65 messages of 1,047,552 bytes each, then made later progress.
The source event expired under its declared two-second age limit while advisory metadata remained, with no message deletion call.
These native broker results establish no outcome for the full Receiving test or the workspace sweep, and all original records remain unchanged.
