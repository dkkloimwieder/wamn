## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.28 → 19.31 ms) | ×1.07 | 1476 | 16 | 19.31 ms |
| `nodb` | **8** | 16 (2.63 → 3.64 ms) | ×1.10 | 11348 | 64 | 11.70 ms |
| `pg` | **8** | 16 (0.24 → 1.00 ms) | ×0.88 | 81315 | 8 | 0.24 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 347 | 2.68 | 5.22 | 0 | 1 | 3472 | 951 | postgres=2 project=14 | 1.29 | 4.9 | 0 % | 0.15 |
| 4 | 1025 | 3.65 | 7.60 | 0 | 4 | 10248 | 2794 | postgres=2 project=14 | 2.34 | 3.1 | 0 % | 0.41 |
| 8 | 1382 | 5.46 | 11.28 | 0 | 8 | 13814 | 3938 | postgres=2 project=14 | 2.86 | 2.9 | 0 % | 0.56 |
| 16 | 1476 | 10.44 | 19.31 | 0 | 16 | 14753 | 4203 | postgres=2 project=14 | 3.00 | 2.8 | 0 % | 0.59 |
| 32 | 1210 | 25.92 | 40.65 | 0 | 32 | 12071 | 3589 | postgres=2 project=14 | 2.67 | 3.0 | 0 % | 0.49 |
| 64 | 1242 | 51.37 | 69.18 | 0 | 64 | 12359 | 3596 | postgres=2 project=14 | 2.70 | 3.0 | 0 % | 0.50 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2481 | 0.38 | 0.73 | 0 | 1 | 24811 | 344 | postgres=2 project=14 | 1.21 | 0.7 | 0 % | 0.00 |
| 4 | 6785 | 0.53 | 1.47 | 0 | 4 | 67857 | 0 | postgres=2 project=14 | 2.67 | 0.5 | 0 % | 0.00 |
| 8 | 9644 | 0.73 | 2.63 | 0 | 8 | 96446 | 0 | postgres=2 project=14 | 3.58 | 0.5 | 0 % | 0.00 |
| 16 | 10617 | 1.44 | 3.64 | 0 | 16 | 106178 | 1 | postgres=2 project=14 | 3.83 | 0.5 | 0 % | 0.00 |
| 32 | 11207 | 2.79 | 6.11 | 0 | 32 | 112093 | 1 | postgres=2 project=14 | 3.90 | 0.5 | 0 % | 0.00 |
| 64 | 11348 | 5.52 | 11.70 | 0 | 63 | 113455 | 0 | postgres=2 project=14 | 3.94 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 25917 | 0.03 | 0.08 | 0 | 0 | 259031 | 18683 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 51889 | 0.07 | 0.14 | 0 | 0 | 518377 | 37316 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.92 |
| 8 | 81315 | 0.09 | 0.24 | 0 | 0 | 811227 | 57716 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.36 |
| 16 | 71598 | 0.17 | 1.00 | 0 | 0 | 713548 | 52106 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.27 |
| 32 | 70911 | 0.32 | 2.09 | 0 | 0 | 704168 | 50719 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.31 |
| 64 | 68823 | 0.57 | 5.40 | 0 | 0 | 678532 | 48532 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.39 |
