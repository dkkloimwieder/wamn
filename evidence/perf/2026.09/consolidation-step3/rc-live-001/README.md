# RC live result

The run fails at the gates image digest assertion before either selected test Job starts.
The exact error is `the loaded image has no unique runtime digest`.
All three host image records agree on their configuration and runtime digests. The run does not save the failed gates image inspection.
The cause remains unresolved in this record.

The separate cleanup record passes for the owned `wamn-rc` cluster, its two named containers, and its exact images.
The primary result leaves cleanup and source-change fields null. It does not establish either test result or source stability.
The original files remain unchanged. `retained-files.json` records their bytes, hashes, and filesystem modes.
