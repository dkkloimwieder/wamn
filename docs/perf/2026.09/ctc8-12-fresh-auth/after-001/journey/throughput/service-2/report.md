## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (12.25 → 20.12 ms) | ×1.06 | 1417 | 16 | 20.12 ms |
| `nodb` | **4** | 8 (1.52 → 3.71 ms) | ×1.18 | 11038 | 64 | 12.63 ms |
| `pg` | **8** | 16 (0.20 → 1.08 ms) | ×0.84 | 82674 | 8 | 0.20 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 315 | 2.95 | 6.97 | 0 | 1 | 3150 | 812 | postgres=2 project=14 | 1.18 | 5.3 | 0 % | 0.12 |
| 4 | 990 | 3.76 | 8.01 | 0 | 4 | 9898 | 2682 | postgres=2 project=14 | 2.34 | 3.3 | 0 % | 0.37 |
| 8 | 1343 | 5.58 | 12.25 | 0 | 8 | 13425 | 3789 | postgres=2 project=14 | 2.86 | 2.9 | 0 % | 0.50 |
| 16 | 1417 | 10.86 | 20.12 | 0 | 16 | 14162 | 4107 | postgres=2 project=14 | 2.98 | 2.9 | 0 % | 0.53 |
| 32 | 1225 | 25.73 | 37.54 | 0 | 32 | 12222 | 3595 | postgres=2 project=14 | 2.69 | 3.0 | 0 % | 0.45 |
| 64 | 1224 | 52.09 | 74.63 | 0 | 64 | 12179 | 3555 | postgres=2 project=14 | 2.71 | 3.1 | 0 % | 0.45 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2418 | 0.39 | 0.77 | 0 | 1 | 24182 | 327 | postgres=2 project=14 | 1.20 | 0.7 | 0 % | 0.00 |
| 4 | 6651 | 0.54 | 1.52 | 0 | 4 | 66527 | 0 | postgres=2 project=14 | 2.65 | 0.6 | 0 % | 0.00 |
| 8 | 7826 | 0.85 | 3.71 | 0 | 8 | 78295 | 1 | postgres=2 project=14 | 3.15 | 0.5 | 0 % | 0.00 |
| 16 | 5399 | 2.52 | 10.37 | 0 | 15 | 53994 | 0 | postgres=2 project=14 | 1.92 | 0.5 | 0 % | 0.00 |
| 32 | 9973 | 3.00 | 8.48 | 0 | 32 | 99727 | 0 | postgres=2 project=14 | 3.56 | 0.5 | 0 % | 0.00 |
| 64 | 11038 | 5.63 | 12.63 | 0 | 64 | 110370 | 0 | postgres=2 project=14 | 3.88 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 23944 | 0.04 | 0.10 | 0 | 0 | 239298 | 17304 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.51 |
| 4 | 52053 | 0.07 | 0.15 | 0 | 0 | 519954 | 37411 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.91 |
| 8 | 82674 | 0.08 | 0.20 | 0 | 0 | 825043 | 59064 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.39 |
| 16 | 69719 | 0.17 | 1.08 | 0 | 0 | 694773 | 50330 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.20 |
| 32 | 69730 | 0.31 | 2.07 | 0 | 0 | 692371 | 50104 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.32 |
| 64 | 69123 | 0.59 | 5.37 | 0 | 0 | 681233 | 48935 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.42 |
