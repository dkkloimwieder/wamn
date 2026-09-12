## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (12.97 → 23.75 ms) | ×1.06 | 1344 | 16 | 23.75 ms |
| `nodb` | **8** | 16 (3.62 → 9.92 ms) | ×0.69 | 8856 | 32 | 9.70 ms |
| `pg` | **8** | 16 (0.32 → 1.43 ms) | ×0.86 | 70621 | 8 | 0.32 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 289 | 3.25 | 6.80 | 0 | 1 | 2885 | 880 | postgres=2 project=14 | 1.08 | 5.5 | 0 % | 0.13 |
| 4 | 900 | 4.09 | 9.36 | 0 | 4 | 8999 | 3068 | postgres=2 project=14 | 2.07 | 3.2 | 0 % | 0.38 |
| 8 | 1268 | 5.89 | 12.97 | 0 | 8 | 12679 | 4536 | postgres=2 project=14 | 2.64 | 2.8 | 0 % | 0.52 |
| 16 | 1344 | 11.23 | 23.75 | 0 | 16 | 13432 | 4825 | postgres=2 project=14 | 2.74 | 2.8 | 0 % | 0.55 |
| 32 | 1172 | 26.48 | 43.13 | 0 | 32 | 11689 | 4284 | postgres=2 project=14 | 2.49 | 2.9 | 0 % | 0.47 |
| 64 | 1007 | 61.39 | 98.76 | 0 | 64 | 10013 | 3675 | postgres=2 project=14 | 2.26 | 3.1 | 0 % | 0.43 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2029 | 0.45 | 1.24 | 0 | 1 | 20297 | 434 | postgres=2 project=14 | 1.14 | 0.8 | 0 % | 0.00 |
| 4 | 5477 | 0.59 | 2.69 | 0 | 4 | 54778 | 0 | postgres=2 project=14 | 2.27 | 0.6 | 0 % | 0.00 |
| 8 | 8459 | 0.78 | 3.62 | 0 | 8 | 84588 | 0 | postgres=2 project=14 | 3.13 | 0.5 | 0 % | 0.00 |
| 16 | 5818 | 2.38 | 9.92 | 0 | 16 | 58181 | 0 | postgres=2 project=14 | 2.27 | 0.6 | 0 % | 0.00 |
| 32 | 8856 | 3.35 | 9.70 | 0 | 32 | 88579 | 1 | postgres=2 project=14 | 3.31 | 0.5 | 0 % | 0.00 |
| 64 | 8222 | 7.33 | 20.14 | 0 | 64 | 82195 | 0 | postgres=2 project=14 | 3.17 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 22103 | 0.04 | 0.11 | 0 | 0 | 220879 | 15354 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.48 |
| 4 | 41585 | 0.08 | 0.24 | 0 | 0 | 415173 | 29911 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.79 |
| 8 | 70621 | 0.09 | 0.32 | 0 | 0 | 706028 | 47930 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.84 |
| 16 | 60530 | 0.18 | 1.43 | 0 | 0 | 603289 | 43697 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 2.84 |
| 32 | 57795 | 0.36 | 3.15 | 0 | 0 | 573203 | 39273 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.70 |
| 64 | 70107 | 0.52 | 5.97 | 0 | 0 | 692919 | 49674 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.23 |
