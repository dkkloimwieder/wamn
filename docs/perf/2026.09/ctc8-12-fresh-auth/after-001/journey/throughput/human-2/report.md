## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (15.00 → 24.04 ms) | ×1.11 | 1307 | 16 | 24.04 ms |
| `nodb` | **8** | 16 (3.12 → 4.83 ms) | ×1.06 | 9274 | 16 | 4.83 ms |
| `pg` | **8** | 16 (0.27 → 1.17 ms) | ×0.85 | 77991 | 8 | 0.27 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 317 | 2.97 | 5.87 | 0 | 1 | 3170 | 833 | postgres=2 project=14 | 1.21 | 5.3 | 0 % | 0.14 |
| 4 | 950 | 3.87 | 8.99 | 0 | 4 | 9497 | 2647 | postgres=2 project=14 | 2.26 | 3.3 | 0 % | 0.40 |
| 8 | 1182 | 6.24 | 15.00 | 0 | 8 | 11817 | 3304 | postgres=2 project=14 | 2.62 | 3.1 | 0 % | 0.52 |
| 16 | 1307 | 11.67 | 24.04 | 0 | 16 | 13059 | 3875 | postgres=2 project=14 | 2.81 | 3.0 | 0 % | 0.56 |
| 32 | 1154 | 27.05 | 43.77 | 0 | 32 | 11513 | 3264 | postgres=2 project=14 | 2.58 | 3.1 | 0 % | 0.48 |
| 64 | 1120 | 56.00 | 97.79 | 0 | 64 | 11136 | 3316 | postgres=2 project=14 | 2.59 | 3.2 | 0 % | 0.48 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2163 | 0.43 | 0.93 | 0 | 1 | 21632 | 294 | postgres=2 project=14 | 1.19 | 0.8 | 0 % | 0.00 |
| 4 | 5929 | 0.57 | 2.21 | 0 | 4 | 59294 | 0 | postgres=2 project=14 | 2.47 | 0.6 | 0 % | 0.00 |
| 8 | 8756 | 0.78 | 3.12 | 0 | 8 | 87574 | 1 | postgres=2 project=14 | 3.43 | 0.5 | 0 % | 0.00 |
| 16 | 9274 | 1.59 | 4.83 | 0 | 16 | 92755 | 0 | postgres=2 project=14 | 3.53 | 0.5 | 0 % | 0.00 |
| 32 | 8740 | 3.45 | 9.39 | 0 | 31 | 87410 | 0 | postgres=2 project=14 | 3.37 | 0.5 | 0 % | 0.00 |
| 64 | 8354 | 7.29 | 17.95 | 0 | 64 | 83514 | 1 | postgres=2 project=14 | 3.27 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--pbyl0duf as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 25133 | 0.04 | 0.09 | 0 | 0 | 251187 | 18199 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.51 |
| 4 | 44905 | 0.08 | 0.22 | 0 | 0 | 448696 | 32337 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.84 |
| 8 | 77991 | 0.08 | 0.27 | 0 | 0 | 778440 | 56158 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.28 |
| 16 | 66183 | 0.18 | 1.17 | 0 | 0 | 659068 | 44779 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 2.84 |
| 32 | 64694 | 0.32 | 2.69 | 0 | 0 | 642137 | 46399 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 3.10 |
| 64 | 64294 | 0.62 | 6.32 | 0 | 0 | 633224 | 45905 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 3.28 |
