## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (10.30 → 17.22 ms) | ×1.07 | 1611 | 16 | 17.22 ms |
| `nodb` | **8** | 16 (1.94 → 3.74 ms) | ×1.00 | 13011 | 32 | 5.48 ms |
| `pg` | **8** | 16 (0.24 → 1.02 ms) | ×0.92 | 83076 | 8 | 0.24 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 384 | 2.50 | 4.66 | 0 | 1 | 3837 | 982 | postgres=2 project=14 | 1.21 | 4.5 | 0 % | 0.14 |
| 4 | 1145 | 3.31 | 6.37 | 0 | 4 | 11448 | 3116 | postgres=2 project=14 | 2.39 | 2.9 | 0 % | 0.43 |
| 8 | 1513 | 4.99 | 10.30 | 0 | 8 | 15123 | 4251 | postgres=2 project=14 | 2.93 | 2.7 | 0 % | 0.58 |
| 16 | 1611 | 9.61 | 17.22 | 0 | 16 | 16101 | 4772 | postgres=2 project=14 | 3.04 | 2.6 | 0 % | 0.62 |
| 32 | 1304 | 24.01 | 38.80 | 0 | 32 | 13016 | 3825 | postgres=2 project=14 | 2.65 | 2.8 | 0 % | 0.51 |
| 64 | 1197 | 52.67 | 84.19 | 0 | 64 | 11912 | 3508 | postgres=2 project=14 | 2.54 | 2.9 | 0 % | 0.49 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2931 | 0.33 | 0.52 | 0 | 1 | 29313 | 292 | postgres=2 project=14 | 1.22 | 0.6 | 0 % | 0.00 |
| 4 | 8113 | 0.45 | 1.16 | 0 | 4 | 81146 | 0 | postgres=2 project=14 | 2.72 | 0.5 | 0 % | 0.00 |
| 8 | 11785 | 0.60 | 1.94 | 0 | 8 | 117869 | 0 | postgres=2 project=14 | 3.65 | 0.4 | 0 % | 0.00 |
| 16 | 11833 | 1.24 | 3.74 | 0 | 16 | 118330 | 1 | postgres=2 project=14 | 3.61 | 0.4 | 0 % | 0.00 |
| 32 | 13011 | 2.38 | 5.48 | 0 | 32 | 130135 | 1 | postgres=2 project=14 | 3.84 | 0.4 | 0 % | 0.00 |
| 64 | 12824 | 4.85 | 10.77 | 0 | 64 | 128250 | 0 | postgres=2 project=14 | 3.84 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 28550 | 0.03 | 0.07 | 0 | 0 | 285268 | 20515 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 49421 | 0.07 | 0.16 | 0 | 0 | 493636 | 35532 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.87 |
| 8 | 83076 | 0.08 | 0.24 | 0 | 0 | 829332 | 59341 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.34 |
| 16 | 76125 | 0.17 | 1.02 | 0 | 0 | 758826 | 55696 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.28 |
| 32 | 73885 | 0.32 | 1.87 | 0 | 0 | 733492 | 53011 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.22 |
| 64 | 73706 | 0.54 | 4.82 | 0 | 0 | 726449 | 52306 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.38 |
