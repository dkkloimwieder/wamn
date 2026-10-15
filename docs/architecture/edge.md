# Edge

## Status

Epic `wamn-1bd2` added the status of a running edge ([its specification](../history/edge-diagnostics-spec.md)).

- The edge serves `GET /status` on the Unix socket `status.sock` in the directory of the run-state file. It binds the socket before the host takes work and removes it when it stops. A socket file that a killed edge left is removed at the next start, because the start holds the run-state file.
- The socket has no port and no configuration key, so nothing off the box reaches it. It checks no credential, as the host's probe listener checks none. Its own accept loop keeps it apart from the release routes.
- The answer is one JSON object: `started_at`, `device.dropped_frames`, `forward.credential_failures`, `forward.stopped`, `samples.pending`, and `samples.refused`. `device` and `forward` are null when the configuration names no device or no forward. The counters count since `started_at`, because they live in memory.
- `forward.stopped` is true after the forward stops at `credential_bound`. Each refused sample carries its `sample_key`, `captured_at` and `reason`, and no body.
- `wamn-edge status` prints the answer as text: one name and value per line, separated by a tab, and one `refused` line per refused sample. It finds the socket from the run-state path in the configuration.
