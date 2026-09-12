## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (10.18 → 17.96 ms) | ×1.04 | 1569 | 16 | 17.96 ms |
| `nodb` | **8** | 16 (1.99 → 3.16 ms) | ×1.10 | 13697 | 64 | 9.81 ms |
| `pg` | **8** | 16 (0.24 → 1.16 ms) | ×0.78 | 83753 | 8 | 0.24 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 385 | 2.49 | 4.65 | 0 | 1 | 3846 | 1057 | postgres=2 project=14 | 1.30 | 4.5 | 0 % | 0.14 |
| 4 | 1117 | 3.38 | 6.64 | 0 | 4 | 11169 | 3042 | postgres=2 project=14 | 2.39 | 2.9 | 0 % | 0.39 |
| 8 | 1514 | 4.99 | 10.18 | 0 | 8 | 15132 | 4278 | postgres=2 project=14 | 2.90 | 2.6 | 0 % | 0.52 |
| 16 | 1569 | 9.84 | 17.96 | 0 | 16 | 15676 | 4574 | postgres=2 project=14 | 2.99 | 2.6 | 0 % | 0.55 |
| 32 | 1330 | 23.72 | 35.15 | 0 | 32 | 13277 | 3861 | postgres=2 project=14 | 2.70 | 2.8 | 0 % | 0.46 |
| 64 | 1318 | 48.01 | 65.16 | 0 | 64 | 13116 | 3832 | postgres=2 project=14 | 2.68 | 2.8 | 0 % | 0.46 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2869 | 0.33 | 0.58 | 0 | 1 | 28697 | 369 | postgres=2 project=14 | 1.21 | 0.6 | 0 % | 0.00 |
| 4 | 7965 | 0.46 | 1.22 | 0 | 4 | 79661 | 1 | postgres=2 project=14 | 2.68 | 0.5 | 0 % | 0.00 |
| 8 | 11596 | 0.61 | 1.99 | 0 | 8 | 115976 | 1 | postgres=2 project=14 | 3.59 | 0.4 | 0 % | 0.00 |
| 16 | 12746 | 1.19 | 3.16 | 0 | 16 | 127473 | 0 | postgres=2 project=14 | 3.77 | 0.4 | 0 % | 0.00 |
| 32 | 13264 | 2.35 | 5.26 | 0 | 32 | 132638 | 0 | postgres=2 project=14 | 3.83 | 0.4 | 0 % | 0.00 |
| 64 | 13697 | 4.57 | 9.81 | 0 | 64 | 136970 | 0 | postgres=2 project=14 | 3.92 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 28297 | 0.03 | 0.08 | 0 | 0 | 282801 | 20243 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 54037 | 0.07 | 0.14 | 0 | 0 | 539659 | 39280 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.93 |
| 8 | 83753 | 0.08 | 0.24 | 0 | 0 | 836109 | 60123 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.36 |
| 16 | 65713 | 0.18 | 1.16 | 0 | 0 | 655080 | 46871 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.88 |
| 32 | 62935 | 0.32 | 2.67 | 0 | 0 | 624917 | 42810 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.81 |
| 64 | 65490 | 0.56 | 6.23 | 0 | 0 | 641864 | 46283 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.13 |
