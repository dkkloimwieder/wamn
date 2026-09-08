## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (14.51 → 31.71 ms) | ×1.00 | 1197 | 8 | 14.51 ms |
| `nodb` | **16** | 32 (4.74 → 6.81 ms) | ×1.14 | 13225 | 64 | 11.70 ms |
| `pg` | **8** | 16 (0.32 → 1.27 ms) | ×0.92 | 71535 | 8 | 0.32 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 328 | 2.82 | 7.18 | 0 | 1 | 3283 | 510 | postgres=2 project=4 | 0.70 | 5.0 | 0 % | 0.07 |
| 4 | 798 | 4.41 | 13.13 | 0 | 4 | 7981 | 2147 | postgres=2 project=8 | 1.85 | 3.3 | 0 % | 0.30 |
| 8 | 1197 | 6.16 | 14.51 | 0 | 8 | 11962 | 3238 | postgres=2 project=12 | 2.46 | 2.9 | 0 % | 0.44 |
| 16 | 1192 | 12.38 | 31.71 | 0 | 16 | 11911 | 3509 | postgres=2 project=13 | 2.46 | 2.9 | 0 % | 0.45 |
| 32 | 1130 | 26.64 | 56.99 | 0 | 32 | 11269 | 3351 | postgres=2 project=13 | 2.46 | 2.9 | 0 % | 0.42 |
| 64 | 1155 | 53.20 | 109.76 | 0 | 64 | 11488 | 3355 | postgres=2 project=14 | 2.48 | 3.0 | 0 % | 0.41 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2127 | 0.43 | 1.11 | 0 | 1 | 21268 | 318 | postgres=2 project=14 | 1.16 | 0.8 | 0 % | 0.00 |
| 4 | 5602 | 0.58 | 2.40 | 0 | 4 | 56028 | 1 | postgres=2 project=14 | 2.32 | 0.6 | 0 % | 0.00 |
| 8 | 8109 | 0.80 | 3.79 | 0 | 8 | 81106 | 1 | postgres=2 project=14 | 3.01 | 0.5 | 0 % | 0.00 |
| 16 | 9856 | 1.48 | 4.74 | 0 | 16 | 98558 | 0 | postgres=2 project=14 | 3.48 | 0.5 | 0 % | 0.00 |
| 32 | 11243 | 2.72 | 6.81 | 0 | 32 | 112438 | 0 | postgres=2 project=14 | 3.74 | 0.5 | 0 % | 0.00 |
| 64 | 13225 | 4.65 | 11.70 | 0 | 63 | 132243 | 1 | postgres=2 project=14 | 3.95 | 0.4 | 1 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 25305 | 0.04 | 0.10 | 0 | 0 | 252923 | 12105 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 0.33 |
| 4 | 40585 | 0.09 | 0.25 | 0 | 0 | 405413 | 29195 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.80 |
| 8 | 71535 | 0.09 | 0.32 | 0 | 0 | 713354 | 48902 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.88 |
| 16 | 65479 | 0.18 | 1.27 | 0 | 0 | 651669 | 44367 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 2.83 |
| 32 | 61474 | 0.33 | 2.79 | 0 | 0 | 610489 | 43938 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 2.99 |
| 64 | 66957 | 0.52 | 6.05 | 0 | 0 | 660859 | 45413 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 3.06 |
