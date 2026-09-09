## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (13.05 → 19.51 ms) | ×1.08 | 1526 | 16 | 19.51 ms |
| `nodb` | **8** | 16 (2.60 → 3.53 ms) | ×1.17 | 13146 | 64 | 10.39 ms |
| `pg` | **8** | 16 (0.23 → 1.20 ms) | ×0.79 | 81867 | 8 | 0.23 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 247 | 3.45 | 11.25 | 0 | 1 | 2469 | 649 | postgres=2 project=14 | 0.98 | 5.6 | 0 % | 0.11 |
| 4 | 926 | 3.70 | 11.23 | 0 | 4 | 9255 | 2490 | postgres=2 project=14 | 2.11 | 3.1 | 0 % | 0.35 |
| 8 | 1414 | 5.18 | 13.05 | 0 | 8 | 14136 | 3943 | postgres=2 project=14 | 2.74 | 2.7 | 0 % | 0.50 |
| 16 | 1526 | 9.99 | 19.51 | 0 | 16 | 15248 | 4472 | postgres=2 project=14 | 2.95 | 2.7 | 0 % | 0.53 |
| 32 | 1199 | 25.76 | 48.08 | 0 | 32 | 11963 | 3543 | postgres=2 project=14 | 2.50 | 2.9 | 0 % | 0.43 |
| 64 | 939 | 66.64 | 114.13 | 0 | 64 | 9334 | 2767 | postgres=2 project=14 | 2.14 | 3.2 | 0 % | 0.37 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2496 | 0.37 | 0.86 | 0 | 1 | 24964 | 262 | postgres=2 project=14 | 1.18 | 0.7 | 0 % | 0.00 |
| 4 | 6640 | 0.50 | 1.91 | 0 | 4 | 66414 | 1 | postgres=2 project=14 | 2.42 | 0.5 | 0 % | 0.00 |
| 8 | 10482 | 0.65 | 2.60 | 0 | 8 | 104858 | 1 | postgres=2 project=14 | 3.35 | 0.4 | 0 % | 0.00 |
| 16 | 12227 | 1.23 | 3.53 | 0 | 16 | 122288 | 0 | postgres=2 project=14 | 3.69 | 0.4 | 0 % | 0.00 |
| 32 | 12903 | 2.39 | 5.62 | 0 | 32 | 129050 | 0 | postgres=2 project=14 | 3.77 | 0.4 | 0 % | 0.00 |
| 64 | 13146 | 4.73 | 10.39 | 0 | 64 | 131433 | 1 | postgres=2 project=14 | 3.81 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 27242 | 0.03 | 0.08 | 0 | 0 | 272256 | 19657 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 47792 | 0.08 | 0.18 | 0 | 0 | 477431 | 33840 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.81 |
| 8 | 81867 | 0.08 | 0.23 | 0 | 0 | 816991 | 56052 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.13 |
| 16 | 64302 | 0.18 | 1.20 | 0 | 0 | 640678 | 45239 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.74 |
| 32 | 48295 | 0.39 | 3.99 | 0 | 0 | 473050 | 31137 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 2.17 |
| 64 | 72294 | 0.56 | 5.29 | 0 | 0 | 713156 | 51583 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.36 |
