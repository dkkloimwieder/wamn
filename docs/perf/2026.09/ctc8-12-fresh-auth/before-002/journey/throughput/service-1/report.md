## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **16** | 32 (23.00 → 50.54 ms) | ×0.72 | 1411 | 16 | 23.00 ms |
| `nodb` | **8** | 16 (3.09 → 5.30 ms) | ×0.96 | 10407 | 64 | 15.76 ms |
| `pg` | **8** | 16 (0.24 → 1.08 ms) | ×0.90 | 79605 | 8 | 0.24 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 289 | 3.22 | 6.84 | 0 | 1 | 2891 | 696 | postgres=2 project=4 | 0.90 | 5.7 | 0 % | 0.10 |
| 4 | 947 | 3.90 | 8.74 | 0 | 4 | 9464 | 3185 | postgres=2 project=8 | 2.29 | 3.3 | 0 % | 0.37 |
| 8 | 1161 | 6.32 | 15.19 | 0 | 7 | 11604 | 4285 | postgres=2 project=11 | 2.38 | 2.8 | 0 % | 0.43 |
| 16 | 1411 | 10.72 | 23.00 | 0 | 16 | 14092 | 5011 | postgres=2 project=13 | 2.79 | 2.7 | 0 % | 0.51 |
| 32 | 1009 | 31.13 | 50.54 | 0 | 32 | 10060 | 3771 | postgres=2 project=13 | 2.30 | 3.1 | 0 % | 0.40 |
| 64 | 1235 | 51.70 | 80.15 | 0 | 64 | 12298 | 4447 | postgres=2 project=13 | 2.63 | 2.9 | 0 % | 0.45 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2118 | 0.44 | 1.09 | 0 | 1 | 21184 | 409 | postgres=2 project=13 | 1.15 | 0.7 | 0 % | 0.00 |
| 4 | 6251 | 0.54 | 2.01 | 0 | 4 | 62513 | 0 | postgres=2 project=13 | 2.45 | 0.5 | 0 % | 0.00 |
| 8 | 9397 | 0.73 | 3.09 | 0 | 8 | 93978 | 1 | postgres=2 project=13 | 3.38 | 0.5 | 0 % | 0.00 |
| 16 | 9032 | 1.61 | 5.30 | 0 | 16 | 90331 | 1 | postgres=2 project=13 | 3.29 | 0.5 | 0 % | 0.00 |
| 32 | 8644 | 3.43 | 10.26 | 0 | 32 | 86443 | 0 | postgres=2 project=13 | 3.17 | 0.5 | 0 % | 0.00 |
| 64 | 10407 | 5.82 | 15.76 | 0 | 64 | 104077 | 0 | postgres=2 project=13 | 3.43 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 23205 | 0.04 | 0.10 | 0 | 0 | 231860 | 10593 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 0.32 |
| 4 | 45516 | 0.08 | 0.21 | 0 | 0 | 454458 | 32997 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 1.83 |
| 8 | 79605 | 0.08 | 0.24 | 0 | 0 | 794428 | 57093 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 3.21 |
| 16 | 71553 | 0.17 | 1.08 | 0 | 0 | 712934 | 52336 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 3.08 |
| 32 | 59954 | 0.36 | 2.95 | 0 | 0 | 595349 | 43303 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 2.78 |
| 64 | 69857 | 0.55 | 5.63 | 0 | 0 | 686746 | 50137 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 3.24 |
