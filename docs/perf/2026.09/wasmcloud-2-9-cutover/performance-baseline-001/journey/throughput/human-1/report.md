## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (10.10 → 18.94 ms) | ×1.07 | 1655 | 16 | 18.94 ms |
| `nodb` | **8** | 16 (1.73 → 3.08 ms) | ×1.04 | 12265 | 16 | 3.08 ms |
| `pg` | **8** | 16 (0.28 → 0.83 ms) | ×0.94 | 82794 | 32 | 1.31 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 336 | 2.78 | 6.62 | 0 | 1 | 3361 | 945 | postgres=2 project=15 | 1.24 | 4.9 | 0 % | 0.14 |
| 4 | 1075 | 3.41 | 8.82 | 0 | 4 | 10745 | 2916 | postgres=2 project=15 | 2.29 | 2.9 | 0 % | 0.40 |
| 8 | 1552 | 4.85 | 10.10 | 0 | 8 | 15518 | 4360 | postgres=2 project=15 | 2.98 | 2.6 | 0 % | 0.58 |
| 16 | 1655 | 9.18 | 18.94 | 0 | 16 | 16541 | 4868 | postgres=2 project=15 | 3.04 | 2.5 | 0 % | 0.60 |
| 32 | 1210 | 24.91 | 56.26 | 0 | 32 | 12078 | 3665 | postgres=2 project=15 | 2.49 | 2.8 | 0 % | 0.46 |
| 64 | 1438 | 44.19 | 61.81 | 0 | 64 | 14326 | 4048 | postgres=2 project=15 | 2.82 | 2.7 | 0 % | 0.51 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2929 | 0.33 | 0.54 | 0 | 1 | 29291 | 394 | postgres=2 project=15 | 1.22 | 0.6 | 0 % | 0.00 |
| 4 | 6541 | 0.54 | 1.68 | 0 | 4 | 65419 | 1 | postgres=2 project=15 | 2.52 | 0.5 | 0 % | 0.00 |
| 8 | 11743 | 0.62 | 1.73 | 0 | 8 | 117623 | 1 | postgres=2 project=15 | 3.82 | 0.4 | 0 % | 0.00 |
| 16 | 12265 | 1.24 | 3.08 | 0 | 15 | 122661 | 0 | postgres=2 project=15 | 3.87 | 0.4 | 0 % | 0.00 |
| 32 | 10907 | 2.71 | 7.70 | 0 | 32 | 109071 | 0 | postgres=2 project=15 | 3.61 | 0.5 | 0 % | 0.00 |
| 64 | 10533 | 5.81 | 14.08 | 0 | 64 | 105321 | 1 | postgres=2 project=15 | 3.64 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 26023 | 0.03 | 0.09 | 0 | 0 | 260035 | 18807 | postgres=2 project=15 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 50601 | 0.07 | 0.17 | 0 | 0 | 504834 | 36464 | postgres=2 project=15 | 0.03 | 0.0 | 0 % | 1.85 |
| 8 | 80885 | 0.08 | 0.28 | 0 | 0 | 807682 | 57821 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.11 |
| 16 | 76012 | 0.17 | 0.83 | 0 | 0 | 757193 | 55365 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.24 |
| 32 | 82794 | 0.34 | 1.31 | 0 | 0 | 821331 | 59943 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.53 |
| 64 | 80905 | 0.57 | 3.80 | 0 | 0 | 799693 | 58114 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.50 |
