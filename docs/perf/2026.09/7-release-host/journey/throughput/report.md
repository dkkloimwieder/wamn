## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.72 → 17.15 ms) | ×1.07 | 1652 | 16 | 17.15 ms |
| `nodb` | **8** | 16 (2.25 → 3.28 ms) | ×1.11 | 12897 | 32 | 5.23 ms |
| `pg` | **8** | 16 (0.31 → 0.72 ms) | ×1.08 | 78839 | 16 | 0.72 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, authenticated`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 263 | 3.39 | 9.78 | 0 | 1 | 2625 | 638 | postgres=2 project=4 | 0.81 | 5.6 | 0 % | 0.09 |
| 4 | 1089 | 3.39 | 7.84 | 0 | 4 | 10889 | 3704 | postgres=2 project=8 | 2.38 | 3.0 | 0 % | 0.38 |
| 8 | 1543 | 4.83 | 11.72 | 0 | 8 | 15429 | 5549 | postgres=2 project=12 | 2.95 | 2.6 | 0 % | 0.52 |
| 16 | 1652 | 9.31 | 17.15 | 0 | 16 | 16511 | 5900 | postgres=2 project=12 | 3.09 | 2.6 | 0 % | 0.56 |
| 32 | 1378 | 22.60 | 40.64 | 0 | 32 | 13754 | 5152 | postgres=2 project=15 | 2.71 | 2.8 | 0 % | 0.46 |
| 64 | 823 | 74.29 | 134.74 | 0 | 64 | 8168 | 2969 | postgres=2 project=16 | 1.93 | 3.2 | 0 % | 0.33 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 1934 | 0.48 | 1.18 | 0 | 1 | 19347 | 356 | postgres=2 project=16 | 1.14 | 0.8 | 0 % | 0.00 |
| 4 | 5300 | 0.59 | 2.78 | 0 | 4 | 53009 | 0 | postgres=2 project=16 | 2.16 | 0.6 | 0 % | 0.00 |
| 8 | 10828 | 0.65 | 2.25 | 0 | 8 | 108305 | 0 | postgres=2 project=16 | 3.59 | 0.5 | 0 % | 0.00 |
| 16 | 12020 | 1.27 | 3.28 | 0 | 16 | 120208 | 0 | postgres=2 project=16 | 3.79 | 0.4 | 0 % | 0.00 |
| 32 | 12897 | 2.43 | 5.23 | 0 | 32 | 128967 | 0 | postgres=2 project=16 | 3.96 | 0.4 | 0 % | 0.00 |
| 64 | 9577 | 5.88 | 19.77 | 0 | 64 | 95808 | 0 | postgres=2 project=16 | 3.13 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--ugx4tbvu as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 18326 | 0.05 | 0.13 | 0 | 0 | 183143 | 8420 | postgres=2 project=16 | 0.02 | 0.0 | 0 % | 0.32 |
| 4 | 44923 | 0.08 | 0.18 | 0 | 0 | 448615 | 32528 | postgres=2 project=16 | 0.03 | 0.0 | 0 % | 1.85 |
| 8 | 73259 | 0.08 | 0.31 | 0 | 0 | 730989 | 53192 | postgres=2 project=16 | 0.02 | 0.0 | 0 % | 3.03 |
| 16 | 78839 | 0.17 | 0.72 | 0 | 0 | 785570 | 56784 | postgres=2 project=16 | 0.02 | 0.0 | 0 % | 3.28 |
| 32 | 77051 | 0.33 | 1.75 | 0 | 0 | 761434 | 54577 | postgres=2 project=16 | 0.02 | 0.0 | 0 % | 3.26 |
| 64 | 61234 | 0.58 | 6.97 | 0 | 0 | 598188 | 43191 | postgres=2 project=16 | 0.02 | 0.0 | 0 % | 2.92 |
