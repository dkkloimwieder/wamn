## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (20.75 → 35.34 ms) | ×1.04 | 1325 | 32 | 57.81 ms |
| `nodb` | **8** | 16 (4.25 → 6.03 ms) | ×1.11 | 10587 | 64 | 16.42 ms |
| `pg` | **8** | 16 (0.30 → 0.92 ms) | ×0.90 | 72025 | 8 | 0.30 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, authenticated`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 348 | 2.62 | 7.65 | 0 | 1 | 3475 | 891 | postgres=2 project=4 | 0.93 | 4.8 | 0 % | 0.10 |
| 4 | 647 | 5.43 | 16.21 | 0 | 4 | 6469 | 2144 | postgres=2 project=8 | 1.63 | 3.4 | 0 % | 0.28 |
| 8 | 984 | 7.22 | 20.75 | 0 | 8 | 9833 | 3546 | postgres=2 project=11 | 2.15 | 3.0 | 0 % | 0.39 |
| 16 | 1027 | 14.13 | 35.34 | 0 | 16 | 10258 | 3921 | postgres=2 project=13 | 2.19 | 2.9 | 0 % | 0.40 |
| 32 | 1325 | 23.03 | 57.81 | 0 | 32 | 13220 | 4745 | postgres=2 project=13 | 2.63 | 2.8 | 0 % | 0.45 |
| 64 | 1270 | 47.74 | 149.53 | 0 | 64 | 12646 | 4411 | postgres=2 project=13 | 2.60 | 2.8 | 0 % | 0.44 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2689 | 0.36 | 0.62 | 0 | 1 | 26893 | 459 | postgres=2 project=13 | 1.18 | 0.6 | 0 % | 0.00 |
| 4 | 6044 | 0.54 | 2.30 | 0 | 4 | 60449 | 0 | postgres=2 project=13 | 2.29 | 0.5 | 0 % | 0.00 |
| 8 | 7322 | 0.91 | 4.25 | 0 | 8 | 73233 | 0 | postgres=2 project=13 | 2.79 | 0.5 | 0 % | 0.00 |
| 16 | 8126 | 1.77 | 6.03 | 0 | 16 | 81270 | 0 | postgres=2 project=13 | 3.02 | 0.5 | 0 % | 0.00 |
| 32 | 9291 | 3.10 | 9.83 | 0 | 32 | 92915 | 0 | postgres=2 project=13 | 3.22 | 0.5 | 0 % | 0.00 |
| 64 | 10587 | 5.62 | 16.42 | 0 | 64 | 105852 | 0 | postgres=2 project=13 | 3.37 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--6ops3w4x as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 27898 | 0.03 | 0.07 | 0 | 0 | 278800 | 13491 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 0.34 |
| 4 | 52385 | 0.07 | 0.16 | 0 | 0 | 523315 | 37147 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 1.85 |
| 8 | 72025 | 0.08 | 0.30 | 0 | 0 | 718292 | 51804 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 2.89 |
| 16 | 64952 | 0.17 | 0.92 | 0 | 0 | 645048 | 47100 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 2.76 |
| 32 | 53124 | 0.38 | 3.23 | 0 | 0 | 526935 | 38381 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 2.47 |
| 64 | 67099 | 0.53 | 5.41 | 0 | 0 | 656340 | 47671 | postgres=2 project=13 | 0.02 | 0.0 | 0 % | 3.07 |
