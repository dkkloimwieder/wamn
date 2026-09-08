## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (13.27 → 27.87 ms) | ×1.03 | 1259 | 16 | 27.87 ms |
| `nodb` | **8** | 16 (3.03 → 5.83 ms) | ×0.96 | 9653 | 64 | 16.70 ms |
| `pg` | **8** | 16 (0.24 → 1.16 ms) | ×0.88 | 79978 | 8 | 0.24 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 282 | 3.26 | 7.44 | 0 | 1 | 2816 | 724 | postgres=2 project=14 | 1.12 | 5.7 | 0 % | 0.13 |
| 4 | 797 | 4.39 | 13.16 | 0 | 4 | 7966 | 2132 | postgres=2 project=14 | 2.00 | 3.4 | 0 % | 0.36 |
| 8 | 1218 | 6.16 | 13.27 | 0 | 8 | 12181 | 3454 | postgres=2 project=14 | 2.65 | 3.0 | 0 % | 0.52 |
| 16 | 1259 | 11.81 | 27.87 | 0 | 16 | 12574 | 3612 | postgres=2 project=14 | 2.72 | 3.0 | 0 % | 0.54 |
| 32 | 934 | 32.01 | 67.00 | 0 | 32 | 9317 | 2828 | postgres=2 project=14 | 2.22 | 3.3 | 0 % | 0.42 |
| 64 | 1020 | 61.36 | 104.79 | 0 | 64 | 10134 | 2959 | postgres=2 project=14 | 2.43 | 3.3 | 0 % | 0.45 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1949 | 0.46 | 1.48 | 0 | 1 | 19491 | 315 | postgres=2 project=14 | 1.13 | 0.8 | 0 % | 0.00 |
| 4 | 6304 | 0.56 | 1.74 | 0 | 4 | 63049 | 1 | postgres=2 project=14 | 2.61 | 0.6 | 0 % | 0.00 |
| 8 | 8993 | 0.77 | 3.03 | 0 | 8 | 89950 | 1 | postgres=2 project=14 | 3.42 | 0.5 | 0 % | 0.00 |
| 16 | 8643 | 1.64 | 5.83 | 0 | 16 | 86429 | 0 | postgres=2 project=14 | 3.33 | 0.5 | 0 % | 0.00 |
| 32 | 8276 | 3.54 | 11.17 | 0 | 32 | 82759 | 0 | postgres=2 project=14 | 3.26 | 0.5 | 0 % | 0.00 |
| 64 | 9653 | 6.26 | 16.70 | 0 | 64 | 96535 | 1 | postgres=2 project=14 | 3.60 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 24579 | 0.04 | 0.10 | 0 | 0 | 245631 | 17845 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.51 |
| 4 | 50021 | 0.07 | 0.16 | 0 | 0 | 499681 | 35942 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.90 |
| 8 | 79978 | 0.08 | 0.24 | 0 | 0 | 798397 | 57702 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.34 |
| 16 | 70064 | 0.17 | 1.16 | 0 | 0 | 698115 | 50845 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.21 |
| 32 | 66957 | 0.33 | 2.23 | 0 | 0 | 663386 | 48056 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.24 |
| 64 | 66522 | 0.59 | 6.06 | 0 | 0 | 656440 | 47338 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.33 |
