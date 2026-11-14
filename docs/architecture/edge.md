# Edge

## Operation storage

HTTP and device calls enter the same operation boundary. The boundary authorizes the caller before it claims a key or runs application code.
The edge owns its SQLx SQLite file. One connection holds the file exclusively, with WAL and full synchronization. The pool does not retire idle or old connections. A process file lock preserves ownership while a failed connection is replaced. Each replacement takes the SQLite lock before it serves work.
Claims use the partition, stable operation reference and caller key. The partition holds the organization ID.
A completed key returns its stored result. A changed input under that key is a conflict.
An unfinished claim returns `in-progress` for 30 seconds, then `outcome-unknown`. Unfinished claims expire after 30 days.
The device stores each sample and its completed outcome in one transaction. A refused device call stores no sample.
The edge refuses the old generic intent database shape.
Operators resolve refused samples through a local decision type. There is no intent listing or resolution command.

## Status

A running edge reports its status on a local socket.

- The edge serves `GET /status` on the Unix socket `status.sock` in the directory of the run-state file. It binds the socket before the host takes work and removes it when it stops. A socket file that a killed edge left is removed at the next start, because the start holds the run-state file.
- The socket has no port and no configuration key, so nothing off the box reaches it. It tests no credential, as the host's probe listener tests none. Its own accept loop keeps it apart from the release routes.
- The answer is one JSON object: `started_at`, `device.dropped_frames`, `forward.credential_failures`, `forward.stopped`, `samples.pending`, and `samples.refused`. `device` and `forward` are null when the configuration names no device or no forward. The counters count since `started_at`, because they live in memory.
- `forward.stopped` is true after the forward stops at `credential_bound`. Each refused sample carries its `sample_key`, `captured_at` and `reason`, and no body.
- `wamn-edge status` prints the answer as text: one name and value per line, separated by a tab, and one `refused` line per refused sample. It finds the socket from the run-state path in the configuration.
