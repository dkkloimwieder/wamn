## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (21.67 → 33.47 ms) | ×1.14 | 1231 | 64 | 109.94 ms |
| `nodb` | **8** | 16 (2.33 → 3.99 ms) | ×1.08 | 12098 | 64 | 11.40 ms |
| `pg` | **8** | 16 (0.23 → 0.89 ms) | ×0.90 | 85883 | 8 | 0.23 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 193 | 4.34 | 15.48 | 0 | 1 | 1932 | 626 | postgres=2 project=14 | 0.84 | 6.4 | 0 % | 0.10 |
| 4 | 636 | 4.73 | 20.69 | 0 | 4 | 6362 | 1987 | postgres=2 project=14 | 1.43 | 3.4 | 0 % | 0.24 |
| 8 | 925 | 7.77 | 21.67 | 0 | 8 | 9244 | 3138 | postgres=2 project=14 | 2.02 | 3.0 | 0 % | 0.37 |
| 16 | 1054 | 14.13 | 33.47 | 0 | 16 | 10525 | 3996 | postgres=2 project=14 | 2.28 | 3.0 | 0 % | 0.43 |
| 32 | 1126 | 27.25 | 47.18 | 0 | 32 | 11237 | 3860 | postgres=2 project=15 | 2.40 | 3.0 | 0 % | 0.41 |
| 64 | 1231 | 49.37 | 109.94 | 0 | 64 | 12258 | 4492 | postgres=2 project=18 | 2.53 | 2.8 | 0 % | 0.43 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2560 | 0.36 | 0.84 | 0 | 1 | 25603 | 400 | postgres=2 project=18 | 1.17 | 0.6 | 0 % | 0.00 |
| 4 | 6928 | 0.51 | 1.58 | 0 | 4 | 69298 | 0 | postgres=2 project=18 | 2.57 | 0.5 | 0 % | 0.00 |
| 8 | 10447 | 0.68 | 2.33 | 0 | 8 | 104501 | 0 | postgres=2 project=18 | 3.62 | 0.5 | 0 % | 0.00 |
| 16 | 11316 | 1.29 | 3.99 | 0 | 16 | 113186 | 1 | postgres=2 project=18 | 3.59 | 0.4 | 0 % | 0.00 |
| 32 | 10398 | 2.87 | 8.04 | 0 | 32 | 103995 | 1 | postgres=2 project=18 | 3.59 | 0.5 | 0 % | 0.00 |
| 64 | 12098 | 5.14 | 11.40 | 0 | 64 | 120965 | 0 | postgres=2 project=18 | 3.89 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 20751 | 0.04 | 0.12 | 0 | 0 | 207392 | 15007 | postgres=2 project=18 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 53687 | 0.07 | 0.15 | 0 | 0 | 536219 | 38523 | postgres=2 project=18 | 0.03 | 0.0 | 0 % | 1.88 |
| 8 | 85883 | 0.08 | 0.23 | 0 | 0 | 857482 | 61312 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.31 |
| 16 | 77248 | 0.17 | 0.89 | 0 | 0 | 769435 | 55650 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.21 |
| 32 | 69616 | 0.31 | 2.33 | 0 | 0 | 690374 | 49406 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.01 |
| 64 | 68582 | 0.56 | 5.93 | 0 | 0 | 677895 | 49193 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.22 |
