# 2b — an unbound-but-routable host answers 503, and the route comes back

Validates the fork patch `2a183dfb` (`wamn-2w3x.2`) and answers the question
`wamn-0h0g.17.20` left open: does the endpoint ever return?

| | |
|---|---|
| source commit | `1fd93af3` (`perf/17-20-watch`), fork pin `2a183dfb` |
| load average at launch | 5.37 7.57 7.76 |
| method | one-second sampler across the journey's `--measure-startup` restart, held past teardown by `WAMN_JOURNEY_HOLD_SECONDS` |
| samples | 505 |

## Result

**Zero 404s in 505 samples.** The 21 seconds of false 404 measured in `2-auth`
are gone; the same window now answers 503 with `Retry-After: 1`.

**The route comes back.** This was genuinely unknown before — the previous watch
was truncated by teardown 33 s after withdrawal.

| moment | time | |
|---|---|---|
| last 200 before the kill | 20:19:47 | |
| first 503 | 20:19:54 | endpoint still advertised, workload unbound |
| last 503 | 20:20:14 | **20 s** of retryable refusal |
| endpoint withdrawn | 20:20:15 | 62 s with no endpoint at all |
| **first 200 after** | **20:21:16** | **89 s from the kill** |

Status histogram over the run: 128 × 200, **15 × 503, 0 × 404**, 369 × connection
error (the withdrawn-endpoint stretch and the pre-cluster warmup).

Normal serving is unaffected: the run's six sampled requests all returned 200 at
15–34 ms.

## What this does and does not change

**Honest, not shorter — as ruled.** The 503 window is 20 s against the 404
window's 21 s. The patch changes what a client is told, not how long the rebind
takes. A caller now retries where it previously accepted a wrong answer.

**The rebind window is unchanged and still the real defect.** The operator keeps
the EndpointSlice pointed at a host whose workload is unbound, because the
endpoint's `Ready` is written `true` unconditionally and gated only on a
`workload.Status` that stays stale. The desired shape — endpoint readiness
computed from bound-workload state, so a restarting host receives no traffic at
all — is upstream Go, recorded on `wamn-0h0g.17.20` with its trigger, neither
patched nor filed per standing law.

## Why the journey's restart arm still fails

Not the defect any more. The probe's retry budget is **45 s** and its
`activeDeadlineSeconds` is **90 s**; measured recovery is **89 s**. The probe
gives up 44 s before the route returns, and the deadline leaves no room for the
success it would then report.

The probe's own comment states the dichotomy it was written to settle:

> a route that returns 404 immediately after a host restart and then recovers
> means Kubernetes readiness precedes in-host route registration and this
> journey waited on the wrong signal; one that never recovers means released
> routes are lost across a restart.

**It recovers.** So by the probe's own terms the journey waited on the wrong
signal, and the arm needs a budget above the measured recovery — an owner
decision, because raising it ratifies ~89 s of post-restart unavailability as
acceptable.

## Raw data

`2b-503-retryable/` holds `restart-watch.log` (505 one-second samples), the
client log, the journey evidence and the launch load average.
