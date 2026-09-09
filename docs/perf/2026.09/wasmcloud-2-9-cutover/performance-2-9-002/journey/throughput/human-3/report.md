## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **4** | 8 (10.84 → 20.45 ms) | ×1.08 | 1507 | 16 | 20.22 ms |
| `nodb` | **4** | 8 (1.24 → 3.79 ms) | ×1.06 | 12034 | 16 | 3.67 ms |
| `pg` | **8** | 16 (0.26 → 1.12 ms) | ×0.89 | 80498 | 8 | 0.26 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 367 | 2.59 | 5.41 | 0 | 1 | 3672 | 1008 | postgres=2 project=14 | 1.27 | 4.6 | 0 % | 0.15 |
| 4 | 930 | 3.86 | 10.84 | 0 | 4 | 9302 | 2562 | postgres=2 project=14 | 2.09 | 3.1 | 0 % | 0.38 |
| 8 | 1003 | 7.25 | 20.45 | 0 | 8 | 10021 | 2878 | postgres=2 project=14 | 2.19 | 3.0 | 0 % | 0.45 |
| 16 | 1507 | 10.14 | 20.22 | 0 | 16 | 15061 | 4159 | postgres=2 project=14 | 2.89 | 2.7 | 0 % | 0.59 |
| 32 | 1285 | 23.95 | 46.70 | 0 | 32 | 12825 | 3785 | postgres=2 project=14 | 2.60 | 2.8 | 0 % | 0.49 |
| 64 | 1080 | 56.23 | 105.76 | 0 | 64 | 10743 | 3238 | postgres=2 project=14 | 2.36 | 3.0 | 0 % | 0.45 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2719 | 0.35 | 0.64 | 0 | 1 | 27192 | 257 | postgres=2 project=14 | 1.20 | 0.6 | 0 % | 0.00 |
| 4 | 7883 | 0.46 | 1.24 | 0 | 4 | 78846 | 0 | postgres=2 project=14 | 2.67 | 0.5 | 0 % | 0.00 |
| 8 | 8332 | 0.78 | 3.79 | 0 | 7 | 83350 | 1 | postgres=2 project=14 | 2.88 | 0.5 | 0 % | 0.00 |
| 16 | 12034 | 1.23 | 3.67 | 0 | 16 | 120362 | 1 | postgres=2 project=14 | 3.64 | 0.4 | 0 % | 0.00 |
| 32 | 10617 | 2.77 | 8.40 | 0 | 32 | 106188 | 0 | postgres=2 project=14 | 3.33 | 0.4 | 0 % | 0.00 |
| 64 | 11480 | 5.31 | 13.64 | 0 | 64 | 114782 | 1 | postgres=2 project=14 | 3.56 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 26475 | 0.03 | 0.08 | 0 | 0 | 264523 | 19148 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 52738 | 0.07 | 0.15 | 0 | 0 | 526832 | 37963 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.90 |
| 8 | 80498 | 0.08 | 0.26 | 0 | 0 | 803520 | 56743 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.21 |
| 16 | 71884 | 0.17 | 1.12 | 0 | 0 | 716269 | 48885 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 2.93 |
| 32 | 70400 | 0.30 | 2.27 | 0 | 0 | 698843 | 50238 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.15 |
| 64 | 65467 | 0.56 | 6.16 | 0 | 0 | 644585 | 46299 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.23 |
