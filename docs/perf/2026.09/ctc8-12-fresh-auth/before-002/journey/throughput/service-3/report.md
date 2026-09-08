## Knee and peak per layer

| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |
|---|---:|---:|---:|---:|---:|---:|
| `route` | **4** | 8 (9.18 → 17.10 ms) | ×1.15 | 1409 | 16 | 26.28 ms |
| `nodb` | **8** | 16 (3.36 → 5.47 ms) | ×1.09 | 11228 | 64 | 12.51 ms |
| `pg` | **8** | 16 (0.33 → 1.03 ms) | ×0.84 | 73116 | 8 | 0.33 ms |

## `route` — oha against `POST /purchase_order/get through flow-http, service PAT, repetition 3`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 315 | 2.91 | 7.05 | 0 | 1 | 3153 | 1093 | postgres=2 project=18 | 1.22 | 5.1 | 0 % | 0.13 |
| 4 | 942 | 3.92 | 9.18 | 0 | 4 | 9421 | 3184 | postgres=2 project=18 | 2.16 | 3.2 | 0 % | 0.36 |
| 8 | 1083 | 6.71 | 17.10 | 0 | 8 | 10829 | 3909 | postgres=2 project=18 | 2.32 | 3.0 | 0 % | 0.42 |
| 16 | 1409 | 10.66 | 26.28 | 0 | 16 | 14080 | 4966 | postgres=2 project=18 | 2.82 | 2.8 | 0 % | 0.52 |
| 32 | 1078 | 27.34 | 66.04 | 0 | 32 | 10755 | 4077 | postgres=2 project=18 | 2.30 | 3.0 | 0 % | 0.42 |
| 64 | 1068 | 56.01 | 125.28 | 0 | 64 | 10620 | 3754 | postgres=2 project=18 | 2.36 | 3.1 | 0 % | 0.40 |

## `nodb` — oha against `GET /no-such-route through flow-http: routed and answered 404 by the guest, no auth, no database`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 2294 | 0.41 | 0.99 | 0 | 1 | 22941 | 425 | postgres=2 project=18 | 1.16 | 0.7 | 0 % | 0.00 |
| 4 | 5838 | 0.56 | 2.43 | 0 | 4 | 58382 | 1 | postgres=2 project=18 | 2.28 | 0.5 | 0 % | 0.00 |
| 8 | 8705 | 0.78 | 3.36 | 0 | 8 | 87062 | 0 | postgres=2 project=18 | 3.16 | 0.5 | 0 % | 0.00 |
| 16 | 9477 | 1.50 | 5.47 | 0 | 16 | 94778 | 0 | postgres=2 project=18 | 3.31 | 0.5 | 0 % | 0.00 |
| 32 | 10188 | 2.96 | 8.15 | 0 | 32 | 101879 | 1 | postgres=2 project=18 | 3.52 | 0.5 | 0 % | 0.00 |
| 64 | 11228 | 5.54 | 12.51 | 0 | 64 | 112244 | 0 | postgres=2 project=18 | 3.76 | 0.5 | 0 % | 0.00 |

## `pg` — pgbench against `pgbench -M prepared, the generated purchase_order/get read against wamn-db-acme--receiving--dev--yf906w0c as postgres`

| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 1 | 26694 | 0.03 | 0.09 | 0 | 0 | 266776 | 19186 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 0.50 |
| 4 | 47830 | 0.08 | 0.17 | 0 | 0 | 477881 | 34453 | postgres=2 project=18 | 0.03 | 0.0 | 0 % | 1.85 |
| 8 | 73116 | 0.08 | 0.33 | 0 | 0 | 729759 | 51177 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 2.88 |
| 16 | 61682 | 0.18 | 1.03 | 0 | 0 | 612814 | 42444 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 2.60 |
| 32 | 61778 | 0.33 | 2.81 | 0 | 0 | 613403 | 41924 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 2.71 |
| 64 | 65555 | 0.53 | 6.98 | 0 | 0 | 646877 | 47042 | postgres=2 project=18 | 0.02 | 0.0 | 0 % | 3.14 |
