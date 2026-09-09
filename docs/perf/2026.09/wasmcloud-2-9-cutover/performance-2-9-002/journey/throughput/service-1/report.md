## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **4** | 8 (7.52 → 13.81 ms) | ×1.19 | 1369 | 16 | 25.76 ms |
| `nodb` | **16** | 32 (4.16 → 7.85 ms) | ×1.00 | 12094 | 64 | 12.68 ms |
| `pg` | **8** | 16 (0.21 → 0.84 ms) | ×0.90 | 85909 | 8 | 0.21 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 1`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 333 | 2.84 | 6.08 | 0 | 1 | 3328 | 662 | postgres=2 project=4 | 0.91 | 5.0 | 0 % | 0.10 |
| 4 | 1090 | 3.41 | 7.52 | 0 | 4 | 10904 | 2977 | postgres=2 project=8 | 2.37 | 3.0 | 0 % | 0.38 |
| 8 | 1302 | 5.61 | 13.81 | 0 | 8 | 13010 | 3770 | postgres=2 project=11 | 2.64 | 2.8 | 0 % | 0.48 |
| 16 | 1369 | 10.79 | 25.76 | 0 | 16 | 13678 | 3902 | postgres=2 project=13 | 2.72 | 2.7 | 0 % | 0.50 |
| 32 | 1169 | 25.24 | 66.32 | 0 | 32 | 11664 | 3493 | postgres=2 project=13 | 2.46 | 2.9 | 0 % | 0.43 |
| 64 | 1167 | 52.73 | 92.82 | 0 | 64 | 11609 | 3543 | postgres=2 project=14 | 2.50 | 3.0 | 0 % | 0.44 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2116 | 0.43 | 1.29 | 0 | 1 | 21161 | 226 | postgres=2 project=14 | 1.13 | 0.7 | 0 % | 0.00 |
| 4 | 6586 | 0.50 | 2.20 | 0 | 4 | 65872 | 0 | postgres=2 project=14 | 2.36 | 0.5 | 0 % | 0.00 |
| 8 | 8899 | 0.73 | 3.41 | 0 | 8 | 89017 | 0 | postgres=2 project=14 | 2.99 | 0.5 | 0 % | 0.00 |
| 16 | 11265 | 1.30 | 4.16 | 0 | 16 | 112725 | 1 | postgres=2 project=14 | 3.54 | 0.4 | 0 % | 0.00 |
| 32 | 11277 | 2.65 | 7.85 | 0 | 32 | 112765 | 1 | postgres=2 project=14 | 3.47 | 0.4 | 0 % | 0.00 |
| 64 | 12094 | 5.07 | 12.68 | 0 | 64 | 120939 | 0 | postgres=2 project=14 | 3.72 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 26318 | 0.03 | 0.09 | 0 | 0 | 263000 | 12031 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 0.32 |
| 4 | 55758 | 0.07 | 0.13 | 0 | 0 | 556814 | 40236 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.93 |
| 8 | 85909 | 0.08 | 0.21 | 0 | 0 | 857579 | 61894 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.43 |
| 16 | 77421 | 0.17 | 0.84 | 0 | 0 | 771517 | 55950 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.27 |
| 32 | 77313 | 0.31 | 1.66 | 0 | 0 | 768309 | 55866 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.40 |
| 64 | 76557 | 0.54 | 4.52 | 0 | 0 | 756046 | 54903 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.49 |
