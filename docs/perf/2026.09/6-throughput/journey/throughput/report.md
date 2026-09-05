## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **4** | 8 (43.98 → 94.18 ms) | ×0.91 | 162 | 64 | 461.63 ms |
| `nodb` | **4** | 8 (11.68 → 26.03 ms) | ×0.91 | 698 | 32 | 71.83 ms |
| `pg` | **8** | 16 (0.25 → 1.42 ms) | ×0.92 | 79383 | 8 | 0.25 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, authenticated`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 68 | 14.38 | 20.80 | 0 | 1 | 676 | 171 | postgres=2 project=4 | 1.22 | 32.6 | 44 % | 0.02 |
| 4 | 145 | 26.83 | 43.98 | 0 | 4 | 1446 | 510 | postgres=2 project=8 | 1.59 | 15.3 | 68 % | 0.06 |
| 8 | 131 | 59.46 | 94.18 | 0 | 8 | 1306 | 470 | postgres=2 project=10 | 1.61 | 17.0 | 55 % | 0.06 |
| 16 | 152 | 104.15 | 148.94 | 0 | 16 | 1508 | 542 | postgres=2 project=10 | 1.59 | 14.5 | 62 % | 0.07 |
| 32 | 142 | 223.89 | 325.06 | 0 | 32 | 1386 | 524 | postgres=2 project=13 | 1.60 | 15.9 | 52 % | 0.07 |
| 64 | 162 | 402.59 | 461.63 | 0 | 64 | 1555 | 593 | postgres=2 project=13 | 1.64 | 14.5 | 51 % | 0.07 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 239 | 3.97 | 8.11 | 0 | 1 | 2387 | 43 | postgres=2 project=13 | 1.58 | 9.2 | 48 % | 0.00 |
| 4 | 611 | 6.26 | 11.68 | 0 | 4 | 6111 | 0 | postgres=2 project=13 | 1.60 | 3.6 | 65 % | 0.00 |
| 8 | 557 | 14.52 | 26.03 | 0 | 8 | 5560 | 0 | postgres=2 project=13 | 1.57 | 3.9 | 66 % | 0.00 |
| 16 | 663 | 24.21 | 42.89 | 0 | 16 | 6619 | 1 | postgres=2 project=13 | 1.59 | 3.3 | 49 % | 0.00 |
| 32 | 698 | 46.31 | 71.83 | 0 | 32 | 6948 | 1 | postgres=2 project=13 | 1.58 | 3.1 | 64 % | 0.00 |
| 64 | 674 | 99.42 | 151.12 | 0 | 64 | 6673 | 0 | postgres=2 project=13 | 1.58 | 3.3 | 62 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--nvo5um9n as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 22584 | 0.04 | 0.10 | 0 | 0 | 225680 | 10647 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 0.32 |
| 4 | 47682 | 0.07 | 0.19 | 0 | 0 | 476170 | 33452 | postgres=2 project=13 | 0.04 | 0.0 | 0 % | 1.79 |
| 8 | 79383 | 0.08 | 0.25 | 0 | 0 | 792019 | 51727 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 2.86 |
| 16 | 72948 | 0.08 | 1.42 | 0 | 0 | 726980 | 49700 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 2.86 |
| 32 | 69164 | 0.26 | 2.62 | 0 | 0 | 686832 | 45999 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 2.83 |
| 64 | 71323 | 0.43 | 6.21 | 0 | 0 | 704290 | 47506 | postgres=2 project=13 | 0.03 | 0.0 | 0 % | 3.02 |
