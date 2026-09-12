# 2a — instrument the authenticate legs

Measurement only. No behaviour changed; this run exists so the auth fix is
chosen against numbers rather than a guess.

| | |
|---|---|
| source commit | `8669ba27` (`perf/2-auth`, off `main` `5dd7aa95`) |
| load average at launch | 2.57 3.71 2.77 |
| load average at finish | 1.51 2.57 2.67 |
| route | `POST /purchase_order/get`, `Host: receiving.localhost` |
| write route | `POST /purchase_order/update` (same journey, one sample) |
| samples | one cold, then four hot, then one write |
| cluster | disposable `wamn-receiving-journey` kind cluster, private kubeconfig |

## What was added

Three spans on the legs `FlowHttpRouting::authenticate` awaits — `wamn.auth.pat`,
`wamn.auth.roles`, `wamn.auth.permissions` — and four inside the permission read
itself: `wamn.auth.perm.begin`, `.timeout`, `.query`, `.commit`.

## The interior of `wamn.route.authenticate`

`↳` columns are children of `wamn.auth.permissions`. `residue` is
`authenticate` minus its three legs. All values in milliseconds.

| trace | auth.pat | auth.roles | auth.permissions | ↳acquire | ↳begin | ↳timeout | ↳query | ↳commit | residue | **authenticate** |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | 2.026 | 0.939 | 47.71 | 45.474 | 0.175 | 0.178 | 1.122 | 0.264 | 0.206 | **50.882** |
| hot-1 | 1.419 | 0.753 | 2.709 | 0.163 | 0.31 | 0.358 | 0.983 | 0.532 | 0.239 | **5.121** |
| hot-2 | 0.985 | 0.731 | 2.151 | 0.153 | 0.387 | 0.284 | 0.665 | 0.303 | 0.25 | **4.118** |
| hot-3 | 1.132 | 0.633 | 1.79 | 0.101 | 0.27 | 0.26 | 0.63 | 0.234 | 0.23 | **3.785** |
| hot-4 | 1.199 | 0.72 | 2.006 | 0.103 | 0.304 | 0.268 | 0.683 | 0.305 | 0.264 | **4.189** |
| write | 1.071 | 0.55 | 1.792 | 0.13 | 0.213 | 0.232 | 0.656 | 0.276 | 0.195 | **3.607** |

Hot means over the four samples:

| leg | mean ms | share of authenticate |
|---|---:|---:|
| `auth.pat` | 1.184 | 27.5 % |
| `auth.roles` | 0.709 | 16.5 % |
| `auth.permissions` | 2.164 | 50.3 % |
| leg residue | 0.246 | 5.7 % |
| **authenticate** | **4.303** | |

Inside `auth.permissions` (hot means): acquire 0.130, BEGIN 0.318,
SET timeout 0.293, query 0.740, COMMIT 0.344, glue 0.339.

## The finding

**The 3.3 ms that had no span is not compute. It is nine database round trips.**
The three legs plus their residue account for 100 % of `authenticate`; nothing
is hidden, and no leg does meaningful work between its awaits.

The nine, and why each exists:

| # | round trip | cost ms | why |
|---:|---|---:|---|
| 1–2 | `authenticate_pat` Parse+Describe, then Bind+Execute | 1.184 | `tokio_postgres::Client::query_opt` is handed a `&str` |
| 3–4 | `project_roles` Parse+Describe, then Bind+Execute | 0.709 | same |
| 5 | `BEGIN` | 0.318 | opens a transaction around a read |
| 6 | `SET statement_timeout` | 0.293 | re-sent every request |
| 7–8 | permission query Parse+Describe, then Bind+Execute | 0.740 | `&str` again |
| 9 | `COMMIT` | 0.344 | closes the transaction opened at 5 |

The `&str` claim is not inferred from the timings. `tokio-postgres` 0.7.18
`to_statement.rs` converts a `&str` through `prepare::prepare`, which is an
unconditional Parse+Describe+Sync with no cache — `prepare_cached` belongs to
`deadpool_postgres::Object`, not to `Client`. The trace agrees with the source:
`.timeout` uses `prepare_cached` and costs 0.293 ms for one round trip, while
`.query` on a bare `&str` costs 0.740 ms — 2.5× — against a `BEGIN` that is one
round trip at 0.318 ms.

**Four of the nine buy nothing.**

