## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.70 → 31.29 ms) | ×1.07 | 1536 | 16 | 31.29 ms |
| `nodb` | **8** | 16 (1.80 → 2.87 ms) | ×1.08 | 13177 | 64 | 10.26 ms |
| `pg` | **8** | 16 (0.23 → 0.92 ms) | ×0.88 | 85188 | 8 | 0.23 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 359 | 2.58 | 7.13 | 0 | 1 | 3585 | 946 | postgres=2 project=15 | 1.18 | 4.6 | 0 % | 0.12 |
| 4 | 826 | 3.93 | 16.63 | 0 | 4 | 8263 | 2265 | postgres=2 project=15 | 1.87 | 3.1 | 0 % | 0.30 |
| 8 | 1431 | 5.18 | 11.70 | 0 | 8 | 14309 | 4016 | postgres=2 project=15 | 2.79 | 2.7 | 0 % | 0.49 |
| 16 | 1536 | 9.13 | 31.29 | 0 | 16 | 15351 | 4309 | postgres=2 project=15 | 2.86 | 2.6 | 0 % | 0.51 |
| 32 | 1348 | 23.22 | 39.88 | 0 | 32 | 13452 | 4044 | postgres=2 project=15 | 2.67 | 2.8 | 0 % | 0.44 |
| 64 | 1435 | 44.79 | 69.15 | 0 | 64 | 14289 | 4142 | postgres=2 project=15 | 2.92 | 2.8 | 0 % | 0.48 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2801 | 0.34 | 0.60 | 0 | 1 | 28020 | 385 | postgres=2 project=15 | 1.21 | 0.6 | 0 % | 0.00 |
| 4 | 7809 | 0.48 | 1.13 | 0 | 4 | 78105 | 0 | postgres=2 project=15 | 2.77 | 0.5 | 0 % | 0.00 |
| 8 | 11449 | 0.63 | 1.80 | 0 | 8 | 114521 | 0 | postgres=2 project=15 | 3.75 | 0.5 | 0 % | 0.00 |
| 16 | 12347 | 1.26 | 2.87 | 0 | 16 | 123470 | 1 | postgres=2 project=15 | 3.98 | 0.4 | 0 % | 0.00 |
| 32 | 13149 | 2.37 | 5.06 | 0 | 31 | 131496 | 1 | postgres=2 project=15 | 4.02 | 0.4 | 0 % | 0.00 |
| 64 | 13177 | 4.74 | 10.26 | 0 | 64 | 131769 | 0 | postgres=2 project=15 | 3.97 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--vg29mn11 as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 30718 | 0.03 | 0.07 | 0 | 0 | 307016 | 21976 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 58830 | 0.06 | 0.12 | 0 | 0 | 587540 | 42225 | postgres=2 project=15 | 0.03 | 0.0 | 0 % | 1.92 |
| 8 | 85188 | 0.08 | 0.23 | 0 | 0 | 850355 | 61913 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.32 |
| 16 | 75109 | 0.17 | 0.92 | 0 | 0 | 748494 | 53685 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.08 |
| 32 | 58755 | 0.35 | 2.77 | 0 | 0 | 583126 | 42479 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 2.80 |
| 64 | 67179 | 0.53 | 6.24 | 0 | 0 | 661661 | 48117 | postgres=2 project=15 | 0.02 | 0.0 | 0 % | 3.19 |
