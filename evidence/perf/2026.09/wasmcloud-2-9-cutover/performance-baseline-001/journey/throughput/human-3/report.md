## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (12.66 → 19.10 ms) | ×1.19 | 1576 | 16 | 19.10 ms |
| `nodb` | **16** | 32 (2.79 → 5.09 ms) | ×1.04 | 13242 | 64 | 10.34 ms |
| `pg` | **8** | 16 (0.20 → 0.56 ms) | ×1.00 | 89804 | 8 | 0.20 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 377 | 2.54 | 4.77 | 0 | 1 | 3767 | 1030 | postgres=2 project=15 | 1.27 | 4.5 | 0 % | 0.14 |
| 4 | 882 | 4.03 | 11.23 | 0 | 4 | 8819 | 2445 | postgres=2 project=15 | 2.02 | 3.2 | 0 % | 0.36 |
| 8 | 1328 | 5.59 | 12.66 | 0 | 8 | 13280 | 3770 | postgres=2 project=15 | 2.67 | 2.8 | 0 % | 0.52 |
| 16 | 1576 | 9.68 | 19.10 | 0 | 16 | 15753 | 4402 | postgres=2 project=15 | 3.01 | 2.6 | 0 % | 0.59 |
| 32 | 1422 | 22.48 | 35.68 | 0 | 32 | 14194 | 4175 | postgres=2 project=15 | 2.78 | 2.7 | 0 % | 0.52 |
| 64 | 1252 | 48.26 | 106.32 | 0 | 64 | 12460 | 3628 | postgres=2 project=15 | 2.53 | 2.8 | 0 % | 0.47 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2752 | 0.35 | 0.64 | 0 | 1 | 27523 | 390 | postgres=2 project=15 | 1.20 | 0.6 | 0 % | 0.00 |
| 4 | 6844 | 0.52 | 1.55 | 0 | 4 | 68454 | 1 | postgres=2 project=15 | 2.58 | 0.5 | 0 % | 0.00 |
| 8 | 9110 | 0.77 | 2.88 | 0 | 8 | 91108 | 0 | postgres=2 project=15 | 3.36 | 0.5 | 0 % | 0.00 |
| 16 | 12603 | 1.23 | 2.79 | 0 | 16 | 126042 | 0 | postgres=2 project=15 | 3.93 | 0.4 | 0 % | 0.00 |
| 32 | 13082 | 2.40 | 5.09 | 0 | 32 | 130816 | 0 | postgres=2 project=15 | 3.98 | 0.4 | 0 % | 0.00 |
| 64 | 13242 | 4.69 | 10.34 | 0 | 64 | 132449 | 0 | postgres=2 project=15 | 3.97 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 30139 | 0.03 | 0.07 | 0 | 0 | 301229 | 21675 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 58005 | 0.07 | 0.12 | 0 | 0 | 579182 | 41723 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 1.92 |
| 8 | 89804 | 0.08 | 0.20 | 0 | 0 | 896483 | 64119 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.42 |
| 16 | 89439 | 0.15 | 0.56 | 0 | 0 | 891004 | 63819 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.41 |
| 32 | 74247 | 0.34 | 1.66 | 0 | 0 | 736990 | 53229 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.18 |
| 64 | 80267 | 0.58 | 3.90 | 0 | 0 | 792913 | 57083 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.48 |
