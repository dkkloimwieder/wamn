## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.38 → 32.36 ms) | ×0.87 | 1342 | 8 | 11.38 ms |
| `nodb` | **8** | 16 (2.74 → 5.34 ms) | ×0.93 | 9683 | 8 | 2.74 ms |
| `pg` | **8** | 16 (0.30 → 1.02 ms) | ×1.06 | 75382 | 16 | 1.02 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 345 | 2.78 | 5.03 | 0 | 1 | 3446 | 1185 | postgres=2 project=13 | 1.27 | 4.9 | 0 % | 0.15 |
| 4 | 878 | 4.15 | 10.36 | 0 | 4 | 8774 | 3029 | postgres=2 project=13 | 2.05 | 3.2 | 0 % | 0.37 |
| 8 | 1342 | 5.64 | 11.38 | 0 | 8 | 13419 | 4704 | postgres=2 project=13 | 2.74 | 2.8 | 0 % | 0.54 |
| 16 | 1170 | 12.48 | 32.36 | 0 | 16 | 11690 | 4190 | postgres=2 project=14 | 2.46 | 2.9 | 0 % | 0.50 |
| 32 | 1135 | 26.88 | 56.76 | 0 | 32 | 11324 | 4227 | postgres=2 project=14 | 2.45 | 3.0 | 0 % | 0.47 |
| 64 | 1160 | 53.49 | 98.87 | 0 | 64 | 11544 | 4233 | postgres=2 project=14 | 2.52 | 3.0 | 0 % | 0.47 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2368 | 0.40 | 0.81 | 0 | 1 | 23683 | 428 | postgres=2 project=14 | 1.19 | 0.7 | 0 % | 0.00 |
| 4 | 6221 | 0.55 | 1.91 | 0 | 4 | 62215 | 0 | postgres=2 project=14 | 2.49 | 0.5 | 0 % | 0.00 |
| 8 | 9683 | 0.72 | 2.74 | 0 | 8 | 96838 | 0 | postgres=2 project=14 | 3.44 | 0.5 | 0 % | 0.00 |
| 16 | 8964 | 1.61 | 5.34 | 0 | 16 | 89734 | 1 | postgres=2 project=14 | 3.23 | 0.5 | 0 % | 0.00 |
| 32 | 8519 | 3.42 | 10.80 | 0 | 32 | 85190 | 0 | postgres=2 project=14 | 3.12 | 0.5 | 0 % | 0.00 |
| 64 | 7326 | 8.03 | 24.66 | 0 | 64 | 73233 | 0 | postgres=2 project=14 | 2.70 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 15453 | 0.06 | 0.18 | 0 | 0 | 154382 | 11150 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.48 |
| 4 | 42801 | 0.08 | 0.23 | 0 | 0 | 427421 | 29472 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.72 |
| 8 | 71153 | 0.08 | 0.30 | 0 | 0 | 709811 | 47975 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.75 |
| 16 | 75382 | 0.16 | 1.02 | 0 | 0 | 751262 | 54668 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.17 |
| 32 | 66715 | 0.35 | 2.34 | 0 | 0 | 662787 | 48412 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.01 |
| 64 | 72220 | 0.56 | 5.58 | 0 | 0 | 712593 | 50348 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 3.21 |
