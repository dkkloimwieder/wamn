## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **8** | 16 (11.03 → 16.46 ms) | ×1.08 | 1614 | 16 | 16.46 ms |
| `nodb` | **8** | 16 (2.45 → 3.31 ms) | ×1.17 | 13202 | 64 | 10.34 ms |
| `pg` | **8** | 16 (0.26 → 0.85 ms) | ×1.00 | 78247 | 8 | 0.26 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, human PAT, repetition 2`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 353 | 2.69 | 5.52 | 0 | 1 | 3525 | 903 | postgres=2 project=14 | 1.19 | 4.7 | 0 % | 0.14 |
| 4 | 1103 | 3.39 | 7.12 | 0 | 4 | 11025 | 2993 | postgres=2 project=14 | 2.36 | 2.9 | 0 % | 0.42 |
| 8 | 1493 | 5.02 | 11.03 | 0 | 8 | 14928 | 4234 | postgres=2 project=14 | 2.86 | 2.6 | 0 % | 0.57 |
| 16 | 1614 | 9.59 | 16.46 | 0 | 16 | 16132 | 4718 | postgres=2 project=14 | 3.05 | 2.6 | 0 % | 0.62 |
| 32 | 1365 | 23.16 | 33.43 | 0 | 32 | 13621 | 3944 | postgres=2 project=14 | 2.71 | 2.7 | 0 % | 0.51 |
| 64 | 1184 | 48.50 | 143.95 | 0 | 64 | 11783 | 3529 | postgres=2 project=14 | 2.43 | 2.8 | 0 % | 0.46 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2286 | 0.40 | 1.22 | 0 | 1 | 22859 | 301 | postgres=2 project=14 | 1.14 | 0.7 | 0 % | 0.00 |
| 4 | 7882 | 0.46 | 1.23 | 0 | 4 | 78832 | 1 | postgres=2 project=14 | 2.68 | 0.5 | 0 % | 0.00 |
| 8 | 10574 | 0.65 | 2.45 | 0 | 8 | 105781 | 0 | postgres=2 project=14 | 3.40 | 0.4 | 0 % | 0.00 |
| 16 | 12355 | 1.23 | 3.31 | 0 | 16 | 123574 | 0 | postgres=2 project=14 | 3.74 | 0.4 | 0 % | 0.00 |
| 32 | 13076 | 2.38 | 5.38 | 0 | 32 | 130775 | 1 | postgres=2 project=14 | 3.85 | 0.4 | 0 % | 0.00 |
| 64 | 13202 | 4.72 | 10.34 | 0 | 64 | 132040 | 1 | postgres=2 project=14 | 3.85 | 0.4 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--8z6m8x0j as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 23831 | 0.04 | 0.09 | 0 | 0 | 238180 | 17249 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 0.50 |
| 4 | 53901 | 0.07 | 0.14 | 0 | 0 | 538379 | 38758 | postgres=2 project=14 | 0.03 | 0.0 | 0 % | 1.91 |
| 8 | 78247 | 0.08 | 0.26 | 0 | 0 | 781254 | 56114 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.09 |
| 16 | 78066 | 0.17 | 0.85 | 0 | 0 | 778125 | 56254 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.28 |
| 32 | 74695 | 0.30 | 1.81 | 0 | 0 | 741891 | 53618 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.34 |
| 64 | 72500 | 0.61 | 4.26 | 0 | 0 | 715152 | 51530 | postgres=2 project=14 | 0.02 | 0.0 | 0 % | 3.41 |
