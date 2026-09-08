## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.28 → 26.57 ms) | ×1.00 | 1381 | 8 | 11.28 ms |
| `nodb` | **8** | 16 (2.62 → 3.67 ms) | ×1.11 | 11353 | 64 | 11.86 ms |
| `pg` | **8** | 16 (0.29 → 1.07 ms) | ×1.01 | 73128 | 16 | 1.07 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 358 | 2.61 | 4.93 | 0 | 1 | 3584 | 985 | postgres=2 project=14 | 1.30 | 4.8 | 0 % | 0.13 |
| 4 | 1035 | 3.60 | 7.74 | 0 | 4 | 10345 | 2806 | postgres=2 project=14 | 2.38 | 3.2 | 0 % | 0.37 |
| 8 | 1381 | 5.45 | 11.28 | 0 | 8 | 13802 | 3921 | postgres=2 project=14 | 2.89 | 2.9 | 0 % | 0.51 |
| 16 | 1375 | 10.93 | 26.57 | 0 | 16 | 13735 | 3931 | postgres=2 project=14 | 2.86 | 2.9 | 0 % | 0.51 |
| 32 | 1251 | 25.42 | 38.05 | 0 | 32 | 12479 | 3697 | postgres=2 project=14 | 2.71 | 3.0 | 0 % | 0.45 |
| 64 | 1262 | 51.31 | 72.36 | 0 | 64 | 12563 | 3649 | postgres=2 project=14 | 2.76 | 3.0 | 0 % | 0.46 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2417 | 0.39 | 0.77 | 0 | 1 | 24171 | 346 | postgres=2 project=14 | 1.21 | 0.7 | 0 % | 0.00 |
| 4 | 6001 | 0.56 | 2.28 | 0 | 4 | 60019 | 0 | postgres=2 project=14 | 2.46 | 0.6 | 0 % | 0.00 |
| 8 | 9671 | 0.73 | 2.62 | 0 | 8 | 96725 | 0 | postgres=2 project=14 | 3.59 | 0.5 | 0 % | 0.00 |
| 16 | 10702 | 1.43 | 3.67 | 0 | 16 | 107041 | 1 | postgres=2 project=14 | 3.82 | 0.5 | 0 % | 0.00 |
| 32 | 11230 | 2.76 | 6.22 | 0 | 32 | 112310 | 0 | postgres=2 project=14 | 3.89 | 0.5 | 0 % | 0.00 |
| 64 | 11353 | 5.50 | 11.86 | 0 | 64 | 113515 | 0 | postgres=2 project=14 | 3.91 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 24049 | 0.04 | 0.10 | 0 | 0 | 240298 | 17343 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 47945 | 0.08 | 0.16 | 0 | 0 | 478661 | 34225 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.87 |
| 8 | 72329 | 0.09 | 0.29 | 0 | 0 | 721370 | 52492 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.14 |
| 16 | 73128 | 0.17 | 1.07 | 0 | 0 | 728767 | 52285 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.24 |
| 32 | 71200 | 0.31 | 2.05 | 0 | 0 | 706715 | 51076 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.33 |
| 64 | 68731 | 0.59 | 4.69 | 0 | 0 | 678673 | 48473 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.40 |
