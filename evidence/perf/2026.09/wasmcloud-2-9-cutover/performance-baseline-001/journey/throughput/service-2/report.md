## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.63 → 20.43 ms) | ×1.09 | 1500 | 16 | 20.43 ms |
| `nodb` | **8** | 16 (2.97 → 3.80 ms) | ×1.18 | 12376 | 64 | 10.73 ms |
| `pg` | **8** | 16 (0.22 → 0.77 ms) | ×0.90 | 84779 | 8 | 0.22 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 382 | 2.43 | 6.00 | 0 | 1 | 3815 | 970 | postgres=2 project=15 | 1.19 | 4.4 | 0 % | 0.12 |
| 4 | 1015 | 3.49 | 11.44 | 0 | 4 | 10150 | 2791 | postgres=2 project=15 | 2.21 | 3.0 | 0 % | 0.35 |
| 8 | 1382 | 5.41 | 11.63 | 0 | 8 | 13810 | 3866 | postgres=2 project=15 | 2.74 | 2.7 | 0 % | 0.48 |
| 16 | 1500 | 10.08 | 20.43 | 0 | 16 | 14989 | 4361 | postgres=2 project=15 | 2.94 | 2.7 | 0 % | 0.52 |
| 32 | 1205 | 25.40 | 50.15 | 0 | 32 | 12021 | 3550 | postgres=2 project=15 | 2.53 | 2.9 | 0 % | 0.42 |
| 64 | 1363 | 47.16 | 86.02 | 0 | 64 | 13569 | 4014 | postgres=2 project=15 | 2.71 | 2.8 | 0 % | 0.46 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2281 | 0.38 | 1.45 | 0 | 1 | 22814 | 289 | postgres=2 project=15 | 1.12 | 0.7 | 0 % | 0.00 |
| 4 | 6148 | 0.54 | 2.24 | 0 | 4 | 61492 | 0 | postgres=2 project=15 | 2.37 | 0.5 | 0 % | 0.00 |
| 8 | 9422 | 0.73 | 2.97 | 0 | 8 | 94222 | 1 | postgres=2 project=15 | 3.34 | 0.5 | 0 % | 0.00 |
| 16 | 11088 | 1.36 | 3.80 | 0 | 16 | 110896 | 1 | postgres=2 project=15 | 3.71 | 0.5 | 0 % | 0.00 |
| 32 | 12070 | 2.52 | 7.28 | 0 | 32 | 120700 | 0 | postgres=2 project=15 | 3.80 | 0.4 | 0 % | 0.00 |
| 64 | 12376 | 5.05 | 10.73 | 0 | 64 | 123728 | 0 | postgres=2 project=15 | 3.92 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 27652 | 0.03 | 0.08 | 0 | 0 | 276345 | 19923 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 50486 | 0.07 | 0.17 | 0 | 0 | 504233 | 36287 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 1.84 |
| 8 | 84779 | 0.08 | 0.22 | 0 | 0 | 846391 | 60029 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.22 |
| 16 | 76674 | 0.17 | 0.77 | 0 | 0 | 764491 | 55887 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.21 |
| 32 | 73651 | 0.32 | 1.97 | 0 | 0 | 731370 | 52209 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.16 |
| 64 | 71117 | 0.54 | 5.54 | 0 | 0 | 701541 | 49683 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.18 |