- `BEGIN` and `COMMIT` wrap a read that installs no session claim. The function's
  own doc comment says so: *"installs no `app.role` or `app.user_id` session
  claim."* This is exactly the rule 3c proved for guest statements, never applied
  to the host's own authorization read. **0.662 ms.**
- `SET statement_timeout` carries a pool-uniform value re-sent per request.
  `wamn-0h0g.17.18` already owns hoisting it into connection setup. **0.293 ms.**

Removing those three round trips is arithmetic on measured spans: **0.955 ms,
22 % of `authenticate`,** with no cache and no semantic change.

Holding the three statements as prepared `Statement` handles instead of `&str`
removes three more Parse round trips. That one is a **projection**, not a
measurement: at the 0.30–0.32 ms this cluster charges for a single round trip it
is worth roughly 0.9–1.0 ms, and the honest way to confirm it is to build it and
re-measure.

## Where the hot request now stands

Same four hot samples, whole-request phases. `residue` is
`handle_http_request` minus the phases that partition it.

| trace | auth | resolve | linker_setup | link | instantiate | postgres | statement | residue | **total** | ratio |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| cold | 50.882 | 0.115 | 3.366 | 0.191 | 1.483 | 54.482 | 1.514 | 10.454 | **121.474** | 40.53 |
| hot-1 | 5.121 | 0.063 | 3.824 | 0.899 | 1.667 | 1.913 | 0.743 | 7.954 | **21.768** | 9.03 |
| hot-2 | 4.118 | 0.066 | 3.387 | 0.172 | 1.241 | 1.428 | 0.649 | 6.992 | **17.938** | 9.49 |
| hot-3 | 3.785 | 0.072 | 2.669 | 0.145 | 1.179 | 1.327 | 0.544 | 7.245 | **16.664** | 9.67 |
| hot-4 | 4.189 | 0.067 | 3.32 | 0.244 | 1.612 | 2.061 | 0.817 | 7.879 | **19.682** | 8.10 |
| write | 3.607 | 0.057 | 2.649 | 0.184 | 1.282 | 4.823 | 2.081 | 7.149 | **20.232** | 6.02 |

`ratio` is `handle_http_request / (postgres.statement + component.instantiate)`.
The target is 3. Hot sits at **8.1–9.7**.

Ranked by hot mean, the request is now:

| phase | mean ms | note |
|---|---:|---|
| unspanned residue | 7.52 | fork body-read and guest execution — `wamn-0h0g.17.16`, `.17.17` |
| `wamn.route.authenticate` | 4.30 | this report |
| `wamn.component.linker_setup` | 3.30 | fix 1b, the `Linker` rebuilt per request |
| `wamn.postgres` | 1.68 | after 3b and 3c |
| `wamn.component.instantiate` | 1.42 | fix 4 would remove it |

Cold in this run was 121 ms at the probe, against the journey's own cold receipt
of 24.4 ms for the same cluster; the difference is entirely
`wamn.postgres.acquire` — 45.5 ms in the auth pool and 51.9 ms in the guest pool
— the first TCP connect and role handshake on each of the two pools. The probe
fires before the journey's own request, so it pays that connect and the journey
does not.

## Gate status

The journey's `--measure-startup` **restart arm failed again**, for the sixth
consecutive run: `error: timed out waiting for the condition on
jobs/startup-request-restart-first`. It is a pre-existing failure independent of
this change — it predates every fix in this directory — and it does not touch
the cold, hot or write samples above, which all returned 200 with complete
traces. Everything reported here was measured; the journey as a whole did not
pass.

Nothing in the tree asserts the overhead ratio yet. Arming it today would make
every journey run fail at the end and skip its own teardown, so it is an open
owner decision rather than something to land silently.

## Honesty

- Absolute hot milliseconds are **not** comparable across reports. The journey
  builds on the machine it measures and load ran 1.5–3.7 during this one.
- The ratio and the within-trace shares **are** comparable: they are quotients of
  spans from the same trace.
- The three-round-trip saving from held `Statement` handles is arithmetic on the
  measured per-round-trip cost, not a measurement. The 0.955 ms from dropping
  `BEGIN`/`COMMIT`/`SET` is a sum of measured spans.

## Raw data

`2a-auth-instrument/` holds the six traces, `auth-table.md`, the client log, the
launch load average and the full journey evidence directory.
`tools/auth-row.sh` regenerates the interior table; `tools/span-tree.sh` and
`tools/phase-row.sh` regenerate the trees and the phase table.

## Span trees, verbatim

See `2a-auth-instrument/span-trees.txt`.
